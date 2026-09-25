using System.Diagnostics;
using NAudio.CoreAudioApi;
using NAudio.Wave;
using RecordThis.Models;
using RecordThis.Services.Storage;

namespace RecordThis.Services.Audio;

/// <summary>
/// Records a capture endpoint (WasapiCapture) or render endpoint (WasapiLoopbackCapture) straight
/// to a disk spool in the device's native mix format.
///
/// Silence watchdog: in loopback mode Windows delivers no packets while nothing is playing, which
/// would shrink the timeline. A PeriodicTimer checks how long it has been since the last real
/// packet; once that exceeds 1.5× the WASAPI buffer duration it writes zero-level frames covering
/// the wall-clock time elapsed since that packet. The anchor is reset on every real packet, so the
/// device clock and the QPC clock never accumulate drift against each other.
/// </summary>
public sealed class WasapiAudioCaptureService : IAudioCaptureService
{
    /// <summary>NAudio polls the endpoint every half buffer, i.e. ~50 ms packets.</summary>
    private const int BufferMilliseconds = 100;
    private const double GapThresholdFactor = 1.5;
    private static readonly TimeSpan WatchdogInterval = TimeSpan.FromMilliseconds(20);
    private static readonly TimeSpan StopTimeout = TimeSpan.FromSeconds(2);

    private readonly SpoolFileManager _spoolManager;
    private readonly object _gate = new();

    // Guarded by _gate.
    private MMDevice? _device;
    private WasapiCapture? _client;
    private FileStream? _spool;
    private SpoolSession? _session;
    private CaptureFormat? _format;
    private SilenceInjectionBuffer? _silence;
    private CancellationTokenSource? _watchdogCts;
    private TaskCompletionSource<CaptureResult>? _completion;
    private StopReason? _requestedReason;
    private Exception? _pendingError;
    private long _gapThresholdTicks;
    private long _anchorTimestamp;
    private long _anchorFrames;
    private long _silenceFrames;

    // Written under _gate, read lock-free by the UI thread.
    private long _framesWritten;
    private int _sampleRate;
    private string? _deviceId;
    private int _peakBits;

    public WasapiAudioCaptureService(SpoolFileManager spoolManager)
    {
        _spoolManager = spoolManager;
    }

    public event EventHandler<CaptureResult>? Stopped;

    public bool IsRecording
    {
        get
        {
            lock (_gate)
            {
                return _spool is not null;
            }
        }
    }

    public string? ActiveDeviceId => Volatile.Read(ref _deviceId);

    public CaptureFormat? ActiveFormat
    {
        get
        {
            lock (_gate)
            {
                return _format;
            }
        }
    }

    public TimeSpan RecordedDuration
    {
        get
        {
            int rate = Volatile.Read(ref _sampleRate);
            return rate == 0 ? TimeSpan.Zero : TimeSpan.FromSeconds((double)Interlocked.Read(ref _framesWritten) / rate);
        }
    }

    // COM objects are created on a thread-pool (MTA) thread with no SynchronizationContext, so NAudio
    // raises RecordingStopped directly on its capture thread rather than posting to the UI.
    public Task<SpoolSession> StartAsync(AudioDeviceModel device) => Task.Run(() => StartCore(device));

