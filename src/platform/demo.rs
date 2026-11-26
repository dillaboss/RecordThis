//! Simulated audio devices for running the app on non-Windows systems (development, tests and
//! screenshots). Loopback devices go silent between phrases and deliver no packets while silent,
//! exactly like WASAPI loopback, so the silence watchdog gets exercised.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::format::{CaptureFormat, SampleEncoding};
use crate::errors::{AudioError, ExportError};

use super::{DeviceEvent, DeviceInfo};

pub struct ThreadGuard;

pub fn init_thread() -> ThreadGuard {
    ThreadGuard
}

pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let d = |id: &str, name: &str, is_output, is_default| DeviceInfo { id: id.into(), name: name.into(), is_output, is_default };
    Ok(vec![
        d("demo-out-1", "Speakers (Demo Audio)", true, true),
        d("demo-out-2", "Headphones (Demo Audio)", true, false),
        d("demo-mic", "Microphone (Demo Audio)", false, true),
    ])
}

pub fn mix_format(id: &str) -> Option<CaptureFormat> {
    Some(match id {
        "demo-out-2" => CaptureFormat::new(44100, 2, 32, SampleEncoding::IeeeFloat, 3).ok()?,
        "demo-mic" => CaptureFormat::new(48000, 1, 16, SampleEncoding::Pcm, 4).ok()?,
        _ => CaptureFormat::new(48000, 2, 32, SampleEncoding::IeeeFloat, 3).ok()?,
    })
}

pub struct DeviceWatcher;

impl DeviceWatcher {
    pub fn new(_on_event: Arc<dyn Fn(DeviceEvent) + Send + Sync>) -> Option<Self> {
        Some(DeviceWatcher)
    }
}

pub struct CaptureStream {
    format: CaptureFormat,
    loopback: bool,
    started: Option<Instant>,
    produced: u64,
    noise: u32,
}

impl CaptureStream {
    pub fn open(device: &DeviceInfo) -> Result<Self, AudioError> {
        let format = mix_format(&device.id).ok_or_else(|| AudioError::hr(crate::errors::E_NOTFOUND, "Device not found"))?;
        Ok(Self { format, loopback: device.is_output, started: None, produced: 0, noise: 0x1234_5678 })
    }

    pub fn format(&self) -> CaptureFormat {
        self.format
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        self.started = Some(Instant::now());
        Ok(())
    }

    fn sample(&mut self, frame: u64) -> f32 {
        let rate = self.format.sample_rate as f32;
        let t = frame as f32 / rate;
        if self.loopback {
            // Phrases of notes, 2.6 s on, 1.4 s off.
            let cycle = t % 4.0;
            if cycle > 2.6 {
                return 0.0;
            }
            let notes = [261.6f32, 329.6, 392.0, 523.3, 392.0, 329.6];
            let i = ((cycle / 0.43) as usize).min(notes.len() - 1);
            let local = cycle % 0.43;
            let env = (local * 30.0).min(1.0) * (-local * 4.0).exp();
            0.55 * env * ((t * notes[i] * std::f32::consts::TAU).sin() + 0.3 * (t * notes[i] * 2.0 * std::f32::consts::TAU).sin())
        } else {
            // Speech-like bursts over a quiet noise floor.
            self.noise ^= self.noise << 13;
            self.noise ^= self.noise >> 17;
            self.noise ^= self.noise << 5;
            let n = (self.noise as f32 / u32::MAX as f32) - 0.5;
            let syllable = ((t * 3.1).sin() * (t * 0.7).sin()).max(0.0);
            0.004 * n + 0.45 * syllable * ((t * 180.0 * std::f32::consts::TAU).sin() * 0.6 + n * 0.4)
        }
    }

