using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Dispatching;
using RecordThis.Models;
using RecordThis.Services.Audio;
using RecordThis.Services.Audio.Pipeline;
using RecordThis.Services.Storage;
using RecordThis.Services.UI;

namespace RecordThis.ViewModels;

/// <summary>Waveform preview with trimming, playback, WAV/MP3 export, and copy-to-clipboard.</summary>
public sealed partial class ReviewViewModel : ObservableObject, IDisposable
{
    private const int WaveformBuckets = 480;
    private const int FormatWav = 0;
    private const int FormatMp3 = 1;
    private static readonly TimeSpan PositionInterval = TimeSpan.FromMilliseconds(50);
    private static readonly TimeSpan MinimumSelectionLength = TimeSpan.FromMilliseconds(200);
    private static readonly TimeSpan SilenceLeadIn = TimeSpan.FromMilliseconds(150);
    private static readonly TimeSpan SilenceTail = TimeSpan.FromMilliseconds(300);
    private static readonly TimeSpan CopyNoticeLifetime = TimeSpan.FromSeconds(5);

    private readonly AudioExportService _export;
    private readonly SpoolFileManager _spools;
    private readonly IFilePickerService _picker;
    private readonly IClipboardService _clipboard;
    private readonly SettingsService _settings;
    private readonly ReviewPlaybackService _playback = new();
    private readonly DispatcherQueue _dispatcher;
    private readonly DispatcherQueueTimer _positionTimer;
    private readonly DispatcherQueueTimer _noticeTimer;

    private SpoolSession? _session;
    private CaptureResult? _result;
    private IDisposable? _hold;
    private CancellationTokenSource? _analysisCts;
    private Task<bool>? _analysisTask;
    private WaveformData? _analysis;
    private float _mixedPeak;
    private bool _disposed;

    public ReviewViewModel(
        AudioExportService export,
        SpoolFileManager spools,
        IFilePickerService picker,
        IClipboardService clipboard,
        SettingsService settings)
    {
        _export = export;
        _spools = spools;
        _picker = picker;
        _clipboard = clipboard;
        _settings = settings;
        _dispatcher = DispatcherQueue.GetForCurrentThread()
                      ?? throw new InvalidOperationException("ReviewViewModel must be created on the UI thread.");

        _positionTimer = _dispatcher.CreateTimer();
        _positionTimer.Interval = PositionInterval;
        _positionTimer.Tick += (_, _) => UpdatePosition();

        _noticeTimer = _dispatcher.CreateTimer();
        _noticeTimer.IsRepeating = false;
        _noticeTimer.Tick += (_, _) => Notice = null;

        _playback.PlaybackStopped += OnPlaybackStopped;

        // Restore remembered export choices (fields set directly so nothing is re-saved during startup).
        var saved = settings.Current;
        _selectedFormat = saved.ExportFormat == "mp3" ? FormatChoices[FormatMp3] : FormatChoices[FormatWav];
        _selectedSampleRate = SampleRateChoices[1];
        _selectedBitDepth = BitDepthChoices.FirstOrDefault(c => c.Value == saved.BitDepth) ?? BitDepthChoices[1];
        _selectedBitRate = BitRateChoices.FirstOrDefault(c => c.Value == saved.Mp3BitRate) ?? BitRateChoices[1];
        _preferCopy = saved.PreferCopy;
    }

    /// <summary>Raised when the review screen closes (discarded, or "New recording" after saving).</summary>
    public event EventHandler? Closed;

    /// <summary>Raised when an unsaved recording is discarded. The spool is still on disk so it can be undone.</summary>
    public event EventHandler<CaptureResult>? Discarded;

    // ---------------------------------------------------------------- Export choices

    public IReadOnlyList<ExportChoice> FormatChoices { get; } =
    [
        new("WAV (lossless)", FormatWav),
        new("MP3 (smaller)", FormatMp3),
    ];

    public IReadOnlyList<ExportChoice> SampleRateChoices { get; } =
    [
        new("44.1 kHz", 44100),
        new("48 kHz", 48000),
    ];

    public IReadOnlyList<ExportChoice> BitDepthChoices { get; } =
    [
        new("16-bit (dithered)", 16),
        new("24-bit", 24),
    ];

