using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Dispatching;
using RecordThis.Models;
using RecordThis.Services.Audio;
using RecordThis.Services.Audio.Pipeline;
using RecordThis.Services.Storage;
using RecordThis.Services.UI;

namespace RecordThis.ViewModels;

/// <summary>Waveform preview, playback scrubbing, and WAV export orchestration.</summary>
public sealed partial class ReviewViewModel : ObservableObject, IDisposable
{
    private const int WaveformBuckets = 480;
    private static readonly TimeSpan PositionInterval = TimeSpan.FromMilliseconds(50);

    private readonly AudioExportService _export;
    private readonly SpoolFileManager _spools;
    private readonly IFilePickerService _picker;
    private readonly IDialogService _dialogs;
    private readonly IClipboardService _clipboard;
    private readonly ReviewPlaybackService _playback = new();
    private readonly DispatcherQueue _dispatcher;
    private readonly DispatcherQueueTimer _positionTimer;
    private readonly DispatcherQueueTimer _noticeTimer;

    private SpoolSession? _session;
    private IDisposable? _hold;
    private CancellationTokenSource? _analysisCts;
    private Task<bool>? _analysisTask;
    private float _mixedPeak;
    private bool _disposed;

    public ReviewViewModel(
        AudioExportService export,
        SpoolFileManager spools,
        IFilePickerService picker,
        IDialogService dialogs,
        IClipboardService clipboard)
    {
        _export = export;
        _spools = spools;
        _picker = picker;
        _dialogs = dialogs;
        _clipboard = clipboard;
        _dispatcher = DispatcherQueue.GetForCurrentThread()
                      ?? throw new InvalidOperationException("ReviewViewModel must be created on the UI thread.");

        _positionTimer = _dispatcher.CreateTimer();
        _positionTimer.Interval = PositionInterval;
        _positionTimer.Tick += (_, _) => UpdatePosition();

        _noticeTimer = _dispatcher.CreateTimer();
        _noticeTimer.IsRepeating = false;
        _noticeTimer.Tick += (_, _) => Notice = null;

        _playback.PlaybackStopped += OnPlaybackStopped;

        _selectedSampleRate = SampleRateChoices[1];
        _selectedBitDepth = BitDepthChoices[1];
    }

    /// <summary>Raised when the user discards (or starts over after saving) and the capture screen should return.</summary>
    public event EventHandler? Closed;

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

    [ObservableProperty]
    private ExportChoice _selectedSampleRate;

    [ObservableProperty]
    private ExportChoice _selectedBitDepth;

    [ObservableProperty]
    private string _headerText = string.Empty;

    [ObservableProperty]
    private string _detailsText = string.Empty;

    [ObservableProperty]
    private WaveformData? _waveform;

    [ObservableProperty]
    private bool _isAnalyzing;

