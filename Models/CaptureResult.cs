namespace RecordThis.Models;

public enum StopReason
{
    User,
    DeviceLost,
    SystemSuspend,
    Error,
    Recovered,
}

public sealed record CaptureResult(
    SpoolSession Session,
    StopReason Reason,
    TimeSpan Duration,
    TimeSpan SilenceInjected,
    Exception? Error);
