using RecordThis.Models;

namespace RecordThis.Services.Audio;

/// <summary>Turns WASAPI HRESULTs into messages that tell the user what to do next.</summary>
public static class AudioErrorTranslator
{
    private const int AudclntEDeviceInvalidated = unchecked((int)0x88890004);
    private const int AudclntEUnsupportedFormat = unchecked((int)0x88890008);
    private const int AudclntEDeviceInUse = unchecked((int)0x8889000A);
    private const int AudclntEServiceNotRunning = unchecked((int)0x88890010);
    private const int AudclntEExclusiveModeOnly = unchecked((int)0x88890012);
    private const int EAccessDenied = unchecked((int)0x80070005);
    private const int ENotFound = unchecked((int)0x80070490);
    private const int EDeviceNotConnected = unchecked((int)0x8007048F);

    private static readonly HashSet<int> KnownCodes =
    [
        AudclntEDeviceInvalidated, AudclntEUnsupportedFormat, AudclntEDeviceInUse, AudclntEServiceNotRunning,
        AudclntEExclusiveModeOnly, EAccessDenied, ENotFound, EDeviceNotConnected,
    ];

    public static bool IsDeviceLoss(Exception error) =>
        FindHResult(error) is AudclntEDeviceInvalidated or ENotFound or EDeviceNotConnected;

    public static Notice ToStartNotice(Exception error, AudioDeviceModel? device)
    {
        string name = device?.Name ?? "The selected device";
        return FindHResult(error) switch
        {
            AudclntEExclusiveModeOnly or AudclntEDeviceInUse => new Notice(
                "Another app has exclusive control of this device",
                $"{name} is locked in exclusive mode. Close the app using it, or open Sound settings, choose the device's Properties > Advanced and clear \"Allow applications to take exclusive control of this device\".",
                NoticeSeverity.Error,
                NoticeAction.OpenSoundSettings,
                "Open Sound settings"),

            EAccessDenied when device is { IsLoopback: false } => new Notice(
                "Windows is blocking microphone access",
                "Turn on \"Let desktop apps access your microphone\" in Privacy & security settings, then try again.",
                NoticeSeverity.Error,
                NoticeAction.OpenMicrophonePrivacy,
                "Open privacy settings"),

            AudclntEDeviceInvalidated or ENotFound or EDeviceNotConnected => new Notice(
                "Device unavailable",
                $"{name} was unplugged or disabled. Choose another source.",
                NoticeSeverity.Warning),

            AudclntEUnsupportedFormat => new Notice(
                "The device's audio format isn't supported",
                "Open Sound settings and change the device's default format (for example to 48000 Hz, 24-bit), then try again.",
                NoticeSeverity.Error,
                NoticeAction.OpenSoundSettings,
                "Open Sound settings"),

            AudclntEServiceNotRunning => new Notice(
                "Windows Audio isn't running",
                "Restart the Windows Audio service, or restart your PC, then try again.",
                NoticeSeverity.Error),

            _ => new Notice("Recording couldn't start", Describe(error), NoticeSeverity.Error),
        };
    }

    public static string Describe(Exception error) =>
        string.IsNullOrWhiteSpace(error.Message) ? $"Error 0x{error.HResult:X8}." : error.Message;

    private static int FindHResult(Exception error)
    {
        for (Exception? e = error; e is not null; e = e.InnerException)
        {
            if (KnownCodes.Contains(e.HResult))
            {
                return e.HResult;
            }
        }

        return error.HResult;
    }
}
