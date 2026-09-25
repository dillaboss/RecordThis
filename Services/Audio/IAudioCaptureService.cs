using RecordThis.Models;

namespace RecordThis.Services.Audio;

public interface IAudioCaptureService : IDisposable
{
    /// <summary>Raised exactly once per recording, on a background thread, whatever ended it.</summary>
    event EventHandler<CaptureResult>? Stopped;

    bool IsRecording { get; }

    string? ActiveDeviceId { get; }

    CaptureFormat? ActiveFormat { get; }

    /// <summary>Timeline length on disk, including watchdog-injected silence.</summary>
    TimeSpan RecordedDuration { get; }

    Task<SpoolSession> StartAsync(AudioDeviceModel device);

    Task<CaptureResult?> StopAsync(StopReason reason = StopReason.User);

    /// <summary>Highest sample magnitude since the previous call (lock-free; for UI meters).</summary>
    float ReadAndResetPeak();
}
