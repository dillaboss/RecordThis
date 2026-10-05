using System.Diagnostics;
using System.Runtime.InteropServices;

namespace RecordThis.Services.Platform;

/// <summary>
/// Registers a system-wide hotkey (Ctrl+Alt+R) with RegisterHotKey and listens for WM_HOTKEY by
/// subclassing the main window's procedure. Works while RecordThis is minimized or behind other
/// windows, so loopback recordings can start and stop without covering the content being recorded.
/// </summary>
public sealed class HotkeyService : IDisposable
{
    private const int HotkeyId = 0x5254; // "RT"
    private const uint WM_HOTKEY = 0x0312;
    private const int GWLP_WNDPROC = -4;
    private const uint MOD_ALT = 0x0001;
    private const uint MOD_CONTROL = 0x0002;
    private const uint MOD_NOREPEAT = 0x4000;
    private const uint VK_R = 0x52;

    private readonly nint _hwnd;
    private readonly WndProc _proc; // kept alive for as long as the subclass is installed
    private nint _previousProc;
    private bool _disposed;

    public HotkeyService(nint hwnd)
    {
        _hwnd = hwnd;
        _proc = HookProc;
        _previousProc = SetWindowLongPtr(_hwnd, GWLP_WNDPROC, Marshal.GetFunctionPointerForDelegate(_proc));

        IsRegistered = _previousProc != 0 &&
                       RegisterHotKey(_hwnd, HotkeyId, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_R);

        if (!IsRegistered)
        {
            Debug.WriteLine($"RegisterHotKey failed: {Marshal.GetLastWin32Error()}");
        }
    }

    /// <summary>Raised on the UI thread when the hotkey is pressed anywhere in Windows.</summary>
    public event EventHandler? Pressed;

    public string Gesture => "Ctrl+Alt+R";

    /// <summary>False if another app already owns the shortcut.</summary>
    public bool IsRegistered { get; }

    private nint HookProc(nint hWnd, uint msg, nint wParam, nint lParam)
    {
        if (msg == WM_HOTKEY && wParam == HotkeyId)
        {
            Pressed?.Invoke(this, EventArgs.Empty);
            return 0;
        }

        return CallWindowProc(_previousProc, hWnd, msg, wParam, lParam);
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        if (IsRegistered)
        {
            UnregisterHotKey(_hwnd, HotkeyId);
        }

        if (_previousProc != 0)
        {
            SetWindowLongPtr(_hwnd, GWLP_WNDPROC, _previousProc);
            _previousProc = 0;
        }
    }

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate nint WndProc(nint hWnd, uint msg, nint wParam, nint lParam);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool RegisterHotKey(nint hWnd, int id, uint fsModifiers, uint vk);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool UnregisterHotKey(nint hWnd, int id);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW", SetLastError = true)]
    private static extern nint SetWindowLongPtr(nint hWnd, int nIndex, nint dwNewLong);

    [DllImport("user32.dll", EntryPoint = "CallWindowProcW")]
    private static extern nint CallWindowProc(nint lpPrevWndFunc, nint hWnd, uint msg, nint wParam, nint lParam);
}
