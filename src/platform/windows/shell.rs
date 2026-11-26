//! Shell and window integration: global hotkey and sleep notifications (via a hidden window on
//! its own thread), clipboard-as-file, Explorer/Settings launching, the accent colour, title-bar
//! colours and the taskbar "recording" badge.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, COLORREF, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT};
use windows::Win32::UI::Shell::{ITaskbarList3, ShellExecuteW, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{ensure_ui_com, wide};
use crate::platform::AccentColors;

const HOTKEY_ID: i32 = 0x5254; // "RT"
const VK_R: u32 = 0x52;
const PBT_APMSUSPEND: usize = 0x4;
const WM_QUIT_HOOKS: u32 = WM_APP + 1;

struct Callbacks {
    on_hotkey: Box<dyn Fn() + Send>,
    on_suspend: Box<dyn Fn() + Send>,
}

thread_local! {
    static HOOK_CALLBACKS: RefCell<Option<Callbacks>> = const { RefCell::new(None) };
}

unsafe extern "system" fn hooks_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
            HOOK_CALLBACKS.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    (c.on_hotkey)();
                }
            });
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 == PBT_APMSUSPEND {
                // Blocking here holds off sleep until the recording is finalized.
                HOOK_CALLBACKS.with(|c| {
                    if let Some(c) = c.borrow().as_ref() {
                        (c.on_suspend)();
                    }
                });
            }
            LRESULT(1)
        }
        WM_QUIT_HOOKS => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// System-wide Ctrl+Alt+R and finalize-before-sleep. Both callbacks run on the hooks thread.
pub struct SystemHooks {
    hwnd: isize,
    registered: bool,
    thread: Option<JoinHandle<()>>,
}

