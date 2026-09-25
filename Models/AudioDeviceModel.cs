namespace RecordThis.Models;

public enum DeviceKind
{
    Input,
    Output,
}

/// <summary>An active Windows audio endpoint the user can record from.</summary>
public sealed record AudioDeviceModel(string Id, string Name, DeviceKind Kind, bool IsDefault)
{
    /// <summary>Render endpoints (speakers, headphones) are recorded through WASAPI loopback.</summary>
    public bool IsLoopback => Kind == DeviceKind.Output;

    public string Glyph => IsLoopback ? "\uE767" : "\uE720";

    public string Subtitle => (IsLoopback ? "Everything this device plays" : "Microphone or line input")
                              + (IsDefault ? " (Windows default)" : string.Empty);

    public string ModeLabel => IsLoopback ? "WASAPI Loopback" : "WASAPI Capture";
}
