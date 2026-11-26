//! Everything that talks to the operating system. The Windows implementation is the real app; the
//! demo implementation (any other OS) simulates devices so the UI and the whole audio pipeline
//! can be run, tested and screenshotted without Windows.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    /// Render endpoints (speakers, headphones) are recorded through WASAPI loopback.
    pub is_output: bool,
    pub is_default: bool,
}

#[derive(Clone, Debug)]
pub enum DeviceEvent {
    /// Something about the endpoint list changed (added, removed, state, default, name).
    Changed,
    /// A specific endpoint was removed or is no longer active.
    Unavailable(String),
}

/// The Windows accent colour in the two shades WinUI uses for accent fills.
#[derive(Clone, Copy, Debug)]
pub struct AccentColors {
    /// AccentLight2: used on dark backgrounds.
    pub on_dark: slint::Color,
    /// AccentDark1: used on light backgrounds.
    pub on_light: slint::Color,
}

/// Scratch folder for clipboard copies. Older copies are cleaned up when a new one is made.
pub fn clipboard_temp_path(file_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("RecordThis").join("Clipboard");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path()); // fails harmlessly if a chat app is still uploading it
        }
    }
    dir.join(file_name)
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(not(windows))]
mod demo;
#[cfg(not(windows))]
pub use self::demo::*;
