//! Finalization: spool (trimmed) → BS.775 downmix → clip-safe gain → sinc resampler → either a
//! 16-bit TPDF-dithered / 24-bit WAV, or Windows' Media Foundation MP3 encoder. Everything streams
//! from disk. Output goes to a temporary file that is renamed when complete, so a failed or
//! cancelled export never leaves a truncated file behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::downmix::{clip_safe_gain, Downmix};
use super::reader::{FrameSource, SpoolReader};
use super::resample::ExactLengthResampler;
use super::wav::{PcmEncoder, WavWriter, MAX_DATA_BYTES};
use crate::errors::ExportError;
use crate::platform::Mp3Writer;
use crate::storage::spool::SpoolSession;

const FRAMES_PER_CHUNK: usize = 16384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Mp3,
    Wav,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Mp3 => "mp3",
            ExportFormat::Wav => "wav",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Mp3 => "MP3",
            ExportFormat::Wav => "WAV",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// 44100 or 48000.
    pub sample_rate: u32,
    /// WAV only: 16 (dithered) or 24.
    pub bit_depth: u16,
    /// MP3 only: 128, 192 or 320.
    pub mp3_kbps: u32,
    /// Trim range in source frames, end exclusive.
    pub start_frame: u64,
    pub end_frame: u64,
}

#[derive(Clone, Debug)]
pub struct ExportResult {
    pub path: PathBuf,
    pub duration: Duration,
    pub downmixed: bool,
    pub file_size: u64,
}

pub fn export(
    session: &SpoolSession,
    mixed_peak: f32,
    options: &ExportOptions,
    destination: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> Result<ExportResult, ExportError> {
    let mut reader = SpoolReader::open(&session.raw_path, session.format)?;
    let total = reader.total_frames();
    let start = options.start_frame.min(total);
    let end = options.end_frame.clamp(start, total);
    reader.set_position(start);
    reader.set_end_limit(end);

    let mut downmix = Downmix::new(reader, session.format.channel_mask);
    downmix.gain = clip_safe_gain(mixed_peak);
    let downmixed = downmix.is_downmixing();
    let mut chain = ExactLengthResampler::new(downmix, options.sample_rate, end - start);
    let expected = chain.expected_frames();
    let channels = chain.channels();

    let file_name = destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let partial = destination.with_file_name(format!(".{file_name}.partial.{}", options.format.extension()));

    let result = match options.format {
        ExportFormat::Wav => write_wav(&mut chain, options, expected, channels, &partial, progress, cancel),
        ExportFormat::Mp3 => write_mp3(&mut chain, options, expected, channels, &partial, progress, cancel),
    }
    // rename() replaces an existing file atomically (MoveFileExW + REPLACE_EXISTING on Windows).
    .and_then(|_| std::fs::rename(&partial, destination).map_err(ExportError::from));

    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }

    progress(1.0);
    Ok(ExportResult {
        path: destination.to_owned(),
        duration: Duration::from_secs_f64(expected as f64 / options.sample_rate as f64),
        downmixed,
        file_size: std::fs::metadata(destination).map(|m| m.len()).unwrap_or(0),
    })
}

fn report(progress: &mut dyn FnMut(f64), last: &mut i32, done: u64, expected: u64) {
    let percent = (done * 100 / expected.max(1)) as i32;
    if percent != *last {
        *last = percent;
        progress(percent as f64 / 100.0);
    }
}

fn write_wav(
    chain: &mut dyn FrameSource,
    options: &ExportOptions,
    expected: u64,
    channels: u16,
    path: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> Result<(), ExportError> {
    let bits = options.bit_depth;
    if bits != 16 && bits != 24 {
        return Err(ExportError::Other("Bit depth must be 16 or 24.".into()));
    }
    if expected * channels as u64 * (bits as u64 / 8) > MAX_DATA_BYTES {
        return Err(ExportError::TooLongForWav);
    }

    let mut writer = WavWriter::create(path, options.sample_rate, channels, bits)?;
    let mut encoder = PcmEncoder::new(bits);
    let mut floats = vec![0f32; FRAMES_PER_CHUNK * channels as usize];
    let mut bytes = Vec::with_capacity(floats.len() * 3);
    let (mut done, mut last) = (0u64, -1);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let n = chain.read(&mut floats)?;
        if n == 0 {
            break;
        }
        encoder.encode_bytes(&floats[..n * channels as usize], &mut bytes);
        writer.write(&bytes)?;
        done += n as u64;
        report(progress, &mut last, done, expected);
    }
    writer.finish()?;
    Ok(())
}

fn write_mp3(
    chain: &mut dyn FrameSource,
    options: &ExportOptions,
    expected: u64,
    channels: u16,
    path: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> Result<(), ExportError> {
    let mut writer = Mp3Writer::create(path, options.sample_rate, channels, options.mp3_kbps)?;
    let mut encoder = PcmEncoder::new(16);
    let mut floats = vec![0f32; FRAMES_PER_CHUNK * channels as usize];
    let mut pcm = Vec::with_capacity(floats.len());
    let (mut done, mut last) = (0u64, -1);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let n = chain.read(&mut floats)?;
        if n == 0 {
            break;
        }
        encoder.encode16(&floats[..n * channels as usize], &mut pcm);
        writer.write(&pcm)?;
        done += n as u64;
        report(progress, &mut last, done, expected);
    }
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::format::{CaptureFormat, SampleEncoding};
    use crate::storage::spool::{open_for_writing, SpoolManager};
    use std::io::Write;

    #[test]
    fn exports_a_trimmed_downmixed_resampled_wav() {
        let dir = std::env::temp_dir().join(format!("rt-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spools = SpoolManager::in_dir(dir.clone());
        // 1 s of 5.1 float at 48 kHz with a loud centre channel (forces clip-safe gain).
        let fmt = CaptureFormat::new(48000, 6, 32, SampleEncoding::IeeeFloat, 0x3F).unwrap();
        let session = spools.create_session(fmt, "Test").unwrap();
        let mut f = open_for_writing(&session.raw_path).unwrap();
        let mut bytes = Vec::new();
        for i in 0..48000 {
            let v = ((i as f32 / 48000.0) * 440.0 * std::f32::consts::TAU).sin();
            for c in 0..6 {
                let s = if c == 0 || c == 2 { v } else { 0.0 };
                bytes.extend_from_slice(&s.to_le_bytes());
            }
        }
        f.write_all(&bytes).unwrap();
        drop(f);

        let reader = SpoolReader::open(&session.raw_path, fmt).unwrap();
        let wave = crate::audio::waveform::analyze(reader, fmt.channel_mask, 100, &AtomicBool::new(false)).unwrap().unwrap();
        assert!(wave.mixed_peak > 1.6, "L + 0.707·C should exceed full scale: {}", wave.mixed_peak);

        let opts = ExportOptions { format: ExportFormat::Wav, sample_rate: 44100, bit_depth: 16, mp3_kbps: 192, start_frame: 12000, end_frame: 36000 };
        let dest = dir.join("out.wav");
        let mut calls = 0;
        let r = export(&session, wave.mixed_peak, &opts, &dest, &mut |_| calls += 1, &AtomicBool::new(false)).unwrap();
        assert!(r.downmixed);
        let data = std::fs::read(&dest).unwrap();
        let frames = (data.len() - 44) / 4;
        assert_eq!(frames, (24000.0f64 * 44100.0 / 48000.0).round() as usize);
        let max = data[44..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs()).max().unwrap();
        assert!(max <= 32767 && max > 31000, "peak should land just under full scale, got {max}");
        assert!(calls > 2);
        assert!(!dir.join(".out.wav.partial.wav").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