    public IReadOnlyList<ExportChoice> BitRateChoices { get; } =
    [
        new("128 kbps", 128),
        new("192 kbps", 192),
        new("320 kbps", 320),
    ];

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsWav), nameof(IsMp3), nameof(SaveButtonText))]
    private ExportChoice _selectedFormat;

    [ObservableProperty]
    private ExportChoice _selectedSampleRate;

    [ObservableProperty]
    private ExportChoice _selectedBitDepth;

    [ObservableProperty]
    private ExportChoice _selectedBitRate;

    /// <summary>True when the last share action was Copy: Copy becomes the highlighted (accent) button.</summary>
    [ObservableProperty]
    private bool _preferCopy;

    public bool IsWav => SelectedFormat.Value == FormatWav;

    public bool IsMp3 => SelectedFormat.Value == FormatMp3;

    public string SaveButtonText => IsMp3 ? "Save As (.mp3)" : "Save As (.wav)";

    partial void OnSelectedFormatChanged(ExportChoice value)
    {
        _settings.Current.ExportFormat = value.Value == FormatMp3 ? "mp3" : "wav";
        _settings.Save();
    }

    partial void OnSelectedBitDepthChanged(ExportChoice value)
    {
        _settings.Current.BitDepth = value.Value;
        _settings.Save();
    }

    partial void OnSelectedBitRateChanged(ExportChoice value)
    {
        _settings.Current.Mp3BitRate = value.Value;
        _settings.Save();
    }

    partial void OnPreferCopyChanged(bool value)
    {
        _settings.Current.PreferCopy = value;
        _settings.Save();
    }

    // ---------------------------------------------------------------- Display state

    [ObservableProperty]
    private string _headerText = string.Empty;

    [ObservableProperty]
    private string _detailsText = string.Empty;

    [ObservableProperty]
    private WaveformData? _waveform;

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(TrimSilenceCommand))]
    private bool _isAnalyzing;

    /// <summary>Playback position, 0..1 of the whole recording.</summary>
    [ObservableProperty]
    private double _progress;

    [ObservableProperty]
    private string _positionText = TimeFormat.Clock(TimeSpan.Zero);

    [ObservableProperty]
    private string _durationText = TimeFormat.Clock(TimeSpan.Zero);

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PlayPauseText), nameof(PlayPauseGlyph))]
    private bool _isPlaying;

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(SaveCommand), nameof(CopyCommand), nameof(DiscardCommand))]
    private bool _isExporting;

    /// <summary>Export progress, 0..100.</summary>
    [ObservableProperty]
    private double _exportProgress;

    [ObservableProperty]
    private string? _savedPath;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsSaved))]
    private bool _hasUnsavedRecording;

    public bool IsSaved => !HasUnsavedRecording;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasNotice))]
    private Notice? _notice;

    public bool HasNotice => Notice is not null;

    public string PlayPauseText => IsPlaying ? "Pause" : "Play";

    /// <summary>Pause (E769) while playing, Play (E768) otherwise.</summary>
    public string PlayPauseGlyph => IsPlaying ? "\uE769" : "\uE768";

    partial void OnNoticeChanged(Notice? value)
    {
        // Any new notice (or none) cancels a pending auto-dismiss; short-lived ones start a fresh countdown.
        _noticeTimer.Stop();
        if (value?.Lifetime is { } lifetime)
        {
            _noticeTimer.Interval = lifetime;
            _noticeTimer.Start();
        }
    }

    // ---------------------------------------------------------------- Trimming

    /// <summary>Trim-in point as a fraction of the recording (0..1).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsTrimmed), nameof(TrimText))]
    [NotifyCanExecuteChangedFor(nameof(ResetTrimCommand), nameof(TrimSilenceCommand))]
    private double _trimStart;

    /// <summary>Trim-out point as a fraction of the recording (0..1).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsTrimmed), nameof(TrimText))]
    [NotifyCanExecuteChangedFor(nameof(ResetTrimCommand), nameof(TrimSilenceCommand))]
    private double _trimEnd = 1;

    /// <summary>Smallest selection the handles allow, as a fraction of the recording.</summary>
    [ObservableProperty]
    private double _minimumSelection = 0.01;

    public bool IsTrimmed => TrimStart > 0.0005 || TrimEnd < 0.9995;

    public string TrimText
    {
        get
        {
            if (_session is null)
            {
                return string.Empty;
            }

            if (!IsTrimmed)
            {
                return "Drag the edges of the waveform to trim the start or end.";
            }

            TimeSpan total = _session.Duration;
            return $"Keeping {TimeFormat.Precise(total * (TrimEnd - TrimStart))} of {TimeFormat.Precise(total)}";
        }
    }

    partial void OnTrimStartChanged(double value) => ApplyTrimToPlayback();

    partial void OnTrimEndChanged(double value) => ApplyTrimToPlayback();

    private (long Start, long End) TrimFrames()
    {
        long total = _playback.TotalFrames > 0 ? _playback.TotalFrames : _session?.FrameCount ?? 0;
        long start = (long)Math.Floor(TrimStart * total);
        long end = (long)Math.Ceiling(TrimEnd * total);
        return (Math.Clamp(start, 0, total), Math.Clamp(end, 0, total));
    }

    private void ApplyTrimToPlayback()
    {
        var (start, end) = TrimFrames();
        _playback.SetRange(start, end);
    }

    private bool CanTrimSilence()
    {
        if (_analysis is not { HasSound: true } data || data.TotalFrames == 0 || IsAnalyzing)
        {
            return false;
        }

        var (start, end) = SilenceBounds(data);
        return Math.Abs(start - TrimStart) > 0.0005 || Math.Abs(end - TrimEnd) > 0.0005;
    }

    /// <summary>Moves the handles to where sound actually starts and ends, with a little breathing room.</summary>
    [RelayCommand(CanExecute = nameof(CanTrimSilence))]
    private void TrimSilence()
    {
        if (_analysis is not { HasSound: true } data)
        {
            return;
        }

        var (start, end) = SilenceBounds(data);
        TrimStart = start;
        TrimEnd = end;
    }

    private (double Start, double End) SilenceBounds(WaveformData data)
    {
        double rate = _session?.Format.SampleRate ?? 48000;
        double total = data.TotalFrames;
        double start = Math.Max(0, data.FirstSoundFrame - SilenceLeadIn.TotalSeconds * rate) / total;
        double end = Math.Min(total, data.LastSoundFrame + SilenceTail.TotalSeconds * rate) / total;
        return (start, Math.Max(end, Math.Min(1, start + MinimumSelection)));
    }

    [RelayCommand(CanExecute = nameof(IsTrimmed))]
    private void ResetTrim()
    {
        TrimStart = 0;
        TrimEnd = 1;
    }

    // ---------------------------------------------------------------- Session lifecycle

    public void Load(CaptureResult result)
    {
        if (_session is not null)
        {
            Release(deleteSpool: !HasUnsavedRecording);
        }

        var session = result.Session;
        _session = session;
        _result = result;
        _hold = _spools.Hold(session);
        _mixedPeak = 0f;
        _analysis = null;

        TimeSpan duration = session.Duration;
        HeaderText = $"Playback preview ({TimeFormat.Clock(duration)} captured)";
        DetailsText = BuildDetails(result);
        DurationText = TimeFormat.Clock(duration);
        PositionText = TimeFormat.Clock(TimeSpan.Zero);
        Progress = 0;
        Waveform = null;
        IsPlaying = false;
        ExportProgress = 0;
        SavedPath = null;
        HasUnsavedRecording = true;
        Notice = NoticeFor(result);

        // Default to the capture's rate family so the common case needs no resampling.
        SelectedSampleRate = session.Format.SampleRate % 11025 == 0 ? SampleRateChoices[0] : SampleRateChoices[1];

        try
        {
            _playback.Load(session, 1f);
        }
        catch (Exception ex)
        {
            Notice = new Notice(
                "Preview isn't available",
                $"{AudioErrorTranslator.Describe(ex)} You can still save the recording.",
                NoticeSeverity.Warning);
        }

        MinimumSelection = duration > TimeSpan.Zero
            ? Math.Min(1, MinimumSelectionLength.TotalSeconds / duration.TotalSeconds)
            : 1;
        TrimStart = 0;
        TrimEnd = 1;
        ApplyTrimToPlayback();
        OnPropertyChanged(nameof(TrimText));

        _analysisCts = new CancellationTokenSource();
        _analysisTask = AnalyzeAsync(session, _analysisCts.Token);
        NotifyCommands();
    }

    /// <summary>Deletes the spool and returns to the capture screen without asking.</summary>
    public void CloseSession()
    {
        Release(deleteSpool: true);
        HasUnsavedRecording = false;
        SavedPath = null;
        Notice = null;
        Closed?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>Used by the global shortcut when there's still an unsaved recording on screen.</summary>
    public void ShowFinishFirstNotice(string gesture) =>
        Notice = new Notice(
            "Finish with this recording first",
            $"Copy, save or discard it, then press {gesture} to record again.",
            NoticeSeverity.Informational)
        {
            Lifetime = CopyNoticeLifetime,
        };

    private void Release(bool deleteSpool)
    {
        _analysisCts?.Cancel();
        _analysisCts?.Dispose();
        _analysisCts = null;
        _analysisTask = null;
        _analysis = null;

        _positionTimer.Stop();
        _playback.Unload();
        IsPlaying = false;

        _hold?.Dispose();
        _hold = null;

        if (deleteSpool && _session is not null)
        {
            _spools.Delete(_session);
        }

        _session = null;
        _result = null;
        Waveform = null;
        IsAnalyzing = false;
        NotifyCommands();
    }

    private async Task<bool> AnalyzeAsync(SpoolSession session, CancellationToken token)
    {
        IsAnalyzing = true;
        try
        {
            WaveformData data = await WaveformAnalyzer.AnalyzeAsync(session, WaveformBuckets, token);
            if (token.IsCancellationRequested || !ReferenceEquals(_session, session))
            {
                return false;
            }

            _analysis = data;
            _mixedPeak = data.MixedPeak;
            float gain = AudioExportService.ClipSafeGain(data.MixedPeak);
            _playback.SetGain(gain);

            // Show exactly what will be exported (post-downmix, post-gain).
            var peaks = new float[data.Peaks.Length];
            for (int i = 0; i < peaks.Length; i++)
            {
                peaks[i] = Math.Min(1f, data.Peaks[i] * gain);
            }

            Waveform = data with { Peaks = peaks, MixedPeak = data.MixedPeak * gain };

            if (gain < 1f)
            {
                DetailsText += $" · Lowered {-20 * Math.Log10(gain):0.0} dB so the stereo downmix doesn't clip";
            }

            return true;
        }
        catch (OperationCanceledException)
        {
            return false;
        }
        catch (Exception ex)
        {
            if (ReferenceEquals(_session, session))
            {
                Notice ??= new Notice(
                    "Couldn't draw the waveform",
                    $"{AudioErrorTranslator.Describe(ex)} Playback and saving may still work.",
                    NoticeSeverity.Warning);
            }

            return false;
        }
        finally
        {
            if (ReferenceEquals(_session, session))
            {
                IsAnalyzing = false;
                TrimSilenceCommand.NotifyCanExecuteChanged();
            }
        }
    }

    // ---------------------------------------------------------------- Playback

    private bool CanPlayPause() => _session is not null && _playback.IsLoaded;

    [RelayCommand(CanExecute = nameof(CanPlayPause))]
    private void PlayPause()
    {
        if (IsPlaying)
        {
            Pause();
        }
        else
        {
            Play();
        }
    }

    private void Play()
    {
        try
        {
            _playback.Play();
            IsPlaying = true;
            _positionTimer.Start();
        }
        catch (Exception ex)
        {
            IsPlaying = false;
            Notice = new Notice(
                "Playback isn't available",
                $"{AudioErrorTranslator.Describe(ex)} Check that speakers or headphones are connected. Saving still works.",
                NoticeSeverity.Warning);
        }
    }

    private void Pause()
    {
        _playback.Pause();
        IsPlaying = false;
        _positionTimer.Stop();
        UpdatePosition();
    }

    [RelayCommand]
    private void Seek(double fraction)
    {
        if (_session is null)
        {
            return;
        }

        _playback.Seek(fraction);
        UpdatePosition();
    }

    private void OnPlaybackStopped(object? sender, Exception? error) =>
        _dispatcher.TryEnqueue(() =>
        {
            IsPlaying = false;
            _positionTimer.Stop();
            UpdatePosition();

            if (error is not null)
            {
                Notice = new Notice(
                    "Playback stopped",
                    "The output device changed or was disconnected. Press Play to continue on the current device.",
                    NoticeSeverity.Warning);
            }
        });

    private void UpdatePosition()
    {
        Progress = _playback.Progress;
        PositionText = TimeFormat.Clock(_playback.Position);
    }

    // ---------------------------------------------------------------- Save / Copy

    private bool CanExport() => _session is not null && !IsExporting;

    private ExportOptions BuildOptions(ExportFormat format, int bitDepth)
    {
        var (start, end) = TrimFrames();
        return new ExportOptions(format, SelectedSampleRate.Value, bitDepth, SelectedBitRate.Value, start, end);
    }

    private string BaseFileName(SpoolSession session) =>
        $"RecordThis {session.StartedAt.LocalDateTime:yyyy-MM-dd HH.mm.ss}";

    [RelayCommand(CanExecute = nameof(CanExport))]
    private async Task SaveAsync()
    {
        if (_session is not { } session)
        {
            return;
        }

        var options = BuildOptions(IsMp3 ? ExportFormat.Mp3 : ExportFormat.Wav, SelectedBitDepth.Value);

        string? path;
        try
        {
            path = await _picker.PickSavePathAsync(
                BaseFileName(session),
                options.Extension,
                IsMp3 ? "MP3 audio" : "WAV audio");
        }
        catch (Exception ex)
        {
            Notice = new Notice("Couldn't open the save dialog", AudioErrorTranslator.Describe(ex), NoticeSeverity.Error);
            return;
        }

        if (path is null || !ReferenceEquals(_session, session))
        {
            return; // cancelled
        }

        await RunExportAsync(session, async progress =>
        {
            ExportResult result = await _export.ExportAsync(session, _mixedPeak, options, path, progress, CancellationToken.None);
            SavedPath = result.FilePath;
            HasUnsavedRecording = false;
            PreferCopy = false;

            string quality = options.Format == ExportFormat.Mp3
                ? $"MP3 {options.Mp3BitRate} kbps"
                : $"{SelectedSampleRate.Label}, {options.BitDepth}-bit";
            Notice = new Notice(
                "Recording saved",
                $"{Path.GetFileName(result.FilePath)} · {quality}" + (result.Downmixed ? ", downmixed to stereo" : string.Empty),
                NoticeSeverity.Success,
                NoticeAction.OpenFolder,
                "Show in folder",
                result.FilePath);
        });
    }

    /// <summary>Encodes an MP3 into a scratch folder and puts it on the clipboard as a file, ready for Ctrl+V in a chat app.</summary>
    [RelayCommand(CanExecute = nameof(CanExport))]
    private async Task CopyAsync()
    {
        if (_session is not { } session)
        {
            return;
        }

        await RunExportAsync(session, async progress =>
        {
            string baseName = BaseFileName(session);
            string path;
            string format;
            string? note = null;
            try
            {
                path = _clipboard.CreateTempFilePath(baseName + ".mp3");
                await _export.ExportAsync(session, _mixedPeak, BuildOptions(ExportFormat.Mp3, 16), path, progress, CancellationToken.None);
                format = "MP3";
            }
            catch (Mp3EncoderUnavailableException)
            {
                // Windows "N" editions ship without the MP3 encoder: fall back to a 16-bit WAV.
                path = _clipboard.CreateTempFilePath(baseName + ".wav");
                await _export.ExportAsync(session, _mixedPeak, BuildOptions(ExportFormat.Wav, 16), path, progress, CancellationToken.None);
                format = "WAV";
                note = "This PC has no MP3 encoder, so a WAV was copied instead.";
            }

            _clipboard.CopyFile(path);
            PreferCopy = true;

            Notice = new Notice(
                $"Copied to clipboard as {FormatSize(new FileInfo(path).Length)} {format}",
                note ?? string.Empty,
                NoticeSeverity.Success)
            {
                Lifetime = CopyNoticeLifetime,
            };
        });
    }

    /// <summary>Shared busy/progress/error handling for Save and Copy.</summary>
    private async Task RunExportAsync(SpoolSession session, Func<IProgress<double>, Task> work)
    {
        if (IsPlaying)
        {
            Pause();
        }

        IsExporting = true;
        ExportProgress = 0;
        Notice = null;
        var progress = new Progress<double>(p => ExportProgress = p * 100);

        try
        {
            if (_analysisTask is { } analysis)
            {
                await analysis; // the clip-safe gain needs the downmix peak
            }

            if (ReferenceEquals(_session, session))
            {
                await work(progress);
            }
        }
        catch (Mp3EncoderUnavailableException)
        {
            Notice = new Notice(
                "MP3 isn't available on this PC",
                "Windows on this PC doesn't include an MP3 encoder. Choose WAV instead, or install the Media Feature Pack from Optional features in Settings.",
                NoticeSeverity.Error);
        }
        catch (UnauthorizedAccessException)
        {
            Notice = new Notice(
                "Couldn't save there",
                "RecordThis doesn't have permission to write to that folder. Choose another location — your recording is still here.",
                NoticeSeverity.Error);
        }
        catch (Exception ex)
        {
            Notice = new Notice(
                "Something went wrong",
                $"{AudioErrorTranslator.Describe(ex)} Your recording is still here — try again.",
                NoticeSeverity.Error);
        }
        finally
        {
            IsExporting = false;
        }
    }

    private static string FormatSize(long bytes) =>
        bytes >= 1024 * 1024 ? $"{bytes / (1024.0 * 1024):0.0} MB" : $"{Math.Max(1, bytes / 1024)} KB";

    // ---------------------------------------------------------------- Discard / New recording

    private bool CanDiscard() => _session is not null && !IsExporting;

    /// <summary>
    /// Unsaved: hand the recording to the capture screen, which offers Undo for 5 seconds before deleting it.
    /// Saved: nothing to lose, so just start over.
    /// </summary>
    [RelayCommand(CanExecute = nameof(CanDiscard))]
    private void Discard()
    {
        if (!HasUnsavedRecording || _result is not { } result)
        {
            CloseSession();
            return;
        }

        Release(deleteSpool: false);
        HasUnsavedRecording = false;
        Notice = null;
        Discarded?.Invoke(this, result);
        Closed?.Invoke(this, EventArgs.Empty);
    }

    [RelayCommand]
    private void RunNoticeAction() => ShellLauncher.Run(Notice);

    // ---------------------------------------------------------------- Helpers

    private void NotifyCommands()
    {
        PlayPauseCommand.NotifyCanExecuteChanged();
        SaveCommand.NotifyCanExecuteChanged();
        CopyCommand.NotifyCanExecuteChanged();
        DiscardCommand.NotifyCanExecuteChanged();
        TrimSilenceCommand.NotifyCanExecuteChanged();
        ResetTrimCommand.NotifyCanExecuteChanged();
    }

    private static string BuildDetails(CaptureResult result)
    {
        var format = result.Session.Format;
        string details = $"{result.Session.DeviceName} · {format.Describe()}";
        if (DownmixMatrix.RequiresDownmix(format.Channels))
        {
            details += " · Exports as stereo (ITU-R BS.775 downmix)";
        }

        return details;
    }

    private static Notice? NoticeFor(CaptureResult result) => result.Reason switch
    {
        StopReason.DeviceLost => new Notice(
            "Recording stopped: device disconnected",
            $"{result.Session.DeviceName} was unplugged or disabled. Everything captured up to that moment is here.",
            NoticeSeverity.Warning),

        StopReason.SystemSuspend => new Notice(
            "Recording stopped before sleep",
            "Your PC went to sleep, so RecordThis finished the recording first. Nothing captured before that was lost.",
            NoticeSeverity.Informational),

        StopReason.Error => new Notice(
            "Recording stopped unexpectedly",
            $"{(result.Error is null ? "An error occurred." : AudioErrorTranslator.Describe(result.Error))} Everything captured before the problem is here.",
            NoticeSeverity.Warning),

        StopReason.Recovered => new Notice(
            "Recovered recording",
            "This audio was rebuilt from the scratch file a previous session left behind. Save it or discard it.",
            NoticeSeverity.Informational),

        _ => null,
    };

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _noticeTimer.Stop();
        _playback.PlaybackStopped -= OnPlaybackStopped;

        // An unsaved spool is deliberately left on disk so it's offered for recovery next launch.
        Release(deleteSpool: _session is not null && !HasUnsavedRecording);
        _playback.Dispose();
    }
}
