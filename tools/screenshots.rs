//! Renders every main UI state to PNG with Slint's software renderer, using representative data.
//! For design review and the README; runs on any OS without a display.
//!
//!     cargo run --features screenshots --bin screenshots -- out-dir [scale]

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use recordthis::audio::waveform::envelope_path;
use recordthis::ui::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.clock.get()
    }
}

struct Shooter {
    ui: AppWindow,
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
    scale: f32,
    out: std::path::PathBuf,
}

impl Shooter {
    fn shot(&self, name: &str, logical_w: f32, logical_h: f32) {
        let (w, h) = ((logical_w * self.scale) as u32, (logical_h * self.scale) as u32);
        self.ui.window().set_size(slint::PhysicalSize::new(w, h));
        for _ in 0..30 {
            self.clock.set(self.clock.get() + Duration::from_millis(50));
            slint::platform::update_timers_and_animations();
        }
        self.window.request_redraw();
        let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
        self.window.draw_if_needed(|r| {
            r.render(&mut buf, w as usize);
        });
        let mut bytes = Vec::with_capacity(buf.len() * 4);
        for p in &buf {
            bytes.extend_from_slice(&[p.red, p.green, p.blue, 255]);
        }
        let file = std::fs::File::create(self.out.join(format!("{name}.png"))).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&bytes).unwrap();
        println!("wrote {name}.png ({w}×{h})");
    }
}

fn notice(title: &str, message: &str, severity: i32, action: &str, secondary: &str, closable: bool) -> NoticeData {
    NoticeData {
        visible: true,
        title: title.into(),
        message: message.into(),
        severity,
        action_text: action.into(),
        secondary_text: secondary.into(),
        closable,
    }
}

