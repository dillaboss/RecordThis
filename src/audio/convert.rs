//! Decoding raw interleaved spool bytes (any supported mix format) into 32-bit float, and peak
//! measurement for the live meter.

use super::format::{CaptureFormat, SampleEncoding};

#[inline]
fn i24(b: &[u8]) -> i32 {
    ((b[0] as i32) << 8 | (b[1] as i32) << 16 | (b[2] as i32) << 24) >> 8
}

/// Decodes `source` (whole frames) into `dest`. Returns the number of samples written.
pub fn to_float(source: &[u8], dest: &mut [f32], format: &CaptureFormat) -> usize {
    let bps = format.bytes_per_sample();
    let count = (source.len() / bps).min(dest.len());
    let src = &source[..count * bps];
    let dst = &mut dest[..count];
    match (format.encoding, format.bits_per_sample) {
        (SampleEncoding::IeeeFloat, 32) => {
            for (d, c) in dst.iter_mut().zip(src.chunks_exact(4)) {
                let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                *d = if v.is_finite() { v } else { 0.0 };
            }
        }
        (SampleEncoding::Pcm, 16) => {
            for (d, c) in dst.iter_mut().zip(src.chunks_exact(2)) {
                *d = i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0;
            }
        }
        (SampleEncoding::Pcm, 24) => {
            for (d, c) in dst.iter_mut().zip(src.chunks_exact(3)) {
                *d = i24(c) as f32 / 8_388_608.0;
            }
        }
        (SampleEncoding::Pcm, 32) => {
            for (d, c) in dst.iter_mut().zip(src.chunks_exact(4)) {
                *d = i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0;
            }
        }
        (SampleEncoding::Pcm, 8) => {
            for (d, &b) in dst.iter_mut().zip(src.iter()) {
                *d = (b as f32 - 128.0) / 128.0;
            }
        }
        _ => dst.fill(0.0),
    }
    count
}

/// Absolute peak magnitude of one sample.
#[inline]
fn magnitude(c: &[u8], format: &CaptureFormat) -> f32 {
    match (format.encoding, format.bits_per_sample) {
        (SampleEncoding::IeeeFloat, 32) => {
            let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs();
            if v.is_finite() { v } else { 0.0 }
        }
        (SampleEncoding::Pcm, 16) => (i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).abs(),
        (SampleEncoding::Pcm, 24) => (i24(c) as f32 / 8_388_608.0).abs(),
        (SampleEncoding::Pcm, 32) => (i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0).abs(),
        (SampleEncoding::Pcm, 8) => ((c[0] as f32 - 128.0) / 128.0).abs(),
        _ => 0.0,
    }
}

/// Which meter bar each input channel feeds: (left, right).
pub fn meter_sides(channels: u16, channel_mask: u32) -> Vec<(bool, bool)> {
    if channels == 1 {
        return vec![(true, true)];
    }
    let matrix = super::downmix::build_matrix(channels, channel_mask);
    (0..channels as usize).map(|c| (matrix[0][c] > 0.0, matrix[1][c] > 0.0)).collect()
}

/// Left/right peak (0..1, clamped) of a raw capture packet, for the live meter.
pub fn side_peaks(source: &[u8], format: &CaptureFormat, sides: &[(bool, bool)]) -> (f32, f32) {
    let bps = format.bytes_per_sample();
    let block = format.block_align();
    let (mut left, mut right) = (0f32, 0f32);
    for frame in source.chunks_exact(block) {
        for (c, sample) in frame.chunks_exact(bps).enumerate() {
            let m = magnitude(sample, format);
            let (l, r) = sides.get(c).copied().unwrap_or((true, true));
            if l && m > left {
                left = m;
            }
            if r && m > right {
                right = m;
            }
        }
    }
    (left.min(1.0), right.min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_every_format() {
        let f = CaptureFormat::new(48000, 1, 24, SampleEncoding::Pcm, 0).unwrap();
        let bytes = [0x00, 0x00, 0x80, 0xff, 0xff, 0x7f]; // -1.0, ~+1.0
        let mut out = [0f32; 2];
        assert_eq!(to_float(&bytes, &mut out, &f), 2);
        assert_eq!(out[0], -1.0);
        assert!((out[1] - 1.0).abs() < 1e-6);

        let f = CaptureFormat::new(48000, 1, 8, SampleEncoding::Pcm, 0).unwrap();
        let mut out = [0f32; 2];
        to_float(&[0x80, 0x00], &mut out, &f);
        assert_eq!(out, [0.0, -1.0]);

        let f = CaptureFormat::new(48000, 1, 32, SampleEncoding::IeeeFloat, 0).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0.5f32.to_le_bytes());
        bytes.extend_from_slice(&f32::NAN.to_le_bytes());
        let mut out = [1f32; 2];
        to_float(&bytes, &mut out, &f);
        assert_eq!(out, [0.5, 0.0]);
    }

    #[test]
    fn side_peaks_follow_channel_layout() {
        let f = CaptureFormat::new(48000, 2, 16, SampleEncoding::Pcm, 3).unwrap();
        let mut bytes = Vec::new();
        for (l, r) in [(16384i16, -8192i16), (0, 0)] {
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
        }
        let sides = meter_sides(2, 3);
        let (l, r) = side_peaks(&bytes, &f, &sides);
        assert!((l - 0.5).abs() < 1e-4 && (r - 0.25).abs() < 1e-4);
    }
}