    private SpoolSession StartCore(AudioDeviceModel device)
    {
        lock (_gate)
        {
            if (_client is not null)
            {
                throw new InvalidOperationException("A recording is already in progress.");
            }
        }

        MMDevice? mmDevice = null;
        WasapiCapture? client = null;
        FileStream? spool = null;
        SpoolSession? session = null;
        var enumerator = new MMDeviceEnumerator();
        try
        {
            mmDevice = enumerator.GetDevice(device.Id);

            // Loopback for render endpoints, standard capture for microphones. Both run in shared
            // mode with the engine's own MixFormat, so AUDCLNT_E_UNSUPPORTED_FORMAT cannot occur
            // from us asking for a format the engine doesn't mix in.
            client = device.IsLoopback
                ? new WasapiLoopbackCapture(mmDevice)
                : new WasapiCapture(mmDevice, false, BufferMilliseconds);

            var format = CaptureFormat.FromWaveFormat(client.WaveFormat);
            session = _spoolManager.CreateSession(format, device.Name);
            spool = _spoolManager.OpenForWriting(session);

            lock (_gate)
            {
                _device = mmDevice;
                _client = client;
                _spool = spool;
                _session = session;
                _format = format;
                _silence = new SilenceInjectionBuffer(format);
                _completion = new TaskCompletionSource<CaptureResult>(TaskCreationOptions.RunContinuationsAsynchronously);
                _requestedReason = null;
                _pendingError = null;
                _gapThresholdTicks = (long)(Stopwatch.Frequency * (BufferMilliseconds / 1000.0) * GapThresholdFactor);
                _silenceFrames = 0;
                Interlocked.Exchange(ref _framesWritten, 0);
                Interlocked.Exchange(ref _peakBits, 0);
                Volatile.Write(ref _sampleRate, format.SampleRate);
                Volatile.Write(ref _deviceId, device.Id);
                _anchorFrames = 0;
                _anchorTimestamp = Stopwatch.GetTimestamp();
            }

            client.DataAvailable += OnDataAvailable;
            client.RecordingStopped += OnRecordingStopped;
            client.StartRecording(); // IAudioClient::Initialize runs here; COM errors surface synchronously.

            var cts = new CancellationTokenSource();
            lock (_gate)
            {
                _anchorTimestamp = Stopwatch.GetTimestamp();
                _watchdogCts = cts;
            }
            _ = Task.Run(() => RunWatchdogAsync(cts.Token));

            return session;
        }
        catch
        {
            lock (_gate)
            {
                _device = null;
                _client = null;
                _spool = null;
                _session = null;
                _format = null;
                _silence = null;
                _completion = null;
                Volatile.Write(ref _deviceId, null);
                Volatile.Write(ref _sampleRate, 0);
            }

            if (client is not null)
            {
                client.DataAvailable -= OnDataAvailable;
                client.RecordingStopped -= OnRecordingStopped;
                SafeDispose(client);
            }

            SafeDispose(mmDevice);
            spool?.Dispose();
            if (session is not null)
            {
                _spoolManager.Delete(session);
            }

            throw;
        }
        finally
        {
            SafeDispose(enumerator);
        }
    }

    /// <summary>Runs on NAudio's capture thread. Zero-RAM spooling: every packet goes straight to disk.</summary>
    private void OnDataAvailable(object? sender, WaveInEventArgs e)
    {
        if (e.BytesRecorded <= 0)
        {
            return; // Loopback polls return empty packets while the endpoint is silent.
        }

        CaptureFormat? format;
        lock (_gate)
        {
            format = _format;
            if (_spool is null || format is null)
            {
                return;
            }

            int frames = e.BytesRecorded / format.BlockAlign;
            try
            {
                _spool.Write(e.Buffer, 0, frames * format.BlockAlign);
            }
            catch (IOException ex)
            {
                RequestStopWithError(ex); // e.g. disk full: finalize what we have
                return;
            }

            Interlocked.Add(ref _framesWritten, frames);
            _anchorTimestamp = Stopwatch.GetTimestamp();
            _anchorFrames = _framesWritten;
        }

        UpdatePeak(SampleConverter.Peak(e.Buffer.AsSpan(0, e.BytesRecorded), format));
    }

    private async Task RunWatchdogAsync(CancellationToken token)
    {
        using var timer = new PeriodicTimer(WatchdogInterval);
        try
        {
            while (await timer.WaitForNextTickAsync(token).ConfigureAwait(false))
            {
                lock (_gate)
                {
                    if (_spool is null)
                    {
                        return;
                    }

                    try
                    {
                        FillSilenceGap(force: false);
                    }
                    catch (IOException ex)
                    {
                        RequestStopWithError(ex);
                        return;
                    }
                }
            }
        }
        catch (OperationCanceledException)
        {
        }
    }

    /// <summary>Caller must hold <see cref="_gate"/>.</summary>
    private void FillSilenceGap(bool force)
    {
        if (_spool is null || _format is null || _silence is null)
        {
            return;
        }

        long elapsedTicks = Stopwatch.GetTimestamp() - _anchorTimestamp;
        if (!force && elapsedTicks < _gapThresholdTicks)
        {
            return;
        }

        long expectedFrames = _anchorFrames + elapsedTicks * _format.SampleRate / Stopwatch.Frequency;
        long deficit = expectedFrames - _framesWritten;
        if (deficit <= 0)
        {
            return;
        }

        _silence.Write(_spool, deficit);
        Interlocked.Add(ref _framesWritten, deficit);
        _silenceFrames += deficit;
    }