    pub fn read(&mut self, out: &mut Vec<u8>, timeout: Duration) -> Result<(), AudioError> {
        std::thread::sleep(timeout);
        let Some(start) = self.started else { return Ok(()) };
        let due = (start.elapsed().as_secs_f64() * self.format.sample_rate as f64) as u64;
        while self.produced < due {
            let frame = self.produced;
            let v = self.sample(frame);
            self.produced += 1;
            // Loopback delivers nothing at all while the endpoint is silent.
            if self.loopback && v == 0.0 {
                continue;
            }
            for _ in 0..self.format.channels {
                match self.format.encoding {
                    SampleEncoding::IeeeFloat => out.extend_from_slice(&v.to_le_bytes()),
                    SampleEncoding::Pcm => out.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes()),
                }
            }
        }
        Ok(())
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.started = None; // mirrors IAudioClient::Stop
    }
}

/// A fake output device that consumes audio in real time.
pub struct RenderStream {
    rate: u32,
    channels: usize,
    queued: f64,
    last: Option<Instant>,
}

impl RenderStream {
    pub fn open(sample_rate: u32, channels: u16) -> Result<Self, AudioError> {
        Ok(Self { rate: sample_rate, channels: channels.max(1) as usize, queued: 0.0, last: None })
    }

    fn drain(&mut self) {
        if let Some(last) = self.last {
            self.queued = (self.queued - last.elapsed().as_secs_f64() * self.rate as f64).max(0.0);
            self.last = Some(Instant::now());
        }
    }

    pub fn buffer_frames(&self) -> usize {
        self.rate as usize / 10
    }

    pub fn padding(&self) -> Result<usize, AudioError> {
        let mut q = self.queued;
        if let Some(last) = self.last {
            q = (q - last.elapsed().as_secs_f64() * self.rate as f64).max(0.0);
        }
        Ok(q as usize)
    }

    pub fn wait(&self, timeout: Duration) {
        std::thread::sleep(timeout.min(Duration::from_millis(10)));
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<(), AudioError> {
        self.drain();
        self.queued += (samples.len() / self.channels) as f64;
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        self.last = Some(Instant::now());
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), AudioError> {
        self.drain();
        self.last = None;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), AudioError> {
        self.queued = 0.0;
        Ok(())
    }
}

/// No MP3 encoder outside Windows: exercises the "WAV instead" fallback.
pub struct Mp3Writer;

impl Mp3Writer {
    pub fn create(_path: &Path, _rate: u32, _channels: u16, _kbps: u32) -> Result<Self, ExportError> {
        Err(ExportError::Mp3Unavailable)
    }

    pub fn write(&mut self, _pcm: &[i16]) -> Result<(), ExportError> {
        Ok(())
    }

    pub fn finish(self) -> Result<(), ExportError> {
        Ok(())
    }
}

pub struct SystemHooks;

impl SystemHooks {
    pub fn install(_on_hotkey: Box<dyn Fn() + Send>, _on_suspend: Box<dyn Fn() + Send>) -> Self {
        SystemHooks
    }

    pub fn hotkey_registered(&self) -> bool {
        true
    }
}

pub fn copy_file_to_clipboard(_window: &slint::Window, path: &Path) -> Result<(), String> {
    eprintln!("[demo] clipboard ← {}", path.display());
    Ok(())
}

pub fn open_sound_settings() {
    eprintln!("[demo] open Sound settings");
}

pub fn open_microphone_privacy() {
    eprintln!("[demo] open microphone privacy settings");
}

pub fn reveal_in_folder(path: &Path) {
    eprintln!("[demo] reveal {}", path.display());
}

pub fn accent_color() -> Option<super::AccentColors> {
    None
}

pub fn style_window(_window: &slint::Window, _dark: bool, _caption: slint::Color) {}

pub fn set_recording_badge(_window: &slint::Window, _recording: bool) {}

pub fn bring_to_front(_window: &slint::Window) {}

pub async fn pick_save_path(_window: &slint::Window, suggested: String, extension: &str, _label: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("RecordThis-demo-saves");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join(format!("{suggested}.{extension}")))
}
