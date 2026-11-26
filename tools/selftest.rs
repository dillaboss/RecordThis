//! End-to-end check of the app controller against the simulated (demo) devices, with no display:
//! record (including a loopback silence gap) → stop → review → play → trim → copy (MP3 → WAV
//! fallback) → new recording → undo → save WAV → close with the unsaved-recording dialog.
//!
//!     cargo run --features screenshots --bin selftest        (non-Windows only)

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use recordthis::app::{with_app, App};
use recordthis::ui::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Model};

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
    quit: AtomicBool,
}

struct Proxy(Arc<Queue>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.0.quit.store(true, Ordering::SeqCst);
        self.0.wake.notify_all();
        Ok(())
    }
    fn invoke_from_event_loop(&self, job: Job) -> Result<(), slint::EventLoopError> {
        self.0.jobs.lock().unwrap().push_back(job);
        self.0.wake.notify_all();
        Ok(())
    }
}

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    queue: Arc<Queue>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        loop {
            let jobs: Vec<Job> = self.queue.jobs.lock().unwrap().drain(..).collect();
            for job in jobs {
                job();
            }
            slint::platform::update_timers_and_animations();
            self.window.draw_if_needed(|_| {});
            if self.queue.quit.swap(false, Ordering::SeqCst) {
                return Ok(());
            }
            let guard = self.queue.jobs.lock().unwrap();
            if guard.is_empty() {
                let _ = self.queue.wake.wait_timeout(guard, Duration::from_millis(4)).unwrap();
            }
        }
    }
}

static FAILURES: AtomicUsize = AtomicUsize::new(0);

fn check(ok: bool, what: &str) {
    if ok {
        println!("  PASS  {what}");
    } else {
        println!("  FAIL  {what}");
        FAILURES.fetch_add(1, Ordering::SeqCst);
    }
}

/// Runs `f` on the UI thread and returns its result.
fn ask<R: Send + 'static>(f: impl FnOnce(&Rc<App>) -> R + Send + 'static) -> R {
    let (tx, rx) = mpsc::channel();
    slint::invoke_from_event_loop(move || with_app(move |app| tx.send(f(app)).unwrap())).unwrap();
    rx.recv_timeout(Duration::from_secs(10)).expect("UI thread didn't answer")
}

/// One real mouse click (move, press, release) at a logical position.
fn click(x: f32, y: f32) {
    ask(move |a| {
        let w = a.ui.window();
        let position = slint::LogicalPosition::new(x, y);
        w.dispatch_event(WindowEvent::PointerMoved { position });
        w.dispatch_event(WindowEvent::PointerPressed { position, button: slint::platform::PointerEventButton::Left });
        w.dispatch_event(WindowEvent::PointerReleased { position, button: slint::platform::PointerEventButton::Left });
    });
    std::thread::sleep(Duration::from_millis(100));
}

fn wait_until(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("  (timed out waiting for {what})");
    false
}

