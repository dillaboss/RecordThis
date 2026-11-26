//! Sample-rate conversion (windowed sinc, via rubato) that produces exactly
//! round(source_frames × out_rate / in_rate) frames. The resampler's start-up delay is skipped
//! and its filter tail is flushed with zeros, so the exported timeline never shifts, loses its end
//! or grows. When the rates already match, samples pass straight through.

use std::collections::VecDeque;
use std::io;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType, WindowFunction};

use super::reader::{read_fully, FrameSource};

const CHUNK_FRAMES: usize = 1024;
const MAX_IDLE: usize = 8;

pub struct ExactLengthResampler<S: FrameSource> {
    source: S,
    channels: usize,
    out_rate: u32,
    expected: u64,
    produced: u64,
    source_done: bool,
    engine: Option<Engine>,
}

struct Engine {
    resampler: Async<f32>,
    input: Vec<f32>,
    output: Vec<f32>,
    pending: VecDeque<f32>,
    skip_frames: usize,
}

impl<S: FrameSource> ExactLengthResampler<S> {
    pub fn new(source: S, out_rate: u32, source_frames: u64) -> Self {
        let channels = source.channels() as usize;
        let in_rate = source.sample_rate();
        let (expected, engine) = if in_rate == out_rate {
            (source_frames, None)
        } else {
            let ratio = out_rate as f64 / in_rate as f64;
            let params = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2)
                .oversampling_factor(256)
                .interpolation(SincInterpolationType::Cubic);
            let resampler = Async::<f32>::new_sinc(ratio, 1.0, &params, CHUNK_FRAMES, channels, FixedAsync::Input)
                .expect("valid resampler parameters");
            let engine = Engine {
                input: vec![0.0; resampler.input_frames_max() * channels],
                output: vec![0.0; resampler.output_frames_max() * channels],
                skip_frames: resampler.output_delay(),
                pending: VecDeque::new(),
                resampler,
            };
            ((source_frames as f64 * ratio).round() as u64, Some(engine))
        };
        Self { source, channels, out_rate, expected, produced: 0, source_done: false, engine }
    }

    pub fn expected_frames(&self) -> u64 {
        self.expected
    }

    pub fn is_resampling(&self) -> bool {
        self.engine.is_some()
    }

    pub fn inner(&self) -> &S {
        &self.source
    }

    /// Runs one resampler chunk and queues its output. Returns frames queued.
    fn pump(&mut self) -> io::Result<usize> {
        let ch = self.channels;
        let engine = self.engine.as_mut().expect("resampling");
        let needed = engine.resampler.input_frames_next();
        let got = if self.source_done { 0 } else { read_fully(&mut self.source, &mut engine.input[..needed * ch])? };
        if got < needed {
            self.source_done = true;
        }

        let indexing = Indexing::new().partial_len(got);
        let input = InterleavedSlice::new(&engine.input[..], ch, engine.input.len() / ch).map_err(io::Error::other)?;
        let frames_out = engine.output.len() / ch;
        let mut output = InterleavedSlice::new_mut(&mut engine.output[..], ch, frames_out).map_err(io::Error::other)?;
        let (_, produced) = engine
            .resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(io::Error::other)?;

        let skip = engine.skip_frames.min(produced);
        engine.skip_frames -= skip;
        engine.pending.extend(&engine.output[skip * ch..produced * ch]);
        Ok(produced - skip)
    }
}

impl<S: FrameSource> FrameSource for ExactLengthResampler<S> {
    fn channels(&self) -> u16 {
        self.channels as u16
    }

    fn sample_rate(&self) -> u32 {
        self.out_rate
    }