    /// <summary>Playback position, 0..1.</summary>
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
    [NotifyPropertyChangedFor(nameof(DiscardText))]
    private bool _hasUnsavedRecording;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasNotice))]
    private Notice? _notice;

    public bool HasNotice => Notice is not null;

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

    public string DiscardText => HasUnsavedRecording ? "Discard" : "New recording";

    public string PlayPauseText => IsPlaying ? "Pause" : "Play";

    /// <summary>Pause (E769) while playing, Play (E768) otherwise.</summary>
    public string PlayPauseGlyph => IsPlaying ? "\uE769" : "\uE768";

    // ---------------------------------------------------------------- Session lifecycle

    public void Load(CaptureResult result)
    {
        if (_session is not null)
        {
            Release(deleteSpool: !HasUnsavedRecording);
        }

        var session = result.Session;
        _session = session;
        _hold = _spools.Hold(session);
        _mixedPeak = 0f;

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
        SelectedBitDepth = BitDepthChoices[1];

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

    private void Release(bool deleteSpool)
    {
        _analysisCts?.Cancel();
        _analysisCts?.Dispose();
        _analysisCts = null;
        _analysisTask = null;

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

            _mixedPeak = data.MixedPeak;
            float gain = AudioExportService.ClipSafeGain(data.MixedPeak);
            _playback.SetGain(gain);

            // Show exactly what will be exported (post-downmix, post-gain).
            var peaks = new float[data.Peaks.Length];
            for (int i = 0; i < peaks.Length; i++)
            {
                peaks[i] = Math.Min(1f, data.Peaks[i] * gain);
            }

            Waveform = new WaveformData(peaks, data.MixedPeak * gain);

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

    // ---------------------------------------------------------------- Export

    private bool CanSave() => _session is not null && !IsExporting;

    [RelayCommand(CanExecute = nameof(CanSave))]
    private async Task SaveAsync()
    {
        if (_session is not { } session)
        {
            return;
        }

        string? path;
        try
        {
            path = await _picker.PickSaveWavPathAsync($"RecordThis {session.StartedAt.LocalDateTime:yyyy-MM-dd HH.mm.ss}");
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

        if (IsPlaying)
        {
            Pause();
        }

        IsExporting = true;
        ExportProgress = 0;
        Notice = null;
        var options = new ExportOptions(SelectedSampleRate.Value, SelectedBitDepth.Value);
        var progress = new Progress<double>(p => ExportProgress = p * 100);

        try
        {
            if (_analysisTask is { } analysis)
            {
                await analysis; // the clip-safe gain needs the downmix peak
            }

            ExportResult result = await _export.ExportAsync(session, _mixedPeak, options, path, progress, CancellationToken.None);
            SavedPath = result.FilePath;
            HasUnsavedRecording = false;
            Notice = new Notice(
                "Recording saved",
                $"{Path.GetFileName(result.FilePath)} · {SelectedSampleRate.Label}, {options.BitDepth}-bit" +
                (result.Downmixed ? ", downmixed to stereo" : string.Empty),
                NoticeSeverity.Success,
                NoticeAction.OpenFolder,
                "Show in folder",
                result.FilePath);
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
                "Couldn't save the recording",
                $"{AudioErrorTranslator.Describe(ex)} Your recording is still here — try again or choose another location.",
                NoticeSeverity.Error);
        }
        finally
        {
            IsExporting = false;
        }
    }

    private bool CanCopy() => _session is not null && !IsExporting;

    /// <summary>Encodes an MP3 into a scratch folder and puts it on the clipboard as a file, ready for Ctrl+V in a chat app.</summary>
    [RelayCommand(CanExecute = nameof(CanCopy))]
    private async Task CopyAsync()
    {
        if (_session is not { } session)
        {
            return;
        }

        if (IsPlaying)
        {
            Pause();
        }

        IsExporting = true;
        ExportProgress = 0;
        Notice = null;
        var progress = new Progress<double>(p => ExportProgress = p * 100);
        string baseName = $"RecordThis {session.StartedAt.LocalDateTime:yyyy-MM-dd HH.mm.ss}";
        int sampleRate = SelectedSampleRate.Value;

        try
        {
            if (_analysisTask is { } analysis)
            {
                await analysis;
            }

            string path;
            string format;
            string? note = null;
            try
            {
                path = _clipboard.CreateTempFilePath(baseName + ".mp3");
                await _export.ExportMp3Async(session, _mixedPeak, sampleRate, path, progress, CancellationToken.None);
                format = "MP3";
            }
            catch (Mp3EncoderUnavailableException)
            {
                // Windows "N" editions ship without the MP3 encoder: fall back to a 16-bit WAV.
                path = _clipboard.CreateTempFilePath(baseName + ".wav");
                await _export.ExportAsync(session, _mixedPeak, new ExportOptions(sampleRate, 16), path, progress, CancellationToken.None);
                format = "WAV";
                note = "This PC has no MP3 encoder, so a WAV was copied instead.";
            }

            _clipboard.CopyFile(path);

            Notice = new Notice(
                $"Copied to clipboard as {FormatSize(new FileInfo(path).Length)} {format}",
                note ?? string.Empty,
                NoticeSeverity.Success)
            {
                Lifetime = TimeSpan.FromSeconds(5),
            };
        }
        catch (Exception ex)
        {
            Notice = new Notice(
                "Couldn't copy the recording",
                $"{AudioErrorTranslator.Describe(ex)} You can still use Save As instead.",
                NoticeSeverity.Error);
        }
        finally
        {
            IsExporting = false;
        }
    }

    private static string FormatSize(long bytes) =>
        bytes >= 1024 * 1024 ? $"{bytes / (1024.0 * 1024):0.0} MB" : $"{Math.Max(1, bytes / 1024)} KB";

    private bool CanDiscard() => _session is not null && !IsExporting;

    [RelayCommand(CanExecute = nameof(CanDiscard))]
    private async Task DiscardAsync()
    {
        if (HasUnsavedRecording)
        {
            var choice = await _dialogs.ShowAsync(
                "Discard this recording?",
                "The captured audio will be permanently deleted. This can't be undone.",
                primaryText: "Discard",
                closeText: "Cancel");

            if (choice != DialogChoice.Primary)
            {
                return;
            }
        }

        CloseSession();
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
