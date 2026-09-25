using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;

namespace RecordThis.Services.UI;

public interface IClipboardService
{
    /// <summary>A fresh path in RecordThis's clipboard scratch folder. Older copies in that folder are cleaned up.</summary>
    string CreateTempFilePath(string fileName);

    /// <summary>Puts a file on the clipboard exactly as Explorer's "Copy" does, so chat apps can paste it.</summary>
    void CopyFile(string path);
}

/// <summary>
/// Writes CF_HDROP (a file list) plus "Preferred DropEffect = Copy" through the Win32 clipboard.
/// That's the format Explorer uses, and what Discord, WhatsApp, Teams, Slack, email clients and
/// Explorer itself accept on Ctrl+V. The system owns the data afterwards, so pasting still works
/// after RecordThis is closed.
/// </summary>
public sealed class ClipboardService : IClipboardService
{
    private const uint CF_HDROP = 15;
    private const uint GMEM_MOVEABLE = 0x0002;
    private const uint GMEM_ZEROINIT = 0x0040;
    private const uint DROPEFFECT_COPY = 1;
    private const int DropFilesHeaderSize = 20; // DROPFILES: pFiles, pt.x, pt.y, fNC, fWide

    private readonly Func<nint> _windowHandle;
    private readonly string _folder = Path.Combine(Path.GetTempPath(), "RecordThis", "Clipboard");

    public ClipboardService(Func<nint> windowHandle) => _windowHandle = windowHandle;

    public string CreateTempFilePath(string fileName)
    {
        Directory.CreateDirectory(_folder);
        foreach (string old in Directory.EnumerateFiles(_folder))
        {
            try
            {
                File.Delete(old);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                Debug.WriteLine($"Couldn't clean {old}: {ex.Message}"); // still in use by a chat upload
            }
        }

        return Path.Combine(_folder, fileName);
    }

    public void CopyFile(string path)
    {
        string fullPath = Path.GetFullPath(path);

        // DROPFILES header followed by a double-null-terminated UTF-16 path list.
        byte[] names = Encoding.Unicode.GetBytes(fullPath + "\0\0");
        byte[] drop = new byte[DropFilesHeaderSize + names.Length];
        BitConverter.GetBytes(DropFilesHeaderSize).CopyTo(drop, 0); // pFiles: offset of the list
        BitConverter.GetBytes(1).CopyTo(drop, 16);                   // fWide: paths are Unicode
        names.CopyTo(drop, DropFilesHeaderSize);

        OpenClipboardWithRetry();
        try
        {
            if (!EmptyClipboard())
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }

            SetGlobal(CF_HDROP, drop);

            uint dropEffectFormat = RegisterClipboardFormat("Preferred DropEffect");
            if (dropEffectFormat != 0)
            {
                SetGlobal(dropEffectFormat, BitConverter.GetBytes(DROPEFFECT_COPY));
            }
        }
        finally
        {
            CloseClipboard();
        }
    }

    private void OpenClipboardWithRetry()
    {
        // Another app (clipboard managers, RDP) may hold the clipboard for a moment.
        for (int attempt = 0; attempt < 10; attempt++)
        {
            if (OpenClipboard(_windowHandle()))
            {
                return;
            }

            Thread.Sleep(30);
        }

        throw new InvalidOperationException("The clipboard is busy in another app. Try again in a moment.");
    }

    private static void SetGlobal(uint format, byte[] data)
    {
        nint handle = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, (nuint)data.Length);
        if (handle == 0)
        {
            throw new OutOfMemoryException();
        }

        nint pointer = GlobalLock(handle);
        if (pointer == 0)
        {
            GlobalFree(handle);
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }

        try
        {
            Marshal.Copy(data, 0, pointer, data.Length);
        }
        finally
        {
            GlobalUnlock(handle);
        }

        if (SetClipboardData(format, handle) == 0)
        {
            int error = Marshal.GetLastWin32Error();
            GlobalFree(handle); // ownership only passes to the system on success
            throw new Win32Exception(error);
        }
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool OpenClipboard(nint hWndNewOwner);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool CloseClipboard();

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool EmptyClipboard();

    [DllImport("user32.dll", SetLastError = true)]
    private static extern nint SetClipboardData(uint uFormat, nint hMem);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint RegisterClipboardFormat(string lpszFormat);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern nint GlobalAlloc(uint uFlags, nuint dwBytes);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern nint GlobalLock(nint hMem);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GlobalUnlock(nint hMem);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern nint GlobalFree(nint hMem);
}