/// Speech-like envelope: phrases with gaps, like the user's screenshot.
fn speech_peaks(n: usize) -> Vec<f32> {
    let mut seed = 0x2545_F491u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let phrase = ((t * 23.0).sin() * 0.5 + 0.5) * ((t * 7.3 + 1.0).sin() * 0.5 + 0.6);
            let gate = if (t * 23.0).sin() < -0.55 || !(0.04..=0.95).contains(&t) { 0.0 } else { 1.0 };
            (phrase * gate * (0.55 + 0.45 * rnd())).min(1.0)
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = Path::new(args.get(1).map(String::as_str).unwrap_or("screenshots")).to_owned();
    let scale: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1.5);
    std::fs::create_dir_all(&out).unwrap();

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let clock = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(Headless { window: window.clone(), clock: clock.clone() })).unwrap();
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });

    let ui = AppWindow::new().unwrap();
    ui.set_font_family("Selawik".into());
    let theme = ui.global::<Theme>();
    // The user's Windows accent (yellow) as WinUI derives it.
    theme.set_accent_on_dark(slint::Color::from_rgb_u8(0xFF, 0xD5, 0x2E));
    theme.set_accent_on_light(slint::Color::from_rgb_u8(0x9A, 0x6E, 0x00));
    theme.set_text_on_accent_dark(slint::Color::from_rgb_u8(0, 0, 0));
    theme.set_text_on_accent_light(slint::Color::from_rgb_u8(255, 255, 255));
    theme.set_scheme_override(2);
    ui.show().unwrap();

    let s = Shooter { ui: ui.clone_strong(), window, clock, scale, out };
    let (w, h) = (600.0, 580.0);

    // ---------------------------------------------------------------- Recording screen
    let cs = ui.global::<CaptureState>();
    let devices = Rc::new(VecModel::from(vec![
        DeviceItem { name: "Yamaha AG03 (3- AG06/AG03)".into(), subtitle: "Everything this device plays · Windows default".into() },
        DeviceItem { name: "Speakers (Realtek(R) Audio)".into(), subtitle: "Everything this device plays".into() },
    ]));
    cs.set_devices(ModelRc::from(devices.clone()));
    cs.set_selected_device(0);
    cs.set_format_text("44.1 kHz · 32-bit float · stereo".into());
    cs.set_meter_live(true);
    cs.set_level_left(0.71);
    cs.set_level_right(0.66);
    cs.set_hold_left(0.78);
    cs.set_hold_right(0.74);
    cs.set_level_text("−17 dB".into());
    s.shot("1-record-idle", w, h);

    cs.set_recording(true);
    cs.set_elapsed_text("0:42".into());
    cs.set_level_left(0.83);
    cs.set_level_right(0.86);
    cs.set_hold_left(0.9);
    cs.set_hold_right(0.97);
    cs.set_clipping(true);
    cs.set_level_text("−1 dB".into());
    s.ui.set_window_title("● Recording 0:42 — RecordThis".into());
    s.shot("2-recording", w, h);

    cs.set_recording(false);
    cs.set_clipping(false);
    cs.set_source_kind(1);
    cs.set_devices(ModelRc::from(Rc::new(VecModel::from(vec![DeviceItem {
        name: "Microphone (Yamaha AG03)".into(),
        subtitle: "Microphone or line input · Windows default".into(),
    }]))));
    cs.set_format_text("48 kHz · 24-bit · mono".into());
    cs.set_mono(true);
    cs.set_meter_live(false);
    cs.set_level_text("Not checking".into());
    cs.set_level_left(0.0);
    cs.set_level_right(0.0);
    cs.set_recovery(notice(
        "Unsaved recording found",
        "4:12 from Yamaha AG03, started Nov 24 at 1:58 PM. RecordThis closed before it was saved.",
        2,
        "Recover it",
        "Delete it",
        false,
    ));
    s.shot("3-microphone-recovery", w, h);

    cs.set_recovery(NoticeData::default());
    cs.set_source_kind(0);
    cs.set_devices(ModelRc::from(devices.clone()));
    cs.set_format_text("44.1 kHz · 32-bit float · stereo".into());
    cs.set_mono(false);
    cs.set_meter_live(true);
    cs.set_level_text("Silent".into());
    cs.set_notice(notice("Recording discarded", "", 0, "Undo", "", false));
    s.shot("4-undo-discard", w, h);
    cs.set_notice(NoticeData::default());

    // ---------------------------------------------------------------- Review screen
    let app = ui.global::<AppState>();
    app.set_screen(1);
    s.ui.set_window_title("Review — RecordThis".into());
    let rs = ui.global::<ReviewState>();
    let peaks = speech_peaks(480);
    rs.set_title("Recording · 0:08".into());
    rs.set_subtitle("Yamaha AG03 (3- AG06/AG03) · 44.1 kHz · 32-bit float · stereo".into());
    rs.set_waveform(envelope_path(&peaks, 1.0).into());
    rs.set_progress(0.37);
    rs.set_position_text("0:03".into());
    rs.set_duration_text("0:08".into());
    rs.set_trim_start(0.04);
    rs.set_trim_end(0.93);
    rs.set_trimmed(true);
    rs.set_trim_start_label("0:00.3".into());
    rs.set_trim_end_label("0:07.4".into());
    rs.set_trim_text("Keeping 0:07.1 of 0:08.0  ·  0:00.3 → 0:07.4".into());
    rs.set_can_trim_silence(false);
    rs.set_trim_silence_text("No silence to trim".into());
    let q = Rc::new(VecModel::from(vec![SharedString::from("128 kbps · smaller"), "192 kbps".into(), "320 kbps · best".into()]));
    rs.set_qualities(ModelRc::from(q.clone()));
    rs.set_quality_index(1);
    rs.set_format(0);
    rs.set_format_label("MP3".into());
    rs.set_sample_rate_index(0);
    rs.set_unsaved(true);
    rs.set_prefer_copy(true);
    s.shot("5-review", w, h);

    rs.set_exporting(true);
    rs.set_export_progress(0.42);
    rs.set_export_text("Encoding MP3… 42%".into());
    s.shot("6-review-exporting", w, h);

    rs.set_exporting(false);
    rs.set_unsaved(false);
    rs.set_status(notice("Copied 412 KB MP3", "Paste it into Discord, WhatsApp, Teams or a folder.", 1, "", "", false));
    s.shot("7-review-copied", w, h);

    // Light theme, WAV, untrimmed, saved
    theme.set_scheme_override(1);
    rs.set_trim_start(0.0);
    rs.set_trim_end(1.0);
    rs.set_trimmed(false);
    rs.set_progress(0.0);
    rs.set_position_text("0:00".into());
    rs.set_trim_text("Drag the handles at either end to trim. Keys: [ and ] trim at the playhead.".into());
    rs.set_can_trim_silence(true);
    rs.set_trim_silence_text("Trim silence".into());
    rs.set_format(1);
    rs.set_format_label("WAV".into());
    q.set_vec(vec![SharedString::from("16-bit · CD"), "24-bit · studio".into()]);
    rs.set_quality_label("Bit depth".into());
    rs.set_prefer_copy(false);
    rs.set_status(notice("Saved RecordThis 2026-11-24 14.08.31.wav", "WAV · 44.1 kHz · 24-bit · 2.9 MB", 1, "Show in folder", "", false));
    rs.set_banner(notice(
        "Recording stopped: device disconnected",
        "Yamaha AG03 was unplugged or disabled. Everything captured up to that moment is here.",
        2,
        "",
        "",
        true,
    ));
    s.shot("8-review-light", w, h + 70.0);

    theme.set_scheme_override(2);
    rs.set_banner(NoticeData::default());
    rs.set_status(NoticeData::default());
    rs.set_unsaved(true);
    app.set_close_dialog(true);
    s.shot("9-close-dialog", w, h);
    let _ = q.row_count();
}
