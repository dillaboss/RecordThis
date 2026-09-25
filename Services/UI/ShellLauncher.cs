using System.Diagnostics;
using RecordThis.Models;

namespace RecordThis.Services.UI;

/// <summary>Opens the Windows surface a notice points the user to.</summary>
public static class ShellLauncher
{
    public static void Run(Notice? notice)
    {
        if (notice is null)
        {
            return;
        }

        try
        {
            switch (notice.Action)
            {
                case NoticeAction.OpenSoundSettings:
                    // Classic Sound control panel: Properties > Advanced holds the exclusive-mode switches.
                    Process.Start(new ProcessStartInfo("control.exe", "mmsys.cpl") { UseShellExecute = true });
                    break;

                case NoticeAction.OpenMicrophonePrivacy:
                    Process.Start(new ProcessStartInfo("ms-settings:privacy-microphone") { UseShellExecute = true });
                    break;

                case NoticeAction.OpenFolder when !string.IsNullOrEmpty(notice.ActionArgument):
                    Process.Start(new ProcessStartInfo("explorer.exe", $"/select,\"{notice.ActionArgument}\"") { UseShellExecute = true });
                    break;
            }
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"Couldn't launch {notice.Action}: {ex.Message}");
        }
    }
}
