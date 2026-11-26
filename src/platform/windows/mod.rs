//! Windows implementation: WASAPI capture/loopback and render, MMDevice enumeration and hot-plug,
//! Media Foundation MP3 encoding, and the shell/window integration (hotkey, sleep, clipboard,
//! taskbar badge, accent colour, title bar).

mod audio;
mod devices;
mod mp3;
mod shell;

pub use audio::{CaptureStream, RenderStream};
pub use devices::{list_devices, mix_format, DeviceWatcher};
pub use mp3::Mp3Writer;
pub use shell::*;

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED};

/// COM for a worker thread (multithreaded apartment), released when dropped.
pub struct ThreadGuard(bool);

pub fn init_thread() -> ThreadGuard {
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    ThreadGuard(hr.is_ok())
}

impl Drop for ThreadGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Makes sure COM is usable on the UI thread (winit normally initializes OLE already). Kept for
/// the life of the process.
pub(crate) fn ensure_ui_com() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    DONE.with(|done| {
        if !done.get() {
            // S_FALSE / RPC_E_CHANGED_MODE both mean COM is already up on this thread.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            done.set(true);
        }
    });
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
