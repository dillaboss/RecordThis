//! ITU-R BS.775-style fold-down built from the WAVEFORMATEXTENSIBLE channel mask:
//! Left = L + 0.707·C + 0.707·LFE + 0.707·Ls, Right = R + 0.707·C + 0.707·LFE + 0.707·Rs.
//! These coefficients can sum above 0 dBFS; a peak-aware gain stage downstream
//! ([`clip_safe_gain`]) prevents clipping, not the matrix itself.

use super::reader::FrameSource;

const MINUS_3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Ceiling used when a downmix would clip: −0.1 dBFS.
pub const SAFE_CEILING: f32 = 0.988_55;

/// The BS.775 coefficients add up to more than unity, so a loud 5.1/7.1 capture can exceed 0 dBFS
/// after fold-down. Scale back just enough to peak at −0.1 dBFS; untouched otherwise.
pub fn clip_safe_gain(peak: f32) -> f32 {
    if peak > 1.0 { SAFE_CEILING / peak } else { 1.0 }
}

pub fn requires_downmix(channels: u16) -> bool {
    channels > 2
}

/// Mono stays mono; everything else becomes stereo.
pub fn output_channels(input: u16) -> u16 {
    if input == 1 { 1 } else { 2 }
}

mod speaker {
    pub const FL: u32 = 0x1;
    pub const FR: u32 = 0x2;
    pub const FC: u32 = 0x4;
    pub const LFE: u32 = 0x8;
    pub const BL: u32 = 0x10;
    pub const BR: u32 = 0x20;
    pub const FLC: u32 = 0x40;
    pub const FRC: u32 = 0x80;
    pub const BC: u32 = 0x100;
    pub const SL: u32 = 0x200;
    pub const SR: u32 = 0x400;
    pub const TC: u32 = 0x800;
    pub const TFL: u32 = 0x1000;
    pub const TFC: u32 = 0x2000;
    pub const TFR: u32 = 0x4000;
    pub const TBL: u32 = 0x8000;
    pub const TBC: u32 = 0x10000;
    pub const TBR: u32 = 0x20000;
}

fn default_mask(channels: u16) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x13F,
        _ => 0x63F,
    }
}

/// Returns `matrix[output][input]`.
pub fn build_matrix(channels: u16, channel_mask: u32) -> Vec<Vec<f32>> {
    let n = channels as usize;
    let outputs = output_channels(channels) as usize;
    let mut m = vec![vec![0f32; n]; outputs];
    if n == 1 {
        m[0][0] = 1.0;
        return m;
    }

    let mask = if channel_mask.count_ones() as usize >= n { channel_mask } else { default_mask(channels) };
    let mut positions = vec![0u32; n];
    let mut c = 0;
    for bit in 0..31 {
        if c >= n {
            break;
        }
        if mask & (1 << bit) != 0 {
            positions[c] = 1 << bit;
            c += 1;
        }
    }

    use speaker::*;
    for (c, &p) in positions.iter().enumerate() {
        let (l, r) = match p {
            FL | FLC => (1.0, 0.0),
            FR | FRC => (0.0, 1.0),
            FC | LFE | BC => (MINUS_3DB, MINUS_3DB),
            BL | SL | TFL | TBL => (MINUS_3DB, 0.0),
            BR | SR | TFR | TBR => (0.0, MINUS_3DB),
            TC | TFC | TBC => (0.5, 0.5),
            // Channels beyond the mask are split left/right.
            _ => if c % 2 == 0 { (MINUS_3DB, 0.0) } else { (0.0, MINUS_3DB) },
        };
        m[0][c] = l;
        m[1][c] = r;
    }
    m
}

/// Folds any channel layout down to stereo (mono stays mono), then applies a gain.
pub struct Downmix<S: FrameSource> {
    source: S,
    matrix: Vec<Vec<f32>>,
    in_channels: usize,
    out_channels: usize,
    scratch: Vec<f32>,
    pub gain: f32,
}

impl<S: FrameSource> Downmix<S> {
    pub fn new(source: S, channel_mask: u32) -> Self {
        let ch = source.channels();
        Self {
            matrix: build_matrix(ch, channel_mask),
            in_channels: ch as usize,
            out_channels: output_channels(ch) as usize,
            scratch: Vec::new(),
            gain: 1.0,
            source,
        }
    }

    pub fn is_downmixing(&self) -> bool {
        requires_downmix(self.in_channels as u16)
    }

    pub fn inner(&self) -> &S {
        &self.source
    }

    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.source
    }
}

impl<S: FrameSource> FrameSource for Downmix<S> {
    fn channels(&self) -> u16 {
        self.out_channels as u16
    }

    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }

    fn read(&mut self, out: &mut [f32]) -> std::io::Result<usize> {
        let frames = out.len() / self.out_channels;
        if !self.is_downmixing() {
            let n = self.source.read(&mut out[..frames * self.out_channels])?;
            if self.gain != 1.0 {
                out[..n * self.out_channels].iter_mut().for_each(|s| *s *= self.gain);
            }
            return Ok(n);
        }

        let needed = frames * self.in_channels;
        if self.scratch.len() < needed {
            self.scratch.resize(needed, 0.0);
        }
        let n = self.source.read(&mut self.scratch[..needed])?;
        let (lm, rm) = (&self.matrix[0], &self.matrix[1]);
        for f in 0..n {
            let frame = &self.scratch[f * self.in_channels..(f + 1) * self.in_channels];
            let (mut l, mut r) = (0f32, 0f32);
            for c in 0..self.in_channels {
                l += frame[c] * lm[c];
                r += frame[c] * rm[c];
            }
            out[f * 2] = l * self.gain;
            out[f * 2 + 1] = r * self.gain;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_one_matrix_matches_bs775() {
        let m = build_matrix(6, 0x3F);
        assert_eq!(m[0], vec![1.0, 0.0, MINUS_3DB, MINUS_3DB, MINUS_3DB, 0.0]);
        assert_eq!(m[1], vec![0.0, 1.0, MINUS_3DB, MINUS_3DB, 0.0, MINUS_3DB]);
        // An incomplete mask falls back to the default layout for the channel count.
        assert_eq!(build_matrix(6, 0x3), m);
    }

    #[test]
    fn clip_safe_gain_only_reduces() {
        assert_eq!(clip_safe_gain(0.5), 1.0);
        assert!((clip_safe_gain(2.0) * 2.0 - SAFE_CEILING).abs() < 1e-6);
    }
}