    /// <summary>Caller must hold <see cref="_gate"/>.</summary>
    private void RequestStopWithError(Exception error)
    {
        _pendingError ??= error;
        _requestedReason ??= StopReason.Error;
        try
        {
            _client?.StopRecording();
        }
        catch (Exception ex)
        {
            Debug.WriteLine(ex);
        }
    }

    public async Task<CaptureResult?> StopAsync(StopReason reason = StopReason.User)
    {
        WasapiCapture client;
        Task<CaptureResult> completion;
        lock (_gate)
        {
            if (_client is null || _completion is null)
            {
                return null;
            }

            _requestedReason ??= reason;
            client = _client;
            completion = _completion.Task;
        }

        try
        {
            client.StopRecording();
        }
        catch (Exception ex)
        {
            Debug.WriteLine(ex);
        }

        var finished = await Task.WhenAny(completion, Task.Delay(StopTimeout)).ConfigureAwait(false);
        if (finished != completion)
        {
            // The driver never let the capture thread exit. Finalize the spool anyway so no audio is lost.
            FinalizeCapture(new TimeoutException("The audio device stopped responding."));
        }

        return await completion.ConfigureAwait(false);
    }

    private void OnRecordingStopped(object? sender, StoppedEventArgs e) => FinalizeCapture(e.Exception);

    /// <summary>Idempotent: flushes buffers, closes the spool and raises <see cref="Stopped"/> once.</summary>
    private void FinalizeCapture(Exception? error)
    {
        CaptureResult result;
        WasapiCapture? client;
        MMDevice? device;
        TaskCompletionSource<CaptureResult>? completion;

        lock (_gate)
        {
            if (_spool is null || _session is null || _format is null)
            {
                return;
            }

            _watchdogCts?.Cancel();
            error ??= _pendingError;

            try
            {
                FillSilenceGap(force: true); // time since the last packet is still part of the timeline
            }
            catch (IOException ex)
            {
                error ??= ex;
            }

            try
            {
                _spool.Flush(flushToDisk: true);
            }
            catch (IOException ex)
            {
                error ??= ex;
            }

            _spool.Dispose();

            StopReason reason = _requestedReason
                                ?? (error is null ? StopReason.User
                                    : AudioErrorTranslator.IsDeviceLoss(error) ? StopReason.DeviceLost
                                    : StopReason.Error);

            int rate = _format.SampleRate;
            result = new CaptureResult(
                _session,
                reason,
                TimeSpan.FromSeconds((double)_framesWritten / rate),
                TimeSpan.FromSeconds((double)_silenceFrames / rate),
                reason is StopReason.User or StopReason.SystemSuspend ? null : error);

            client = _client;
            device = _device;
            completion = _completion;

            _client = null;
            _device = null;
            _spool = null;
            _session = null;
            _format = null;
            _silence = null;
            _watchdogCts = null;
            _completion = null;
            Volatile.Write(ref _deviceId, null);
        }

        if (client is not null)
        {
            client.DataAvailable -= OnDataAvailable;
            client.RecordingStopped -= OnRecordingStopped;

            // We may be on NAudio's capture thread; Dispose() joins that thread, so do it elsewhere.
            _ = Task.Run(() =>
            {
                SafeDispose(client);
                SafeDispose(device);
            });
        }

        completion?.TrySetResult(result);
        Stopped?.Invoke(this, result);
    }

    public float ReadAndResetPeak() => BitConverter.Int32BitsToSingle(Interlocked.Exchange(ref _peakBits, 0));

    private void UpdatePeak(float peak)
    {
        // For non-negative floats, integer bit patterns sort the same way as the values.
        int bits = BitConverter.SingleToInt32Bits(peak);
        int current;
        do
        {
            current = Volatile.Read(ref _peakBits);
            if (bits <= current)
            {
                return;
            }
        }
        while (Interlocked.CompareExchange(ref _peakBits, bits, current) != current);
    }

    public void Dispose()
    {
        if (IsRecording)
        {
            StopAsync(StopReason.User).GetAwaiter().GetResult();
        }
    }

    private static void SafeDispose(object? value)
    {
        try
        {
            (value as IDisposable)?.Dispose();
        }
        catch (Exception ex)
        {
            Debug.WriteLine(ex);
        }
    }
}
