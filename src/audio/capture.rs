//! Records a capture endpoint (or a render endpoint through loopback) straight to a disk spool in
//! the device's native mix format, or just meters it ("monitor" mode, used for the live level
//! before recording starts).
//!
//! Silence watchdog: in loopback mode Windows delivers no packets while nothing is playing, which
//! would shrink the timeline. The capture thread polls every 10 ms; once 150 ms pass without a real
//! packet it writes zero-level frames covering the wall-clock time since that packet. The anchor
//! resets on every real packet, so the device clock and the system clock never drift apart.
//! Everything runs on one thread, so there's no timer racing the packet writer.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::convert;
use super::format::CaptureFormat;
use crate::errors::AudioError;
use crate::platform::{self, CaptureStream, DeviceInfo};
use crate::storage::spool::{open_for_writing, SpoolManager, SpoolSession};

const POLL: Duration = Duration::from_millis(10);
const GAP_THRESHOLD: Duration = Duration::from_millis(150);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const SILENCE_CHUNK_FRAMES: u64 = 4800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    User,
    DeviceLost,
    SystemSuspend,
    Error,
    Recovered,
}

#[derive(Clone, Debug)]
pub struct CaptureResult {
    pub session: SpoolSession,
    pub reason: StopReason,
    pub duration: Duration,
    pub silence_injected: Duration,
    pub error: Option<AudioError>,
}

pub enum CaptureEvent {
    /// A recording ended, whatever ended it. Raised exactly once per recording, on the capture thread.
    Stopped(CaptureResult),
    /// A level-monitor stream died on its own (device unplugged, exclusive mode taken...).
    MonitorFailed(AudioError),
}

pub type EventCallback = Arc<dyn Fn(CaptureEvent) + Send + Sync>;

#[derive(Default)]
struct Meter {
    left: AtomicU32,
    right: AtomicU32,
    frames: AtomicU64,
    rate: AtomicU32,
}

impl Meter {
    fn raise(slot: &AtomicU32, value: f32) {
        // For non-negative floats, the bit patterns sort the same way as the values.
        let bits = value.to_bits();
        let _ = slot.fetch_max(bits, Ordering::Relaxed);
    }
}

/// Recording state shared between the capture thread and `stop()` (which finalizes the file
/// itself if the driver never lets the thread exit).
struct Recording {
    file: Option<File>,
    session: SpoolSession,
    format: CaptureFormat,
    frames_written: u64,
    silence_frames: u64,
    anchor: Instant,
    anchor_frames: u64,
    requested: Option<StopReason>,
    error: Option<AudioError>,
    result: Option<CaptureResult>,
}

impl Recording {
    fn write(&mut self, bytes: &[u8], meter: &Meter) -> std::io::Result<()> {
        let Some(file) = self.file.as_mut() else { return Ok(()) };
        file.write_all(bytes)?;
        self.frames_written += (bytes.len() / self.format.block_align()) as u64;
        self.anchor = Instant::now();
        self.anchor_frames = self.frames_written;
        meter.frames.store(self.frames_written, Ordering::Relaxed);
        Ok(())
    }

    fn fill_silence_gap(&mut self, force: bool, meter: &Meter) -> std::io::Result<()> {
        let Some(file) = self.file.as_mut() else { return Ok(()) };
        let elapsed = self.anchor.elapsed();
        if !force && elapsed < GAP_THRESHOLD {
            return Ok(());
        }
        let expected = self.anchor_frames + (elapsed.as_secs_f64() * self.format.sample_rate as f64) as u64;
        let mut deficit = expected.saturating_sub(self.frames_written);
        if deficit == 0 {
            return Ok(());
        }
        let block = self.format.block_align();
        let chunk = vec![self.format.silence_byte(); SILENCE_CHUNK_FRAMES as usize * block];
        while deficit > 0 {
            let n = deficit.min(SILENCE_CHUNK_FRAMES);
            file.write_all(&chunk[..n as usize * block])?;
            deficit -= n;
            self.frames_written += n;
            self.silence_frames += n;
        }
        meter.frames.store(self.frames_written, Ordering::Relaxed);
        Ok(())
    }