impl SystemHooks {
    pub fn install(on_hotkey: Box<dyn Fn() + Send>, on_suspend: Box<dyn Fn() + Send>) -> Self {
        let (tx, rx) = mpsc::channel::<(isize, bool)>();
        let thread = std::thread::Builder::new()
            .name("system-hooks".into())
            .spawn(move || unsafe {
                HOOK_CALLBACKS.with(|c| *c.borrow_mut() = Some(Callbacks { on_hotkey, on_suspend }));
                let instance = GetModuleHandleW(None).unwrap_or_default();
                let class = w!("RecordThisHooks");
                let wc = WNDCLASSW { lpfnWndProc: Some(hooks_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
                RegisterClassW(&wc);
                // A hidden top-level window (not message-only): only top-level windows get WM_POWERBROADCAST.
                let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("RecordThis hooks"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None);
                let Ok(hwnd) = hwnd else {
                    let _ = tx.send((0, false));
                    return;
                };
                let registered = RegisterHotKey(Some(hwnd), HOTKEY_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_R).is_ok();
                let _ = tx.send((hwnd.0 as isize, registered));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if registered {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                }
            })
            .ok();
        let (hwnd, registered) = rx.recv().unwrap_or((0, false));
        Self { hwnd, registered, thread }
    }

    /// False if another app already owns the shortcut.
    pub fn hotkey_registered(&self) -> bool {
        self.registered
    }
}

impl Drop for SystemHooks {
    fn drop(&mut self) {
        if self.hwnd != 0 {
            let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_QUIT_HOOKS, WPARAM(0), LPARAM(0)) };
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn hwnd_of(window: &slint::Window) -> Option<HWND> {
    let handle = window.window_handle();
    let raw = handle.window_handle().ok()?.as_raw();
    match raw {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// Puts a file on the clipboard exactly as Explorer's Copy does (CF_HDROP + "Preferred DropEffect"),
/// which is what Discord, WhatsApp, Teams, Slack, mail and Explorer accept on Ctrl+V. The system
/// owns the data afterwards, so pasting still works after RecordThis closes.
pub fn copy_file_to_clipboard(window: &slint::Window, path: &Path) -> Result<(), String> {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut full = full.to_string_lossy().into_owned();
    if let Some(stripped) = full.strip_prefix(r"\\?\") {
        full = stripped.to_owned(); // chat apps don't understand verbatim paths
    }
    // DROPFILES { pFiles: u32, pt: POINT, fNC: BOOL, fWide: BOOL } then a double-null-terminated UTF-16 list.
    let mut drop = vec![0u8; 20];
    drop[0..4].copy_from_slice(&20u32.to_le_bytes());
    drop[16..20].copy_from_slice(&1u32.to_le_bytes());
    for unit in full.encode_utf16().chain([0, 0]) {
        drop.extend_from_slice(&unit.to_le_bytes());
    }

    let owner = hwnd_of(window);
    let mut opened = false;
    for _ in 0..10 {
        // Another app (clipboard managers, RDP) may hold the clipboard for a moment.
        if unsafe { OpenClipboard(owner) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    if !opened {
        return Err("The clipboard is busy in another app. Try again in a moment.".into());
    }
    let result = (|| unsafe {
        EmptyClipboard().map_err(|e| e.message().to_string())?;
        set_global(15 /* CF_HDROP */, &drop)?;
        let effect = RegisterClipboardFormatW(w!("Preferred DropEffect"));
        if effect != 0 {
            set_global(effect, &1u32.to_le_bytes())?;
        }
        Ok(())
    })();
    let _ = unsafe { CloseClipboard() };
    result
}

unsafe fn set_global(format: u32, data: &[u8]) -> Result<(), String> {
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, data.len()) }.map_err(|e| e.message().to_string())?;
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        let _ = unsafe { GlobalFree(Some(handle)) };
        return Err("Couldn't lock clipboard memory.".into());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
        let _ = GlobalUnlock(handle);
    }
    if let Err(e) = unsafe { SetClipboardData(format, Some(HANDLE(handle.0))) } {
        // Ownership only passes to the system on success.
        let _ = unsafe { GlobalFree(Some(HGLOBAL(handle.0))) };
        return Err(e.message().to_string());
    }
    Ok(())
}

fn shell_open(file: &str, params: Option<&str>) {
    let f = wide(file);
    let p = params.map(wide);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(f.as_ptr()), p.as_ref().map(|p| PCWSTR(p.as_ptr())).unwrap_or(PCWSTR::null()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Classic Sound control panel: Properties › Advanced holds the exclusive-mode switches.
pub fn open_sound_settings() {
    shell_open("control.exe", Some("mmsys.cpl"));
}

pub fn open_microphone_privacy() {
    shell_open("ms-settings:privacy-microphone", None);
}

pub fn reveal_in_folder(path: &Path) {
    shell_open("explorer.exe", Some(&format!("/select,\"{}\"", path.display())));
}

/// The Windows accent colour in WinUI's two fill shades.
pub fn accent_color() -> Option<AccentColors> {
    use windows::UI::ViewManagement::{UIColorType, UISettings};
    ensure_ui_com();
    let settings = UISettings::new().ok()?;
    let get = |t: UIColorType| settings.GetColorValue(t).ok().map(|c| slint::Color::from_argb_u8(255, c.R, c.G, c.B));
    Some(AccentColors { on_dark: get(UIColorType::AccentLight2)?, on_light: get(UIColorType::AccentDark1)? })
}

/// Dark/light title bar, coloured to match the window background so it blends in (Windows 11).
pub fn style_window(window: &slint::Window, dark: bool, caption: slint::Color) {
    let Some(hwnd) = hwnd_of(window) else { return };
    let colorref = |c: slint::Color| COLORREF(c.red() as u32 | (c.green() as u32) << 8 | (c.blue() as u32) << 16);
    unsafe {
        let dark_flag = BOOL::from(dark);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &dark_flag as *const _ as *const _, 4); // USE_IMMERSIVE_DARK_MODE
        let caption = colorref(caption);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as *const _, 4); // CAPTION_COLOR
        let text = if dark { COLORREF(0x00FF_FFFF) } else { COLORREF(0x001B_1B1B) };
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(36), &text as *const _ as *const _, 4); // TEXT_COLOR
    }
}

thread_local! {
    static TASKBAR: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
    static BADGE: RefCell<Option<HICON>> = const { RefCell::new(None) };
}

/// Red dot on the taskbar button while recording (the window is usually minimized when the
/// global shortcut is used).
pub fn set_recording_badge(window: &slint::Window, recording: bool) {
    let Some(hwnd) = hwnd_of(window) else { return };
    ensure_ui_com();
    TASKBAR.with(|t| {
        let mut t = t.borrow_mut();
        if t.is_none() {
            if let Ok(list) = unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) } {
                if unsafe { list.HrInit() }.is_ok() {
                    *t = Some(list);
                }
            }
        }
        let Some(list) = t.as_ref() else { return };
        let icon = if recording {
            BADGE.with(|b| {
                *b.borrow_mut().get_or_insert_with(|| unsafe {
                    let instance = GetModuleHandleW(None).unwrap_or_default();
                    LoadImageW(Some(instance.into()), PCWSTR(std::ptr::without_provenance::<u16>(2)) /* MAKEINTRESOURCE(2) */, IMAGE_ICON, 16, 16, LR_DEFAULTCOLOR)
                        .map(|h| HICON(h.0))
                        .unwrap_or_default()
                })
            })
        } else {
            HICON::default()
        };
        let _ = unsafe { list.SetOverlayIcon(hwnd, icon, if recording { w!("Recording") } else { PCWSTR::null() }) };
    });
}

/// Receiving the hotkey grants foreground rights, so this can bring the window forward.
pub fn bring_to_front(window: &slint::Window) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// The Windows save dialog, starting in Music. None if cancelled.
pub async fn pick_save_path(window: &slint::Window, suggested: String, extension: &str, label: &str) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new().set_file_name(format!("{suggested}.{extension}")).add_filter(label, &[extension]);
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let music = PathBuf::from(profile).join("Music");
        if music.is_dir() {
            dialog = dialog.set_directory(music);
        }
    }
    let handle = window.window_handle();
    dialog = dialog.set_parent(&handle);
    let mut path = dialog.save_file().await?.path().to_path_buf();
    if path.extension().is_none() {
        path.set_extension(extension);
    }
    Some(path)
}