fn script() {
    std::thread::sleep(Duration::from_millis(400));
    println!("Recording screen");
    let (screen, devices, live) = ask(|a| {
        let c = a.ui.global::<CaptureState>();
        (a.ui.global::<AppState>().get_screen(), c.get_devices().row_count(), c.get_meter_live())
    });
    check(screen == 0, "starts on the recording screen");
    check(devices == 2, "lists the two loopback devices on the System audio tab");
    check(live, "system audio is metered before recording");
    check(wait_until("meter", Duration::from_secs(2), || ask(|a| a.ui.global::<CaptureState>().get_level_left()) > 0.2), "live meter moves while idle");

    // Single mouse clicks must work (regression: focus scopes used to swallow the first click).
    click(265.0, 55.0);
    check(ask(|a| a.ui.global::<CaptureState>().get_source_kind()) == 1, "one click on the Microphone tab selects it");
    click(123.0, 55.0);
    check(ask(|a| a.ui.global::<CaptureState>().get_source_kind()) == 0, "one click on the System audio tab selects it");
    std::thread::sleep(Duration::from_millis(300));

    click(300.0, 310.0); // Start recording
    let (rec, title) = ask(|a| (a.ui.global::<CaptureState>().get_recording(), a.ui.get_window_title()));
    check(rec, "one click on Start recording begins a recording");
    check(title.starts_with("● Recording"), "window title shows the recording state");
    // 5.6 s spans a 1.4 s stretch where the simulated loopback delivers no packets at all.
    std::thread::sleep(Duration::from_millis(5600));
    let elapsed = ask(|a| a.ui.global::<CaptureState>().get_elapsed_text());
    check(elapsed == "0:05", &format!("elapsed timer counts through the silent gap ({elapsed})"));
    click(300.0, 310.0); // Stop recording

    println!("Review screen");
    let (screen, title) = ask(|a| (a.ui.global::<AppState>().get_screen(), a.ui.global::<ReviewState>().get_title()));
    check(screen == 1, "one click on Stop recording opens the review screen");
    check(title == "Recording · 0:05", &format!("watchdog kept the timeline whole: {title}"));
    check(
        wait_until("analysis", Duration::from_secs(5), || ask(|a| !a.ui.global::<ReviewState>().get_analyzing() && !a.ui.global::<ReviewState>().get_waveform().is_empty())),
        "waveform is drawn",
    );
    let silence_label = ask(|a| a.ui.global::<ReviewState>().get_trim_silence_text());
    println!("        (trim-silence button reads \"{silence_label}\")");

    click(97.0, 246.0); // Play
    std::thread::sleep(Duration::from_millis(700));
    let (playing, progress) = ask(|a| (a.ui.global::<ReviewState>().get_playing(), a.ui.global::<ReviewState>().get_progress()));
    check(playing && progress > 0.05, &format!("one click on Play starts playback (progress {progress:.2})"));
    ask(|a| a.ui.global::<ReviewState>().invoke_play_pause());
    check(!ask(|a| a.ui.global::<ReviewState>().get_playing()), "pause stops playback");
    click(143.0, 377.0);
    check(ask(|a| a.ui.global::<ReviewState>().get_format_label()) == "WAV", "one click on WAV selects it");
    click(75.0, 377.0);

    ask(|a| a.ui.global::<ReviewState>().invoke_trim(0.1, 0.9));
    let (trimmed, text) = ask(|a| (a.ui.global::<ReviewState>().get_trimmed(), a.ui.global::<ReviewState>().get_trim_text()));
    check(trimmed && text.starts_with("Keeping"), &format!("trimming updates the summary: {text}"));

    let label = ask(|a| a.ui.global::<ReviewState>().get_format_label());
    check(label == "MP3", "MP3 is the default format");
    ask(|a| a.ui.global::<ReviewState>().invoke_copy());
    check(wait_until("copy", Duration::from_secs(10), || ask(|a| !a.ui.global::<ReviewState>().get_exporting())), "copy finishes");
    let (status, unsaved) = ask(|a| (a.ui.global::<ReviewState>().get_status(), a.ui.global::<ReviewState>().get_unsaved()));
    check(status.title.starts_with("Copied") && status.title.ends_with("WAV"), &format!("no MP3 encoder → copies a WAV instead: \"{}\" / \"{}\"", status.title, status.message));
    check(!unsaved, "after copying, the left button becomes New recording");

    ask(|a| a.ui.global::<ReviewState>().invoke_discard());
    let (screen, notice) = ask(|a| (a.ui.global::<AppState>().get_screen(), a.ui.global::<CaptureState>().get_notice()));
    check(screen == 0 && notice.title == "Recording discarded" && notice.action_text == "Undo", "New recording returns with an Undo banner");
    ask(|a| a.ui.global::<CaptureState>().invoke_notice_action());
    check(ask(|a| a.ui.global::<AppState>().get_screen()) == 1, "Undo brings the recording back");

    ask(|a| {
        let r = a.ui.global::<ReviewState>();
        r.invoke_set_format(1);
        r.invoke_set_quality(0);
        r.invoke_trim(0.25, 0.75);
    });
    let (label, quality) = ask(|a| (a.ui.global::<ReviewState>().get_format_label(), a.ui.global::<ReviewState>().get_quality_label()));
    check(label == "WAV" && quality == "Bit depth", "switching to WAV swaps the quality options");
    ask(|a| a.ui.global::<ReviewState>().invoke_save());
    check(
        wait_until("save", Duration::from_secs(10), || ask(|a| a.ui.global::<ReviewState>().get_status().title.starts_with("Saved"))),
        "Save writes the file",
    );
    let status = ask(|a| a.ui.global::<ReviewState>().get_status());
    let name = status.title.trim_start_matches("Saved ").to_string();
    let path = std::env::temp_dir().join("RecordThis-demo-saves").join(&name);
    let bytes = std::fs::read(&path).unwrap_or_default();
    let frames = bytes.len().saturating_sub(44) / 4; // 16-bit stereo
    let seconds = frames as f64 / 48000.0;
    check(bytes.starts_with(b"RIFF") && (seconds - 2.8).abs() < 0.2, &format!("{name}: 16-bit WAV with the trimmed half ({seconds:.2} s)"));

    ask(|a| a.ui.global::<ReviewState>().invoke_discard());
    let (screen, notice) = ask(|a| (a.ui.global::<AppState>().get_screen(), a.ui.global::<CaptureState>().get_notice()));
    check(screen == 0 && !notice.visible, "after saving, New recording just starts over");

    println!("Microphone tab");
    ask(|a| a.ui.global::<CaptureState>().invoke_select_source(1));
    let (live, text) = ask(|a| (a.ui.global::<CaptureState>().get_meter_live(), a.ui.global::<CaptureState>().get_level_text()));
    check(!live && text == "Not checking", "microphones aren't opened until asked");
    ask(|a| a.ui.global::<CaptureState>().invoke_toggle_mic_test());
    check(ask(|a| a.ui.global::<CaptureState>().get_meter_live()), "Test mic starts the meter");
    check(wait_until("mic level", Duration::from_secs(3), || ask(|a| a.ui.global::<CaptureState>().get_level_left()) > 0.1), "mic level moves");
    ask(|a| a.ui.global::<CaptureState>().invoke_toggle_mic_test());
    check(!ask(|a| a.ui.global::<CaptureState>().get_meter_live()), "Stop test closes the mic");

    println!("Closing with an unsaved recording");
    ask(|a| a.ui.global::<CaptureState>().invoke_toggle_recording());
    std::thread::sleep(Duration::from_millis(1200));
    ask(|a| a.ui.global::<CaptureState>().invoke_toggle_recording());
    let spool = ask(|a| {
        a.ui.window().dispatch_event(WindowEvent::CloseRequested);
        a.ui.global::<AppState>().get_close_dialog()
    });
    check(spool, "closing asks to save the recording first");
    ask(|a| a.ui.global::<AppState>().invoke_close_choice(1));
}

fn main() {
    // Fresh settings, so the run starts from the defaults.
    let home = std::env::temp_dir().join(format!("rt-selftest-{}", std::process::id()));
    std::env::set_var("HOME", &home);
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(slint::PhysicalSize::new(600, 580));
    let queue = Arc::new(Queue::default());
    slint::platform::set_platform(Box::new(Headless { window, queue })).unwrap();

    let spools_before = count_spools();
    let runner = std::thread::spawn(script);
    recordthis::app::run().expect("app failed");
    let script_ok = runner.join().is_ok();
    check(script_ok, "script completed");
    check(count_spools() == spools_before, "Don't save deleted the scratch file; nothing left behind");

    let failures = FAILURES.load(Ordering::SeqCst);
    println!("\n{}", if failures == 0 { "All checks passed.".to_string() } else { format!("{failures} check(s) failed.") });
    std::process::exit(if failures == 0 { 0 } else { 1 });
}

fn count_spools() -> usize {
    std::fs::read_dir(std::env::temp_dir())
        .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("RecordThis_")).count())
        .unwrap_or(0)
}