    /// Idempotent: flushes, closes the spool and builds the result once.
    fn finalize(&mut self, error: Option<AudioError>, meter: &Meter) -> CaptureResult {
        if let Some(result) = &self.result {
            return result.clone();
        }
        let mut error = error.or_else(|| self.error.take());
        // Time since the last packet is still part of the timeline.
        if let Err(e) = self.fill_silence_gap(true, meter) {
            error.get_or_insert(e.into());
        }
        if let Some(file) = self.file.take() {
            if let Err(e) = file.sync_all() {
                error.get_or_insert(e.into());
            }
        }
        let reason = self.requested.unwrap_or(match &error {
            None => StopReason::User,
            Some(e) if e.is_device_loss() => StopReason::DeviceLost,
            Some(_) => StopReason::Error,
        });
        let rate = self.format.sample_rate as f64;
        let result = CaptureResult {
            session: self.session.clone(),
            reason,
            duration: Duration::from_secs_f64(self.frames_written as f64 / rate),
            silence_injected: Duration::from_secs_f64(self.silence_frames as f64 / rate),
            error: if matches!(reason, StopReason::User | StopReason::SystemSuspend) { None } else { error },
        };
        self.result = Some(result.clone());
        result
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
    recording: Option<Arc<Mutex<Recording>>>,
    device_id: String,
    format: CaptureFormat,
}

pub struct CaptureEngine {
    meter: Arc<Meter>,
    worker: Mutex<Option<Worker>>,
    spools: Arc<SpoolManager>,
}

pub enum Mode {
    Monitor,
    Record,
}

impl CaptureEngine {
    pub fn new(spools: Arc<SpoolManager>) -> Self {
        Self { meter: Arc::new(Meter::default()), worker: Mutex::new(None), spools }
    }

    pub fn is_recording(&self) -> bool {
        self.worker.lock().unwrap().as_ref().is_some_and(|w| w.recording.is_some())
    }

    pub fn is_monitoring(&self) -> bool {
        self.worker.lock().unwrap().as_ref().is_some_and(|w| w.recording.is_none())
    }

    pub fn active_device_id(&self) -> Option<String> {
        self.worker.lock().unwrap().as_ref().map(|w| w.device_id.clone())
    }

    pub fn active_format(&self) -> Option<CaptureFormat> {
        self.worker.lock().unwrap().as_ref().map(|w| w.format)
    }

    /// Timeline length on disk, including injected silence.
    pub fn recorded_duration(&self) -> Duration {
        let rate = self.meter.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.meter.frames.load(Ordering::Relaxed) as f64 / rate as f64)
    }

    /// Highest left/right magnitude since the previous call (lock-free, for the UI meter).
    pub fn read_and_reset_peaks(&self) -> (f32, f32) {
        (
            f32::from_bits(self.meter.left.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.meter.right.swap(0, Ordering::Relaxed)),
        )
    }

