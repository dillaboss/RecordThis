//! One streaming pass over the spool that yields the review waveform, the post-downmix peak and
//! where sound starts and ends (for "Trim silence"). Allocates only the bucket array plus one
//! read buffer.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use super::downmix::Downmix;
use super::reader::{FrameSource, SpoolReader};

/// −50 dBFS: quieter than this counts as dead air.
pub const SILENCE_THRESHOLD: f32 = 0.003_16;

#[derive(Clone, Debug, Default)]
pub struct WaveformData {
    /// Per-bucket absolute peak of the downmixed signal (may exceed 1.0 after a surround fold-down).
    pub peaks: Vec<f32>,
    /// Highest absolute sample of the downmixed signal; drives the clip-safe export gain.
    pub mixed_peak: f32,
    pub total_frames: u64,
    /// First and last frame above the silence threshold; None if the recording is silent.
    pub sound: Option<(u64, u64)>,
}

pub fn analyze(reader: SpoolReader, channel_mask: u32, buckets: usize, cancel: &AtomicBool) -> io::Result<Option<WaveformData>> {
    let total = reader.total_frames();
    let mut peaks = vec![0f32; buckets.max(1)];
    if total == 0 {
        return Ok(Some(WaveformData { peaks, ..Default::default() }));
    }

    let mut mix = Downmix::new(reader, channel_mask);
    let ch = mix.channels() as usize;
    let mut buf = vec![0f32; 8192 * ch];
    let mut frame = 0u64;
    let mut mixed_peak = 0f32;
    let mut first = None;
    let mut last = 0u64;
    let n_buckets = peaks.len() as u64;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = mix.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for f in 0..n {
            let mut m = 0f32;
            for c in 0..ch {
                m = m.max(buf[f * ch + c].abs());
            }
            let b = (frame * n_buckets / total) as usize;
            if m > peaks[b] {
                peaks[b] = m;
            }
            mixed_peak = mixed_peak.max(m);
            if m > SILENCE_THRESHOLD {
                first.get_or_insert(frame);
                last = frame;
            }
            frame += 1;
        }
    }

    Ok(Some(WaveformData { peaks, mixed_peak, total_frames: total, sound: first.map(|f| (f, last)) }))
}

/// Builds an SVG path (viewbox `width` × 200, centred on y = 100) for a symmetric peak envelope.
/// Silence still shows as a hairline.
pub fn envelope_path(peaks: &[f32], gain: f32) -> String {
    use std::fmt::Write;
    let n = peaks.len().max(1);
    let half = 100.0f32;
    let mut s = String::with_capacity(n * 24);
    let y = |p: f32| (p * gain).clamp(0.012, 1.0) * half;
    let _ = write!(s, "M 0 {:.2}", half - y(peaks.first().copied().unwrap_or(0.0)));
    for (i, &p) in peaks.iter().enumerate() {
        let h = y(p);
        let _ = write!(s, " L {} {:.2} L {} {:.2}", i, half - h, i + 1, half - h);
    }
    for (i, &p) in peaks.iter().enumerate().rev() {
        let h = y(p);
        let _ = write!(s, " L {} {:.2} L {} {:.2}", i + 1, half + h, i, half + h);
    }
    s.push_str(" Z");
    s
}
