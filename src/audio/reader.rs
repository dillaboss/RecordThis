//! Streaming access to a raw spool file as 32-bit float frames. Nothing is held in memory
//! beyond one read buffer, so hour-long recordings preview and export with the same small
//! footprint as a ten-second one.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::convert;
use super::format::CaptureFormat;

/// A pull-based source of interleaved f32 frames.
pub trait FrameSource {
    fn channels(&self) -> u16;
    fn sample_rate(&self) -> u32;
    /// Fills `out` (a whole number of frames) and returns the number of frames written.
    /// Returns 0 only at the end of the stream.
    fn read(&mut self, out: &mut [f32]) -> io::Result<usize>;
}

pub struct SpoolReader {
    file: File,
    format: CaptureFormat,
    total_frames: u64,
    position: u64,
    end_limit: u64,
    file_position: Option<u64>,
    bytes: Vec<u8>,
}

impl SpoolReader {
    pub fn open(path: &Path, format: CaptureFormat) -> io::Result<Self> {
        let file = crate::storage::spool::open_shared_read(path)?;
        let len = file.metadata()?.len();
        // A crash can leave a trailing partial frame; it's ignored.
        let total_frames = len / format.block_align() as u64;
        Ok(Self { file, format, total_frames, position: 0, end_limit: total_frames, file_position: None, bytes: Vec::new() })
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn set_position(&mut self, frame: u64) {
        self.position = frame.min(self.total_frames);
    }

    pub fn end_limit(&self) -> u64 {
        self.end_limit
    }

    /// Reading stops at this absolute frame (the trim-out point).
    pub fn set_end_limit(&mut self, frame: u64) {
        self.end_limit = frame.min(self.total_frames);
    }

    pub fn format(&self) -> &CaptureFormat {
        &self.format
    }
}

impl FrameSource for SpoolReader {
    fn channels(&self) -> u16 {
        self.format.channels
    }

    fn sample_rate(&self) -> u32 {
        self.format.sample_rate
    }

    fn read(&mut self, out: &mut [f32]) -> io::Result<usize> {
        let channels = self.format.channels as usize;
        let block = self.format.block_align();
        let frames = ((out.len() / channels) as u64).min(self.end_limit.saturating_sub(self.position)) as usize;
        if frames == 0 {
            return Ok(0);
        }

        let byte_count = frames * block;
        if self.bytes.len() < byte_count {
            self.bytes.resize(byte_count, 0);
        }

        let offset = self.position * block as u64;
        if self.file_position != Some(offset) {
            self.file.seek(SeekFrom::Start(offset))?;
        }

        let mut read = 0;
        while read < byte_count {
            match self.file.read(&mut self.bytes[read..byte_count]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.file_position = None;
                    return Err(e);
                }
            }
        }
        self.file_position = Some(offset + read as u64);

        let frames_read = read / block;
        convert::to_float(&self.bytes[..frames_read * block], &mut out[..frames_read * channels], &self.format);
        self.position += frames_read as u64;
        Ok(frames_read)
    }
}

/// Reads until `out` is full or the source ends. Returns frames read.
pub fn read_fully<S: FrameSource + ?Sized>(source: &mut S, out: &mut [f32]) -> io::Result<usize> {
    let ch = source.channels() as usize;
    let wanted = out.len() / ch;
    let mut total = 0;
    while total < wanted {
        let n = source.read(&mut out[total * ch..wanted * ch])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}
