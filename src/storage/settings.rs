//! Choices remembered between launches, in %LOCALAPPDATA%\RecordThis\settings.json (same file and
//! keys as the C# version). Failures are ignored; settings are a convenience.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppSettings {
    /// "mp3" or "wav". Applies to both Copy and Save.
    pub export_format: String,
    pub bit_depth: u16,
    pub mp3_bit_rate: u32,
    /// True once the last share action was Copy; makes Copy the highlighted button.
    pub prefer_copy: bool,
    /// "Output" (system audio) or "Input" (microphones).
    pub source_kind: String,
    pub last_output_device_id: Option<String>,
    pub last_input_device_id: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            export_format: "mp3".into(),
            bit_depth: 24,
            mp3_bit_rate: 192,
            prefer_copy: true,
            source_kind: "Output".into(),
            last_output_device_id: None,
            last_input_device_id: None,
        }
    }
}

pub struct Settings {
    path: Option<PathBuf>,
    pub current: AppSettings,
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_dir().map(|d| d.join("settings.json"));
        let current = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, current }
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(&self.current) {
            let _ = std::fs::write(path, json);
        }
    }
}

fn settings_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("RecordThis"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".config").join("recordthis"))
    }
}