    fn read(&mut self, out: &mut [f32]) -> io::Result<usize> {
        let ch = self.channels;
        let wanted = ((out.len() / ch) as u64).min(self.expected - self.produced) as usize;
        if wanted == 0 {
            return Ok(0);
        }

        if self.engine.is_none() {
            let got = if self.source_done { 0 } else { read_fully(&mut self.source, &mut out[..wanted * ch])? };
            if got < wanted {
                // The source came up short (e.g. a trailing partial frame): pad with silence.
                self.source_done = true;
                out[got * ch..wanted * ch].fill(0.0);
            }
            self.produced += wanted as u64;
            return Ok(wanted);
        }

        let mut idle = 0;
        while self.engine.as_ref().unwrap().pending.len() < wanted * ch {
            if self.pump()? == 0 {
                idle += 1;
                if idle >= MAX_IDLE {
                    // Defensive: never stall an export. Pad the remainder with silence.
                    let pending = &mut self.engine.as_mut().unwrap().pending;
                    let missing = wanted * ch - pending.len();
                    pending.extend(std::iter::repeat_n(0.0, missing));
                }
            } else {
                idle = 0;
            }
        }

        let pending = &mut self.engine.as_mut().unwrap().pending;
        for (dst, src) in out[..wanted * ch].iter_mut().zip(pending.drain(..wanted * ch)) {
            *dst = src;
        }
        self.produced += wanted as u64;
        Ok(wanted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sine {
        rate: u32,
        frames: u64,
        pos: u64,
        freq: f32,
    }

    impl FrameSource for Sine {
        fn channels(&self) -> u16 {
            2
        }
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn read(&mut self, out: &mut [f32]) -> io::Result<usize> {
            let n = ((out.len() / 2) as u64).min(self.frames - self.pos) as usize;
            for f in 0..n {
                let t = (self.pos + f as u64) as f32 / self.rate as f32;
                let v = (t * self.freq * std::f32::consts::TAU).sin() * 0.5;
                out[f * 2] = v;
                out[f * 2 + 1] = v;
            }
            self.pos += n as u64;
            Ok(n)
        }
    }

    fn drain<S: FrameSource>(r: &mut S) -> Vec<f32> {
        let mut all = Vec::new();
        let mut buf = vec![0f32; 4096 * 2];
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n * 2]);
        }
        all
    }

    #[test]
    fn produces_exact_length_and_keeps_alignment() {
        let frames = 44_100 + 123;
        let src = Sine { rate: 44_100, frames, pos: 0, freq: 1000.0 };
        let mut r = ExactLengthResampler::new(src, 48_000, frames);
        assert!(r.is_resampling());
        let out = drain(&mut r);
        let expected = (frames as f64 * 48_000.0 / 44_100.0).round() as usize;
        assert_eq!(out.len(), expected * 2);

        // Timeline alignment: the best-matching lag against an ideal 1 kHz sine must be (well)
        // under one output sample, i.e. no start-up delay is left in the output.
        let err_at = |lag: f32| {
            let mut err = 0f32;
            for f in 4800..40_000 {
                let t = (f as f32 + lag) / 48_000.0;
                let ideal = (t * 1000.0 * std::f32::consts::TAU).sin() * 0.5;
                err = err.max((out[f * 2] - ideal).abs());
            }
            err
        };
        let best = (-20..=20).map(|i| i as f32 * 0.05).min_by(|a, b| err_at(*a).total_cmp(&err_at(*b))).unwrap();
        assert!(best.abs() <= 0.5, "output is shifted by {best} samples");
        assert!(err_at(best) < 0.005, "residual error {}", err_at(best));
    }

    #[test]
    fn passthrough_when_rates_match() {
        let src = Sine { rate: 48_000, frames: 1000, pos: 0, freq: 440.0 };
        let mut r = ExactLengthResampler::new(src, 48_000, 1000);
        assert!(!r.is_resampling());
        assert_eq!(drain(&mut r).len(), 2000);
    }

    #[test]
    fn pads_a_short_source() {
        let src = Sine { rate: 48_000, frames: 900, pos: 0, freq: 440.0 };
        let mut r = ExactLengthResampler::new(src, 44_100, 1000);
        let out = drain(&mut r);
        assert_eq!(out.len(), (1000.0f64 * 44_100.0 / 48_000.0).round() as usize * 2);
    }
}
