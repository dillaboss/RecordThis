//! Float → integer PCM (TPDF-dithered 16-bit, or 24-bit) and a minimal streaming RIFF/WAV writer.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// RIFF sizes are 32-bit.
pub const MAX_DATA_BYTES: u64 = u32::MAX as u64 - 1024;

/// Float → little-endian integer PCM. 16-bit output gets ±1 LSB triangular dither, which
/// decorrelates quantization error from the signal; exact digital silence stays silent.
pub struct PcmEncoder {
    bits: u16,
    state: u32,
}

impl PcmEncoder {
    pub fn new(bits: u16) -> Self {
        assert!(bits == 16 || bits == 24);
        Self { bits, state: 0x9E37_79B9 }
    }

    #[inline]
    fn next_unit(&mut self) -> f64 {
        // xorshift32: fast, allocation-free, plenty for dither noise.
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x as f64 / 4_294_967_296.0
    }

    #[inline]
    pub fn sample16(&mut self, s: f32) -> i16 {
        if s == 0.0 || s.is_nan() {
            return 0;
        }
        let dither = self.next_unit() - self.next_unit();
        (s as f64 * 32767.0 + dither).round().clamp(-32768.0, 32767.0) as i16
    }

    pub fn encode16(&mut self, samples: &[f32], out: &mut Vec<i16>) {
        out.clear();
        out.extend(samples.iter().map(|&s| self.sample16(s)));
    }

    /// Appends little-endian bytes to `out`.
    pub fn encode_bytes(&mut self, samples: &[f32], out: &mut Vec<u8>) {
        out.clear();
        if self.bits == 16 {
            for &s in samples {
                out.extend_from_slice(&self.sample16(s).to_le_bytes());
            }
        } else {
            for &s in samples {
                let v = if s.is_nan() { 0 } else { (s as f64 * 8_388_607.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32 };
                out.extend_from_slice(&v.to_le_bytes()[..3]);
            }
        }
    }
}

pub struct WavWriter {
    out: BufWriter<File>,
    data_bytes: u64,
}

impl WavWriter {
    pub fn create(path: &Path, sample_rate: u32, channels: u16, bits: u16) -> io::Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 18, File::create(path)?);
        let block_align = channels * bits / 8;
        out.write_all(b"RIFF")?;
        out.write_all(&0u32.to_le_bytes())?; // patched in finish()
        out.write_all(b"WAVEfmt ")?;
        out.write_all(&16u32.to_le_bytes())?;
        out.write_all(&1u16.to_le_bytes())?; // WAVE_FORMAT_PCM
        out.write_all(&channels.to_le_bytes())?;
        out.write_all(&sample_rate.to_le_bytes())?;
        out.write_all(&(sample_rate * block_align as u32).to_le_bytes())?;
        out.write_all(&block_align.to_le_bytes())?;
        out.write_all(&bits.to_le_bytes())?;
        out.write_all(b"data")?;
        out.write_all(&0u32.to_le_bytes())?;
        Ok(Self { out, data_bytes: 0 })
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.data_bytes += bytes.len() as u64;
        self.out.write_all(bytes)
    }

    pub fn finish(mut self) -> io::Result<()> {
        if self.data_bytes % 2 == 1 {
            self.out.write_all(&[0])?; // RIFF chunks are word-aligned
        }
        let data = self.data_bytes as u32;
        let riff = (36 + self.data_bytes + self.data_bytes % 2) as u32;
        self.out.seek(SeekFrom::Start(4))?;
        self.out.write_all(&riff.to_le_bytes())?;
        self.out.seek(SeekFrom::Start(40))?;
        self.out.write_all(&data.to_le_bytes())?;
        let file = self.out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_silent_and_full_scale_clamps() {
        let mut e = PcmEncoder::new(16);
        assert_eq!(e.sample16(0.0), 0);
        assert_eq!(e.sample16(2.0), 32767);
        assert_eq!(e.sample16(-2.0), -32768);
        let mut bytes = Vec::new();
        let mut e24 = PcmEncoder::new(24);
        e24.encode_bytes(&[1.0, -1.0], &mut bytes);
        assert_eq!(bytes, vec![0xff, 0xff, 0x7f, 0x01, 0x00, 0x80]); // symmetric: -1.0 → -8388607
    }

    #[test]
    fn writes_a_valid_header() {
        let dir = std::env::temp_dir().join(format!("rt-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let mut w = WavWriter::create(&path, 48000, 2, 24).unwrap();
        w.write(&[0u8; 6 * 10]).unwrap();
        w.finish().unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(b.len(), 44 + 60);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 36 + 60);
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 60);
        assert_eq!(u16::from_le_bytes(b[32..34].try_into().unwrap()), 6);
        std::fs::remove_dir_all(dir).ok();
    }
}
