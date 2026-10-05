using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Dispatching;
using RecordThis.Models;
using RecordThis.Services.Audio;
using RecordThis.Services.Devices;
using RecordThis.Services.Platform;
using RecordThis.Services.Storage;
using RecordThis.Services.UI;

namespace RecordThis.ViewModels;

/// <summary>Device selection, live meter, start/stop, and crash-recovery prompts.</summary>
public sealed partial class CaptureViewModel : ObservableObject, IDisposable
{
    private const double MeterFloorDb = -60;
    private const float DecayPerTick = 0.963f;          // ≈ 20 dB/s release at 60 fps
    private const long ClipHoldMilliseconds = 1500;
    private static readonly TimeSpan MeterInterval = TimeSpan.FromMilliseconds(16);
    private static readonly TimeSpan RefreshDebounce = TimeSpan.FromMilliseconds(400);
    private static readonly TimeSpan SuspendStopTimeout = TimeSpan.FromSeconds(3);
    private static readonly TimeSpan UndoWindow = TimeSpan.FromSeconds(5);

    private readonly IAudioCaptureService _capture;
    private readonly WindowsDeviceManager _devices;
    private readonly SpoolFileManager _spools;
    private readonly PowerEventService _power;
    private readonly SettingsService _settings;
    private readonly DispatcherQueue _dispatcher;
    private readonly DispatcherQueueTimer _meterTimer;
    private readonly DispatcherQueueTimer _refreshTimer;
    private readonly DispatcherQueueTimer _noticeTimer;
    private readonly HashSet<Guid> _handledSessions = [];
    private readonly Queue<SpoolSession> _recoveryQueue = new();

    private float _displayPeak;
    private long _clipUntil;
    private bool _refreshPending;
    private bool _disposed;
    private IReadOnlyList<AudioDeviceModel> _allDevices = [];
    private CaptureResult? _pendingDiscard;

    public CaptureViewModel(
        IAudioCaptureService capture,
        WindowsDeviceManager devices,
        SpoolFileManager spools,
        PowerEventService power,
        SettingsService settings)
    {
        _capture = capture;
        _devices = devices;
        _spools = spools;
        _power = power;
        _settings = settings;
        _sourceKind = Enum.TryParse(settings.Current.SourceKind, out DeviceKind kind) ? kind : DeviceKind.Output;
        _dispatcher = DispatcherQueue.GetForCurrentThread()
                      ?? throw new InvalidOperationException("CaptureViewModel must be created on the UI thread.");

        _meterTimer = _dispatcher.CreateTimer();
        _meterTimer.Interval = MeterInterval;
        _meterTimer.Tick += (_, _) => OnMeterTick();

        _refreshTimer = _dispatcher.CreateTimer();
        _refreshTimer.Interval = RefreshDebounce;
        _refreshTimer.IsRepeating = false;
        _refreshTimer.Tick += (_, _) => OnRefreshDue();

        _noticeTimer = _dispatcher.CreateTimer();
        _noticeTimer.IsRepeating = false;
        _noticeTimer.Tick += (_, _) => Notice = null;

        _capture.Stopped += OnCaptureStopped;
        _devices.DevicesChanged += OnDevicesChanged;
        _devices.DeviceUnavailable += OnDeviceUnavailable;
        _power.Suspending += OnSuspending;

        RefreshDevices();
        CheckForRecoverableSessions();
    }

    /// <summary>Raised on the UI thread when a recording (or a recovered spool) is ready for review.</summary>
    public event EventHandler<CaptureResult>? RecordingFinished;

