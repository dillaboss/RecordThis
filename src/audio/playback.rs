//! Review player: streams the spool (downmixed, clip-safe gain) to the default output device on
//! its own thread. The UI sends commands and reads the position lock-free.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::downmix::Downmix;
use super::reader::{FrameSource, SpoolReader};
use crate::errors::AudioError;
use crate::platform::{self, RenderStream};
use crate::storage::spool::SpoolSession;

enum Command {
    Play,
    Pause,
    Seek(u64),
    Range(u64, u64),
    Gain(f32),
    Quit,
}

/// Raised when playback ends: `None` at the end of the selection, `Some(error)` if the output
/// device failed or disappeared.
pub type StoppedCallback = Arc<dyn Fn(Option<AudioError>) + Send + Sync>;

pub struct Player {
    tx: Sender<Command>,
    position: Arc<AtomicU64>,
    playing: Arc<AtomicBool>,
    total_frames: u64,
    sample_rate: u32,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn load(session: &SpoolSession, gain: f32, on_stopped: StoppedCallback) -> Result<Self, AudioError> {
        let reader = SpoolReader::open(&session.raw_path, session.format)?;
        let total_frames = reader.total_frames();
        let mut mix = Downmix::new(reader, session.format.channel_mask);
        mix.gain = gain;

        let (tx, rx) = mpsc::channel();
        let position = Arc::new(AtomicU64::new(0));
        let playing = Arc::new(AtomicBool::new(false));
        let thread = {
            let position = position.clone();
            let playing = playing.clone();
            thread::Builder::new()
                .name("playback".into())
                .spawn(move || {
                    let _com = platform::init_thread();
                    PlayerThread { mix, rx, position, playing, on_stopped, stream: None, range: (0, total_frames), scratch: Vec::new() }.run()
                })
                .map_err(|e| AudioError::new(format!("Couldn't start playback: {e}")))?
        };
        Ok(Self { tx, position, playing, total_frames, sample_rate: session.format.sample_rate, thread: Some(thread) })
    }

    pub fn play(&self) {
        self.playing.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Command::Play);
    }

    pub fn pause(&self) {
        self.playing.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Command::Pause);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub fn seek(&self, frame: u64) {
        let frame = frame.min(self.total_frames);
        self.position.store(frame, Ordering::Relaxed);
        let _ = self.tx.send(Command::Seek(frame));
    }

    /// Limits playback to the trimmed selection (absolute frames).
    pub fn set_range(&self, start: u64, end: u64) {
        let _ = self.tx.send(Command::Range(start.min(self.total_frames), end.clamp(start, self.total_frames)));
    }

    pub fn set_gain(&self, gain: f32) {
        let _ = self.tx.send(Command::Gain(gain));
    }

    /// Frame currently heard (accounts for the device buffer).
    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct PlayerThread {
    mix: Downmix<SpoolReader>,
    rx: Receiver<Command>,
    position: Arc<AtomicU64>,
    playing: Arc<AtomicBool>,
    on_stopped: StoppedCallback,
    stream: Option<RenderStream>,
    range: (u64, u64),
    scratch: Vec<f32>,
}

impl PlayerThread {
    fn reader(&mut self) -> &mut SpoolReader {
        self.mix.inner_mut()
    }

    fn run(mut self) {
        let mut active = false;
        loop {
            // Idle: block for the next command. Playing: poll between buffer refills.
            let cmd = if active {
                match self.rx.recv_timeout(Duration::from_millis(1)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match self.rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            };

            if let Some(cmd) = cmd {
                match cmd {
                    Command::Quit => return,
                    Command::Play => {
                        if !active {
                            let pos = self.reader().position();
                            let (start, end) = self.range;
                            if pos < start || pos >= end {
                                self.reader().set_position(start); // start (or restart) at the trim-in point
                            }
                            match self.ensure_stream().and_then(|s| s.start()) {
                                Ok(()) => active = true,
                                Err(e) => self.fail(e),
                            }
                        }
                    }
                    Command::Pause => {
                        if active {
                            active = false;
                            self.update_position();
                            if let Some(s) = &mut self.stream {
                                let _ = s.stop();
                            }
                        }
                    }
                    Command::Seek(frame) => {
                        self.flush_stream(active);
                        self.reader().set_position(frame);
                        self.position.store(frame, Ordering::Relaxed);
                    }
                    Command::Range(start, end) => {
                        self.range = (start, end);
                        self.reader().set_end_limit(end);
                    }
                    Command::Gain(g) => self.mix.gain = g,
                }
                continue; // drain all queued commands before writing audio
            }

            if active {
                match self.pump() {
                    Ok(true) => {}
                    Ok(false) => {
                        // Reached the end of the selection and the device buffer drained.
                        active = false;
                        if let Some(s) = &mut self.stream {
                            let _ = s.stop();
                        }
                        let pos = self.reader().position();
                        self.position.store(pos, Ordering::Relaxed);
                        self.playing.store(false, Ordering::Relaxed);
                        (self.on_stopped)(None);
                    }
                    Err(e) => {
                        active = false;
                        self.fail(e);
                    }
                }
            }
        }
    }

    fn ensure_stream(&mut self) -> Result<&mut RenderStream, AudioError> {
        if self.stream.is_none() {
            self.stream = Some(RenderStream::open(self.mix.sample_rate(), self.mix.channels())?);
        }
        Ok(self.stream.as_mut().unwrap())
    }

    /// Drops queued audio so a seek is heard immediately.
    fn flush_stream(&mut self, active: bool) {
        if let Some(s) = &mut self.stream {
            let _ = s.stop();
            let _ = s.reset();
            if active {
                let _ = s.start();
            }
        }
    }

    fn fail(&mut self, error: AudioError) {
        self.stream = None; // a fresh stream (on the current default device) is opened on the next Play
        self.playing.store(false, Ordering::Relaxed);
        (self.on_stopped)(Some(error));
    }

    fn update_position(&mut self) {
        let queued = self.stream.as_ref().and_then(|s| s.padding().ok()).unwrap_or(0) as u64;
        let pos = self.mix.inner().position().saturating_sub(queued);
        self.position.store(pos, Ordering::Relaxed);
    }

    /// Tops up the device buffer. Returns false once the selection has fully played.
    fn pump(&mut self) -> Result<bool, AudioError> {
        let ch = self.mix.channels() as usize;
        let stream = self.stream.as_mut().expect("stream open while active");
        stream.wait(Duration::from_millis(20));
        let padding = stream.padding()?;
        let free = stream.buffer_frames().saturating_sub(padding);
        let mut wrote_any = false;
        if free > 0 {
            if self.scratch.len() < free * ch {
                self.scratch.resize(free * ch, 0.0);
            }
            let n = self.mix.read(&mut self.scratch[..free * ch]).map_err(AudioError::from)?;
            if n > 0 {
                self.stream.as_mut().unwrap().write(&self.scratch[..n * ch])?;
                wrote_any = true;
            }
        }
        self.update_position();
        let padding = self.stream.as_ref().unwrap().padding()?;
        Ok(wrote_any || padding > 0)
    }
}