    /// Opens `device` and starts metering it, recording to a new spool in `Record` mode.
    /// `on_event` runs on the capture thread when a recording ends or a monitor fails.
    pub fn start(&self, device: &DeviceInfo, mode: Mode, on_event: EventCallback) -> Result<Option<SpoolSession>, AudioError> {
        let mut guard = self.worker.lock().unwrap();
        if guard.is_some() {
            return Err(AudioError::new("A recording is already in progress"));
        }

        let stop = Arc::new(AtomicBool::new(false));
        let (init_tx, init_rx) = mpsc::channel::<Result<(CaptureFormat, Option<Arc<Mutex<Recording>>>), AudioError>>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let record = matches!(mode, Mode::Record);
        let spools = self.spools.clone();
        let meter = self.meter.clone();
        let stop_flag = stop.clone();
        let dev = device.clone();

        thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let _com = platform::init_thread();
                let opened = (|| {
                    let mut stream = CaptureStream::open(&dev)?;
                    let format = stream.format();
                    let recording = if record {
                        let session = spools.create_session(format, &dev.name)?;
                        let file = match open_for_writing(&session.raw_path) {
                            Ok(f) => f,
                            Err(e) => {
                                spools.delete(&session);
                                return Err(e.into());
                            }
                        };
                        Some(Arc::new(Mutex::new(Recording {
                            file: Some(file),
                            session,
                            format,
                            frames_written: 0,
                            silence_frames: 0,
                            anchor: Instant::now(),
                            anchor_frames: 0,
                            requested: None,
                            error: None,
                            result: None,
                        })))
                    } else {
                        None
                    };
                    // IAudioClient::Start; errors surface here, before the UI reports success.
                    if let Err(e) = stream.start() {
                        if let Some(r) = &recording {
                            let mut r = r.lock().unwrap();
                            r.file.take(); // close before deleting
                            spools.delete(&r.session);
                        }
                        return Err(e);
                    }
                    Ok((stream, format, recording))
                })();

                let (mut stream, format, recording) = match opened {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };
                meter.frames.store(0, Ordering::Relaxed);
                meter.rate.store(format.sample_rate, Ordering::Relaxed);
                meter.left.store(0, Ordering::Relaxed);
                meter.right.store(0, Ordering::Relaxed);
                if let Some(r) = &recording {
                    r.lock().unwrap().anchor = Instant::now();
                }
                let _ = init_tx.send(Ok((format, recording.clone())));

                let sides = convert::meter_sides(format.channels, format.channel_mask);
                let block = format.block_align();
                let mut buf: Vec<u8> = Vec::with_capacity(format.sample_rate as usize / 10 * block);
                let mut error: Option<AudioError> = None;

                while !stop_flag.load(Ordering::Relaxed) {
                    buf.clear();
                    if let Err(e) = stream.read(&mut buf, POLL) {
                        error = Some(e);
                        break;
                    }
                    let whole = buf.len() - buf.len() % block;
                    if whole > 0 {
                        let (l, r) = convert::side_peaks(&buf[..whole], &format, &sides);
                        Meter::raise(&meter.left, l);
                        Meter::raise(&meter.right, r);
                    }
                    let Some(rec) = &recording else { continue };
                    let mut rec = rec.lock().unwrap();
                    if rec.result.is_some() {
                        break; // stop() gave up waiting and finalized without us
                    }
                    let written = if whole > 0 { rec.write(&buf[..whole], &meter) } else { rec.fill_silence_gap(false, &meter) };
                    if let Err(e) = written {
                        error = Some(e.into()); // e.g. disk full: finalize what we have
                        break;
                    }
                }

                drop(stream); // IAudioClient::Stop before the file closes
                if let Some(rec) = &recording {
                    let result = rec.lock().unwrap().finalize(error, &meter);
                    let _ = done_tx.send(());
                    on_event(CaptureEvent::Stopped(result));
                } else {
                    let _ = done_tx.send(());
                    if let Some(e) = error {
                        on_event(CaptureEvent::MonitorFailed(e));
                    }
                }
            })
            .map_err(|e| AudioError::new(format!("Couldn't start the capture thread: {e}")))?;

        match init_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok((format, recording))) => {
                let session = recording.as_ref().map(|r| r.lock().unwrap().session.clone());
                *guard = Some(Worker { stop, done: done_rx, recording, device_id: device.id.clone(), format });
                Ok(session)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err(AudioError::new("The audio device didn't respond"))
            }
        }
    }

    /// Stops recording or monitoring. For a recording, returns its result once the spool is
    /// final (at most ~2 s, even if the driver hangs).
    pub fn stop(&self, reason: StopReason) -> Option<CaptureResult> {
        let worker = self.worker.lock().unwrap().take()?;
        if let Some(rec) = &worker.recording {
            rec.lock().unwrap().requested.get_or_insert(reason);
        }
        worker.stop.store(true, Ordering::Relaxed);
        let finished = worker.done.recv_timeout(STOP_TIMEOUT).is_ok();
        self.meter.rate.store(0, Ordering::Relaxed);
        let rec = worker.recording?;
        let mut rec = rec.lock().unwrap();
        if finished {
            rec.result.clone()
        } else {
            // The driver never let the capture thread exit. Finalize anyway so no audio is lost.
            Some(rec.finalize(Some(AudioError::new("The audio device stopped responding")), &self.meter))
        }
    }

    /// Forgets a worker whose thread already ended on its own (device lost, disk full...).
    pub fn reap(&self, session_id: uuid::Uuid) {
        let mut guard = self.worker.lock().unwrap();
        let matches = guard
            .as_ref()
            .and_then(|w| w.recording.as_ref())
            .is_some_and(|r| r.lock().unwrap().session.id == session_id);
        if matches {
            *guard = None;
            self.meter.rate.store(0, Ordering::Relaxed);
        }
    }

    /// Forgets a monitor whose thread ended on its own.
    pub fn reap_monitor(&self) {
        let mut guard = self.worker.lock().unwrap();
        if guard.as_ref().is_some_and(|w| w.recording.is_none() && w.done.try_recv().is_ok()) {
            *guard = None;
        }
    }
}

impl Drop for CaptureEngine {
    fn drop(&mut self) {
        let _ = self.stop(StopReason::User);
    }
}
