// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(e) = recordthis::app::run() {
        let message = format!("RecordThis couldn't start: {e}");
        eprintln!("{message}");
        #[cfg(windows)]
        show_error(&message);
        std::process::exit(1);
    }
}

/// Release builds have no console, so startup failures need a dialog.
#[cfg(windows)]
fn show_error(message: &str) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let text: Vec<u16> = message.encode_utf16().chain([0]).collect();
    unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), w!("RecordThis"), MB_ICONERROR | MB_OK) };
}
