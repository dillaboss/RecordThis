//! Scratch files in %LOCALAPPDATA%\Temp: `RecordThis_{guid}.raw` (raw PCM in the device's mix
//! format) plus a small `.json` sidecar with the format. Together they are enough to rebuild the
//! audio after a crash, power loss or forced shutdown. The naming and sidecar schema match the
//! C# version, so its leftovers are recoverable too.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audio::format::CaptureFormat;

pub const FILE_PREFIX: &str = "RecordThis_";
const METADATA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct SpoolSession {
    pub id: Uuid,
    pub raw_path: PathBuf,
    pub metadata_path: PathBuf,
    pub format: CaptureFormat,
    pub device_name: String,
    pub started_at: DateTime<FixedOffset>,
}

impl SpoolSession {
    /// Whole frames on disk (a trailing partial frame is ignored).
    pub fn frame_count(&self) -> u64 {
        fs::metadata(&self.raw_path).map(|m| m.len() / self.format.block_align() as u64).unwrap_or(0)
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frame_count() as f64 / self.format.sample_rate as f64)
    }

    /// "RecordThis 2026-11-24 14.08.31"
    pub fn base_file_name(&self) -> String {
        format!("RecordThis {}", self.started_at.with_timezone(&Local).format("%Y-%m-%d %H.%M.%S"))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SpoolMetadata {
    version: u32,
    id: Uuid,
    device_name: String,
    started_at: DateTime<FixedOffset>,
    format: CaptureFormat,
}

pub struct SpoolManager {
    dir: PathBuf,
}

impl Default for SpoolManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SpoolManager {
    pub fn new() -> Self {
        let dir = default_spool_dir();
        let _ = fs::create_dir_all(&dir);
        Self { dir }
    }

    #[cfg(test)]
    pub fn in_dir(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    /// Writes the metadata sidecar first, so any raw file on disk is always interpretable.
    pub fn create_session(&self, format: CaptureFormat, device_name: &str) -> io::Result<SpoolSession> {
        let id = Uuid::new_v4();
        let raw_path = self.dir.join(format!("{FILE_PREFIX}{}.raw", id.simple()));
        let session = SpoolSession {
            id,
            metadata_path: raw_path.with_extension("json"),
            raw_path,
            format,
            device_name: device_name.to_owned(),
            started_at: Local::now().fixed_offset(),
        };
        let meta = SpoolMetadata {
            version: METADATA_VERSION,
            id,
            device_name: session.device_name.clone(),
            started_at: session.started_at,
            format,
        };
        fs::write(&session.metadata_path, serde_json::to_vec_pretty(&meta)?)?;
        Ok(session)
    }

    /// Spools left behind by a previous run, newest first. Files still open in another running
    /// instance are skipped; empty ones are deleted.
    pub fn find_orphans(&self) -> Vec<SpoolSession> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(FILE_PREFIX) || path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(session) = read_session(&path) else { continue };
            if !session.raw_path.exists() {
                let _ = fs::remove_file(&path);
                continue;
            }
            if is_in_use(&session.raw_path) {
                continue;
            }
            if session.frame_count() == 0 {
                self.delete(&session);
                continue;
            }
            sessions.push(session);
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        sessions
    }

    pub fn delete(&self, session: &SpoolSession) {
        let _ = fs::remove_file(&session.raw_path);
        let _ = fs::remove_file(&session.metadata_path);
    }
}

fn read_session(metadata_path: &Path) -> Option<SpoolSession> {
    let meta: SpoolMetadata = serde_json::from_slice(&fs::read(metadata_path).ok()?).ok()?;
    if meta.format.block_align() == 0 || meta.format.sample_rate == 0 {
        return None;
    }
    Some(SpoolSession {
        id: meta.id,
        raw_path: metadata_path.with_extension("raw"),
        metadata_path: metadata_path.to_owned(),
        format: meta.format,
        device_name: meta.device_name,
        started_at: meta.started_at,
    })
}

fn default_spool_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("Temp");
    }
    std::env::temp_dir()
}

#[cfg(windows)]
mod share {
    pub const NONE: u32 = 0;
    pub const READ: u32 = 0x1;
    pub const WRITE: u32 = 0x2;
    pub const DELETE: u32 = 0x4;
}

/// The capture thread's handle. Exclusive, so other instances' orphan scans see it as in use.
pub fn open_for_writing(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut o, share::NONE);
    o.open(path)
}

/// A reader that never gets in the way of deletes or other readers.
pub fn open_shared_read(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut o, share::READ | share::WRITE | share::DELETE);
    o.open(path)
}

/// Keeps a spool marked "in use" while it's being reviewed, so a second running instance won't
/// offer it for recovery. Shared readers and deletes still work.
pub fn hold(path: &Path) -> Option<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut o, share::READ | share::DELETE);
    o.open(path).ok()
}

fn is_in_use(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut o = OpenOptions::new();
        o.read(true);
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut o, share::NONE);
        o.open(path).is_err()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::format::SampleEncoding;
    use std::io::Write;

    #[test]
    fn round_trips_and_recovers_csharp_sidecars() {
        let dir = std::env::temp_dir().join(format!("rt-spool-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let m = SpoolManager::in_dir(dir.clone());
        let f = CaptureFormat::new(48000, 2, 32, SampleEncoding::IeeeFloat, 3).unwrap();
        let s = m.create_session(f, "Speakers").unwrap();
        open_for_writing(&s.raw_path).unwrap().write_all(&[0u8; 80]).unwrap();

        // A sidecar exactly as System.Text.Json wrote it in the C# version.
        let id = "0b9d2c1e-3a4f-4b5c-8d6e-7f8091a2b3c4";
        let csharp = format!(
            r#"{{"Version":1,"Id":"{id}","DeviceName":"Old mic","StartedAt":"2026-10-05T14:16:55.1234567-04:00","Format":{{"SampleRate":44100,"Channels":1,"BitsPerSample":16,"Encoding":1,"ChannelMask":0}}}}"#
        );
        let old = dir.join(format!("{FILE_PREFIX}{}.json", id.replace('-', "")));
        fs::write(&old, csharp).unwrap();
        fs::write(old.with_extension("raw"), [0u8; 10]).unwrap();

        let orphans = m.find_orphans();
        assert_eq!(orphans.len(), 2);
        assert_eq!(orphans[0].device_name, "Speakers"); // newest first
        assert_eq!(orphans[0].frame_count(), 10);
        assert_eq!(orphans[1].format.encoding, SampleEncoding::Pcm);
        assert_eq!(orphans[1].frame_count(), 5);
        for o in &orphans {
            m.delete(o);
        }
        assert!(m.find_orphans().is_empty());
        fs::remove_dir_all(dir).ok();
    }
}
