//! Audio errors carry the Windows HRESULT when there is one, so they can be turned into messages
//! that tell the user what to do next.

use std::fmt;

use crate::notice::{Notice, NoticeAction, Severity};

pub const AUDCLNT_E_DEVICE_INVALIDATED: i32 = 0x8889_0004u32 as i32;
pub const AUDCLNT_E_UNSUPPORTED_FORMAT: i32 = 0x8889_0008u32 as i32;
pub const AUDCLNT_E_DEVICE_IN_USE: i32 = 0x8889_000Au32 as i32;
pub const AUDCLNT_E_SERVICE_NOT_RUNNING: i32 = 0x8889_0010u32 as i32;
pub const AUDCLNT_E_EXCLUSIVE_MODE_ONLY: i32 = 0x8889_0012u32 as i32;
pub const E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;
pub const E_NOTFOUND: i32 = 0x8007_0490u32 as i32;
pub const E_DEVICE_NOT_CONNECTED: i32 = 0x8007_048Fu32 as i32;

#[derive(Clone, Debug)]
pub struct AudioError {
    pub hresult: Option<i32>,
    pub message: String,
}

impl AudioError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { hresult: None, message: message.into() }
    }

    pub fn hr(hresult: i32, message: impl Into<String>) -> Self {
        Self { hresult: Some(hresult), message: message.into() }
    }

    pub fn is_device_loss(&self) -> bool {
        matches!(self.hresult, Some(AUDCLNT_E_DEVICE_INVALIDATED | E_NOTFOUND | E_DEVICE_NOT_CONNECTED))
    }

    /// A sentence for the user.
    pub fn describe(&self) -> String {
        let m = self.message.trim();
        if !m.is_empty() {
            let m = m.trim_end_matches(['\r', '\n']).to_string();
            if m.ends_with('.') { m } else { format!("{m}.") }
        } else if let Some(hr) = self.hresult {
            format!("Error 0x{:08X}.", hr as u32)
        } else {
            "An unknown error occurred.".into()
        }
    }

    /// What to show when recording (or the level preview) can't start on `device_name`.
    pub fn start_notice(&self, device_name: &str, is_microphone: bool) -> Notice {
        match self.hresult {
            Some(AUDCLNT_E_EXCLUSIVE_MODE_ONLY | AUDCLNT_E_DEVICE_IN_USE) => Notice::new(
                "Another app has exclusive control of this device",
                format!("{device_name} is locked in exclusive mode. Close the app using it, or in Sound settings open the device's Properties > Advanced and clear \"Allow applications to take exclusive control of this device\"."),
                Severity::Error,
            )
            .with_action(NoticeAction::OpenSoundSettings, "Open Sound settings"),
            Some(E_ACCESSDENIED) if is_microphone => Notice::new(
                "Windows is blocking microphone access",
                "Turn on \"Let desktop apps access your microphone\" in Privacy & security settings, then try again.",
                Severity::Error,
            )
            .with_action(NoticeAction::OpenMicrophonePrivacy, "Open privacy settings"),
            _ if self.is_device_loss() => Notice::new(
                "Device unavailable",
                format!("{device_name} was unplugged or disabled. Choose another source."),
                Severity::Warning,
            ),
            Some(AUDCLNT_E_UNSUPPORTED_FORMAT) => Notice::new(
                "This device's audio format isn't supported",
                "Open Sound settings and change the device's default format (for example to 48000 Hz, 24-bit), then try again.",
                Severity::Error,
            )
            .with_action(NoticeAction::OpenSoundSettings, "Open Sound settings"),
            Some(AUDCLNT_E_SERVICE_NOT_RUNNING) => Notice::new(
                "Windows Audio isn't running",
                "Restart the Windows Audio service, or restart your PC, then try again.",
                Severity::Error,
            ),
            _ => Notice::new("Recording couldn't start", self.describe(), Severity::Error),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        let message = match e.kind() {
            std::io::ErrorKind::StorageFull => "The disk is full".to_string(),
            _ => e.to_string(),
        };
        Self { hresult: e.raw_os_error().map(|c| if c > 0 { (0x8007_0000u32 | (c as u32 & 0xFFFF)) as i32 } else { c }), message }
    }
}

#[cfg(windows)]
impl From<windows_core::Error> for AudioError {
    fn from(e: windows_core::Error) -> Self {
        let hr = e.code().0;
        let known = match hr {
            AUDCLNT_E_DEVICE_INVALIDATED => "The audio device was disconnected",
            AUDCLNT_E_UNSUPPORTED_FORMAT => "The audio device doesn't support this format",
            AUDCLNT_E_DEVICE_IN_USE | AUDCLNT_E_EXCLUSIVE_MODE_ONLY => "The audio device is in use by another app in exclusive mode",
            AUDCLNT_E_SERVICE_NOT_RUNNING => "Windows Audio isn't running",
            E_ACCESSDENIED => "Access to the device was denied",
            _ => "",
        };
        let message = if known.is_empty() { e.message().to_string() } else { known.to_string() };
        Self::hr(hr, message)
    }
}

/// Why an export failed.
#[derive(Debug)]
pub enum ExportError {
    /// This copy of Windows has no MP3 encoder (e.g. an "N" edition without the Media Feature Pack).
    Mp3Unavailable,
    TooLongForWav,
    Cancelled,
    PermissionDenied,
    Other(String),
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::PermissionDenied => ExportError::PermissionDenied,
            std::io::ErrorKind::StorageFull => ExportError::Other("The disk is full.".into()),
            _ => ExportError::Other(AudioError::from(e).describe()),
        }
    }
}

#[cfg(windows)]
impl From<windows_core::Error> for ExportError {
    fn from(e: windows_core::Error) -> Self {
        if e.code().0 == E_ACCESSDENIED {
            ExportError::PermissionDenied
        } else {
            ExportError::Other(AudioError::from(e).describe())
        }
    }
}
