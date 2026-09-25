namespace RecordThis.Models;

public enum NoticeSeverity
{
    Informational,
    Success,
    Warning,
    Error,
}

public enum NoticeAction
{
    None,
    OpenSoundSettings,
    OpenMicrophonePrivacy,
    OpenFolder,
}

/// <summary>A message shown in an InfoBar, optionally with one follow-up action.</summary>
public sealed record Notice(
    string Title,
    string Message,
    NoticeSeverity Severity,
    NoticeAction Action = NoticeAction.None,
    string? ActionText = null,
    string? ActionArgument = null)
{
    /// <summary>If set, the notice removes itself after this long.</summary>
    public TimeSpan? Lifetime { get; init; }
}