    public ObservableCollection<AudioDeviceModel> Devices { get; } = [];

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(ToggleRecordingCommand))]
    private AudioDeviceModel? _selectedDevice;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsIdle), nameof(CanChangeDevice), nameof(RecordButtonText))]
    [NotifyCanExecuteChangedFor(nameof(ToggleRecordingCommand), nameof(RecoverCommand), nameof(DiscardRecoveryCommand))]
    private bool _isRecording;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsIdle), nameof(CanChangeDevice))]
    [NotifyCanExecuteChangedFor(nameof(ToggleRecordingCommand), nameof(RecoverCommand), nameof(DiscardRecoveryCommand))]
    private bool _isBusy;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ElapsedText), nameof(RecordButtonText))]
    private TimeSpan _elapsed;

    [ObservableProperty]
    private string _levelText = "-∞ dBFS";

    /// <summary>ScaleX of the mask that hides the unlit part of the meter (1 = silent, 0 = full scale).</summary>
    [ObservableProperty]
    private double _meterMask = 1;

    [ObservableProperty]
    private bool _isClipping;

    [ObservableProperty]
    private string _formatText = string.Empty;

    [ObservableProperty]
    private string _modeText = string.Empty;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasNotice))]
    private Notice? _notice;

    public bool HasNotice => Notice is not null;

    /// <summary>Which tab is showing: speakers/headphones (loopback) or microphones.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SourcePlaceholder))]
    private DeviceKind _sourceKind;

    public string SourcePlaceholder => SourceKind == DeviceKind.Output
        ? "No speakers or headphones found"
        : "No microphones found";

    [ObservableProperty]
    private string _hotkeyGesture = "Ctrl+Alt+R";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HotkeyDescription))]
    private bool _isHotkeyAvailable;

    public string HotkeyDescription => IsHotkeyAvailable
        ? "Start or stop recording from any app"
        : "Unavailable: another app is already using this shortcut";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(RecoveryText))]
    [NotifyCanExecuteChangedFor(nameof(RecoverCommand), nameof(DiscardRecoveryCommand))]
    private SpoolSession? _pendingRecovery;

    public bool IsIdle => !IsRecording && !IsBusy;

    public bool CanChangeDevice => IsIdle;

    public string ElapsedText => TimeFormat.Clock(Elapsed);

    public string RecordButtonText => IsRecording ? $"Stop recording ({ElapsedText})" : "Start recording";

    public string RecoveryText
    {
        get
        {
            if (PendingRecovery is not { } session)
            {
                return string.Empty;
            }

            string more = _recoveryQueue.Count > 0 ? $" ({_recoveryQueue.Count} more after this one.)" : string.Empty;
            return $"{TimeFormat.Clock(session.Duration)} from {session.DeviceName}, started {session.StartedAt.LocalDateTime:g}. " +
                   $"RecordThis closed before it was saved.{more}";
        }
    }

    // ---------------------------------------------------------------- Recording

    [RelayCommand(CanExecute = nameof(CanToggleRecording))]
    private async Task ToggleRecordingAsync()
    {
        if (IsRecording)
        {
            await StopRecordingAsync();
            return;
        }

        if (SelectedDevice is not { } device)
        {
            return;
        }

        IsBusy = true;
        Notice = null;
        try
        {
            SpoolSession session = await _capture.StartAsync(device);
            if (_handledSessions.Contains(session.Id))
            {
                return; // the device failed immediately and the stop has already been handled
            }

            IsRecording = true;
            Elapsed = TimeSpan.Zero;
            FormatText = _capture.ActiveFormat?.Describe() ?? FormatText;
            ResetMeter();
            _meterTimer.Start();
        }
        catch (Exception ex)
        {
            Notice = AudioErrorTranslator.ToStartNotice(ex, device);
            if (AudioErrorTranslator.IsDeviceLoss(ex))
            {
                RefreshDevices();
            }
        }
        finally
        {
            IsBusy = false;
        }
    }

    private bool CanToggleRecording() => !IsBusy && (IsRecording || SelectedDevice is not null);

    /// <summary>Stops and finalizes the spool. When this returns, <see cref="RecordingFinished"/> has fired.</summary>
    public async Task StopRecordingAsync(StopReason reason = StopReason.User)
    {
        if (!IsRecording)
        {
            return;
        }

        IsBusy = true;
        try
        {
            CaptureResult? result = await _capture.StopAsync(reason);
            if (result is not null)
            {
                HandleStopped(result);
            }
        }
        finally
        {
            IsBusy = false;
        }
    }

    private void OnCaptureStopped(object? sender, CaptureResult result) =>
        _dispatcher.TryEnqueue(() => HandleStopped(result));

    /// <summary>Idempotent: both StopAsync's result and the Stopped event land here.</summary>
    private void HandleStopped(CaptureResult result)
    {
        if (_disposed || !_handledSessions.Add(result.Session.Id))
        {
            return;
        }

        _meterTimer.Stop();
        ResetMeter();
        IsRecording = false;
        Elapsed = TimeSpan.Zero;

        if (_refreshPending || result.Reason == StopReason.DeviceLost)
        {
            _refreshPending = false;
            RefreshDevices();
        }

        UpdateSelectionDetails();

        if (result.Session.FrameCount == 0)
        {
            _spools.Delete(result.Session);
            Notice = result.Reason switch
            {
                StopReason.DeviceLost => new Notice(
                    "Recording stopped",
                    $"{result.Session.DeviceName} was disconnected before any audio was captured. Choose another source.",
                    NoticeSeverity.Warning),
                _ when result.Error is not null => new Notice(
                    "Recording stopped",
                    AudioErrorTranslator.Describe(result.Error),
                    NoticeSeverity.Error),
                _ => new Notice("Nothing was recorded", "The recording ended before any audio was captured.", NoticeSeverity.Informational),
            };
            return;
        }

        RecordingFinished?.Invoke(this, result);
    }

    /// <summary>Runs on the SystemEvents thread; blocking here holds off sleep until the spool is flushed.</summary>
    private void OnSuspending(object? sender, EventArgs e)
    {
        if (!_capture.IsRecording)
        {
            return;
        }

        try
        {
            _capture.StopAsync(StopReason.SystemSuspend).Wait(SuspendStopTimeout);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine(ex);
        }
    }

    // ---------------------------------------------------------------- Meter

    private void OnMeterTick()
    {
        float peak = _capture.ReadAndResetPeak();
        _displayPeak = Math.Max(peak, _displayPeak * DecayPerTick);

        long now = Environment.TickCount64;
        if (peak >= 0.999f)
        {
            _clipUntil = now + ClipHoldMilliseconds;
        }

        IsClipping = now < _clipUntil;

        double db = _displayPeak > 1e-6f ? 20 * Math.Log10(_displayPeak) : double.NegativeInfinity;
        MeterMask = double.IsNegativeInfinity(db) ? 1 : 1 - Math.Clamp((db - MeterFloorDb) / -MeterFloorDb, 0, 1);
        LevelText = db < MeterFloorDb || double.IsNegativeInfinity(db) ? "-∞ dBFS" : $"{Math.Round(db):0} dBFS";

        var recorded = _capture.RecordedDuration;
        Elapsed = TimeSpan.FromSeconds(Math.Floor(recorded.TotalSeconds));
    }

    private void ResetMeter()
    {
        _displayPeak = 0;
        _clipUntil = 0;
        _capture.ReadAndResetPeak();
        MeterMask = 1;
        IsClipping = false;
        LevelText = "-∞ dBFS";
    }

    // ---------------------------------------------------------------- Devices

    partial void OnSelectedDeviceChanged(AudioDeviceModel? value)
    {
        if (value is not null)
        {
            if (value.Kind == DeviceKind.Output)
            {
                _settings.Current.LastOutputDeviceId = value.Id;
            }
            else
            {
                _settings.Current.LastInputDeviceId = value.Id;
            }

            _settings.Save();
        }

        if (!IsRecording)
        {
            UpdateSelectionDetails();
        }
    }

    partial void OnSourceKindChanged(DeviceKind value)
    {
        _settings.Current.SourceKind = value.ToString();
        _settings.Save();
        ApplySourceFilter();
    }

    private void UpdateSelectionDetails()
    {
        var device = SelectedDevice;
        ModeText = device?.ModeLabel ?? string.Empty;
        FormatText = device is null
            ? string.Empty
            : _devices.TryGetMixFormat(device.Id)?.Describe() ?? "Format unavailable";
    }

    private void OnDevicesChanged(object? sender, EventArgs e) =>
        _dispatcher.TryEnqueue(() =>
        {
            _refreshTimer.Stop();
            _refreshTimer.Start();
        });

    private void OnRefreshDue()
    {
        if (IsRecording || IsBusy)
        {
            _refreshPending = true; // don't disturb the source list mid-recording
            return;
        }

        RefreshDevices();
    }

    private void OnDeviceUnavailable(object? sender, string deviceId)
    {
        // MMDevAPI callback thread: never block it.
        if (string.Equals(deviceId, _capture.ActiveDeviceId, StringComparison.OrdinalIgnoreCase))
        {
            _ = Task.Run(() => _capture.StopAsync(StopReason.DeviceLost));
        }
    }

    private void RefreshDevices()
    {
        try
        {
            _allDevices = _devices.GetDevices();
        }
        catch (Exception ex)
        {
            Notice = new Notice("Couldn't list audio devices", AudioErrorTranslator.Describe(ex), NoticeSeverity.Error);
            return;
        }

        ApplySourceFilter();

        if (_allDevices.Count == 0)
        {
            Notice = new Notice(
                "No audio devices found",
                "Connect speakers, headphones or a microphone, or enable one in Sound settings.",
                NoticeSeverity.Warning,
                NoticeAction.OpenSoundSettings,
                "Open Sound settings");
        }
    }

    /// <summary>Shows only the current tab's devices and picks the best one: current, last used, then Windows default.</summary>
    private void ApplySourceFilter()
    {
        var visible = _allDevices.Where(d => d.Kind == SourceKind).ToList();
        string? keepId = SelectedDevice?.Kind == SourceKind ? SelectedDevice.Id : null;
        string? rememberedId = SourceKind == DeviceKind.Output
            ? _settings.Current.LastOutputDeviceId
            : _settings.Current.LastInputDeviceId;

        if (!visible.SequenceEqual(Devices))
        {
            Devices.Clear();
            foreach (var device in visible)
            {
                Devices.Add(device);
            }
        }

        SelectedDevice = visible.FirstOrDefault(d => d.Id == keepId)
                         ?? visible.FirstOrDefault(d => d.Id == rememberedId)
                         ?? visible.FirstOrDefault(d => d.IsDefault)
                         ?? visible.FirstOrDefault();

        if (SelectedDevice is null)
        {
            UpdateSelectionDetails();
        }
    }

    // ---------------------------------------------------------------- Recovery

    public void CheckForRecoverableSessions()
    {
        foreach (var session in _spools.FindOrphans())
        {
            if (session.Id != PendingRecovery?.Id && _recoveryQueue.All(s => s.Id != session.Id))
            {
                _recoveryQueue.Enqueue(session);
            }
        }

        if (PendingRecovery is null)
        {
            ShowNextRecovery();
        }
    }

    private void ShowNextRecovery()
    {
        PendingRecovery = _recoveryQueue.Count > 0 ? _recoveryQueue.Dequeue() : null;
        OnPropertyChanged(nameof(RecoveryText));
    }

    private bool CanUseRecovery() => PendingRecovery is not null && !IsRecording && !IsBusy;

    [RelayCommand(CanExecute = nameof(CanUseRecovery))]
    private void Recover()
    {
        if (PendingRecovery is not { } session)
        {
            return;
        }

        ShowNextRecovery();
        _handledSessions.Add(session.Id);
        RecordingFinished?.Invoke(this, new CaptureResult(session, StopReason.Recovered, session.Duration, TimeSpan.Zero, null));
    }

    [RelayCommand(CanExecute = nameof(CanUseRecovery))]
    private void DiscardRecovery()
    {
        if (PendingRecovery is { } session)
        {
            _spools.Delete(session);
        }

        ShowNextRecovery();
    }

    // ---------------------------------------------------------------- Notices

    [RelayCommand]
    private void RunNoticeAction()
    {
        if (Notice?.Action == NoticeAction.UndoDiscard && _pendingDiscard is { } discarded)
        {
            _pendingDiscard = null;
            Notice = null;
            RecordingFinished?.Invoke(this, discarded with { Reason = StopReason.User, Error = null });
            return;
        }

        ShellLauncher.Run(Notice);
    }

    partial void OnNoticeChanged(Notice? value)
    {
        _noticeTimer.Stop();

        // The undo chance ends as soon as its notice is gone or replaced.
        if (_pendingDiscard is not null && value?.Action != NoticeAction.UndoDiscard)
        {
            FinalizePendingDiscard();
        }

        if (value?.Lifetime is { } lifetime)
        {
            _noticeTimer.Interval = lifetime;
            _noticeTimer.Start();
        }
    }

    /// <summary>Called when the review screen discards an unsaved recording: keep the spool for 5 seconds so it can be undone.</summary>
    public void OfferUndoDiscard(CaptureResult discarded)
    {
        FinalizePendingDiscard();
        _pendingDiscard = discarded;
        Notice = new Notice("Recording discarded", string.Empty, NoticeSeverity.Informational, NoticeAction.UndoDiscard, "Undo")
        {
            Lifetime = UndoWindow,
        };
    }

    private void FinalizePendingDiscard()
    {
        if (_pendingDiscard is { } discarded)
        {
            _pendingDiscard = null;
            _spools.Delete(discarded.Session);
        }
    }

    /// <summary>Reported by the window once it has tried to register the global shortcut.</summary>
    public void SetHotkeyStatus(bool available, string gesture)
    {
        HotkeyGesture = gesture;
        IsHotkeyAvailable = available;
    }

    /// <summary>Global shortcut: start or stop, if that's possible right now.</summary>
    public bool TryToggleRecording()
    {
        if (!ToggleRecordingCommand.CanExecute(null))
        {
            return false;
        }

        ToggleRecordingCommand.Execute(null);
        return true;
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _meterTimer.Stop();
        _refreshTimer.Stop();
        _noticeTimer.Stop();
        FinalizePendingDiscard();
        _capture.Stopped -= OnCaptureStopped;
        _devices.DevicesChanged -= OnDevicesChanged;
        _devices.DeviceUnavailable -= OnDeviceUnavailable;
        _power.Suspending -= OnSuspending;
    }
}
