//! Review screen: waveform with trimming, playback, MP3/WAV export and copy-to-clipboard.

use std::fs::File;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use super::{ms, notice_data, run_shell_action, text, App, Screen};
use crate::audio::capture::{CaptureResult, StopReason};
use crate::audio::downmix::{clip_safe_gain, requires_downmix};
use crate::audio::export::{export, ExportFormat, ExportOptions, ExportResult};
use crate::audio::playback::Player;
use crate::audio::reader::SpoolReader;
use crate::audio::waveform::{self, WaveformData};
use crate::errors::ExportError;
use crate::notice::{Notice, NoticeAction, Severity};
use crate::platform;
use crate::storage::settings::AppSettings;
use crate::storage::spool::{self, SpoolSession};
use crate::ui::ReviewState;

const BUCKETS: usize = 480;
const MIN_SELECTION: Duration = Duration::from_millis(200);
const SILENCE_LEAD_IN: Duration = Duration::from_millis(150);
const SILENCE_TAIL: Duration = Duration::from_millis(300);
const STATUS_LIFETIME: Duration = Duration::from_secs(6);
const SAMPLE_RATES: [u32; 2] = [44100, 48000];
const BIT_DEPTHS: [u16; 2] = [16, 24];
const BIT_RATES: [u32; 3] = [128, 192, 320];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExportKind {
    Save,
    Copy,
}

pub struct ReviewUi {
    session: Option<SpoolSession>,
    result: Option<CaptureResult>,
    hold: Option<File>,
    player: Option<Rc<Player>>,
    analysis: Option<WaveformData>,
    analyzing: bool,
    analysis_cancel: Arc<AtomicBool>,
    gain: f32,
    trim: (f64, f64),
    min_selection: f64,
    format: ExportFormat,
    sample_rate: u32,
    bit_depth: u16,
    kbps: u32,
    pub exporting: bool,
    /// Export waiting for the waveform pass (it needs the downmix peak).
    queued: Option<(ExportKind, Option<PathBuf>, bool)>,
    saved: bool,
    copied: bool,
    prefer_copy: bool,
    banner: Option<Notice>,
    status: Option<Notice>,
    position_timer: Timer,
    status_timer: Timer,
    qualities: Rc<VecModel<SharedString>>,
}

impl ReviewUi {
    pub fn new(settings: &AppSettings) -> Self {
        Self {
            session: None,
            result: None,
            hold: None,
            player: None,
            analysis: None,
            analyzing: false,
            analysis_cancel: Arc::new(AtomicBool::new(false)),
            gain: 1.0,
            trim: (0.0, 1.0),
            min_selection: 0.01,
            format: if settings.export_format.eq_ignore_ascii_case("wav") { ExportFormat::Wav } else { ExportFormat::Mp3 },
            sample_rate: 48000,
            bit_depth: if BIT_DEPTHS.contains(&settings.bit_depth) { settings.bit_depth } else { 24 },
            kbps: if BIT_RATES.contains(&settings.mp3_bit_rate) { settings.mp3_bit_rate } else { 192 },
            exporting: false,
            queued: None,
            saved: false,
            copied: false,
            prefer_copy: settings.prefer_copy,
            banner: None,
            status: None,
            position_timer: Timer::default(),
            status_timer: Timer::default(),
            qualities: Rc::new(VecModel::default()),
        }
    }

    /// There's a recording on screen that hasn't been saved or copied anywhere.
    pub fn unsaved_and_unused(&self) -> bool {
        self.session.is_some() && !self.saved && !self.copied
    }

    /// The global shortcut shouldn't throw this recording away.
    pub fn needs_attention(&self) -> bool {
        self.unsaved_and_unused() || self.exporting
    }

    fn is_trimmed(&self) -> bool {
        self.trim.0 > 0.0005 || self.trim.1 < 0.9995
    }

    fn total_frames(&self) -> u64 {
        self.session.as_ref().map(|s| s.frame_count()).unwrap_or(0)
    }

    fn trim_frames(&self) -> (u64, u64) {
        let total = self.player.as_ref().map(|p| p.total_frames()).unwrap_or_else(|| self.total_frames());
        let start = (self.trim.0 * total as f64).floor() as u64;
        let end = (self.trim.1 * total as f64).ceil() as u64;
        (start.min(total), end.min(total))
    }

    fn silence_bounds(&self) -> Option<(f64, f64)> {
        let data = self.analysis.as_ref()?;
        let (first, last) = data.sound?;
        let rate = self.session.as_ref()?.format.sample_rate as f64;
        let total = data.total_frames as f64;
        let start = (first as f64 - SILENCE_LEAD_IN.as_secs_f64() * rate).max(0.0) / total;
        let end = (last as f64 + SILENCE_TAIL.as_secs_f64() * rate).min(total) / total;
        Some((start, end.max((start + self.min_selection).min(1.0))))
    }
}

impl App {
    fn rs(&self) -> ReviewState<'_> {
        self.ui.global::<ReviewState>()
    }

    pub(super) fn wire_review(self: &Rc<Self>) {
        let s = self.rs();
        s.set_qualities(ModelRc::from(self.rev.borrow().qualities.clone()));
        s.set_buckets(BUCKETS as i32);
        macro_rules! cb0 {
            ($method:ident, $call:ident $(, $arg:expr)*) => {{
                let weak = Rc::downgrade(self);
                s.$method(move || {
                    if let Some(app) = weak.upgrade() {
                        app.$call($($arg),*);
                    }
                });
            }};
        }
        macro_rules! cb1 {
            ($method:ident, $call:ident) => {{
                let weak = Rc::downgrade(self);
                s.$method(move |v| {
                    if let Some(app) = weak.upgrade() {
                        app.$call(v);
                    }
                });
            }};
        }
        cb0!(on_play_pause, play_pause);
        cb1!(on_seek, seek_to);
        cb1!(on_seek_by, seek_by);
        cb1!(on_set_trim_at_playhead, set_trim_at_playhead);
        cb0!(on_trim_silence, trim_silence);
        cb0!(on_reset_trim, reset_trim);
        cb1!(on_set_format, set_format);
        cb1!(on_set_sample_rate, set_sample_rate);
        cb1!(on_set_quality, set_quality);
        cb0!(on_copy, copy);
        cb0!(on_save, save, false);
        cb0!(on_discard, discard);
        cb0!(on_banner_action, banner_action);
        cb0!(on_dismiss_banner, set_banner, None);
        cb0!(on_status_action, status_action);
        let weak = Rc::downgrade(self);
        s.on_trim(move |a, b| {
            if let Some(app) = weak.upgrade() {
                app.set_trim(a as f64, b as f64);
            }
        });
    }

    pub(super) fn init_review(&self) {
        self.sync_export_choices();
    }

    // ------------------------------------------------------------------ Session lifecycle

    pub(super) fn open_review(self: &Rc<Self>, result: CaptureResult) {
        if self.rev.borrow().session.is_some() {
            self.release_review(true);
        }
        let session = result.session.clone();
        let duration = session.duration();
        let total = session.frame_count();
        {
            let mut rev = self.rev.borrow_mut();
            rev.hold = spool::hold(&session.raw_path);
            rev.session = Some(session.clone());
            rev.result = Some(result.clone());
            rev.analysis = None;
            rev.gain = 1.0;
            rev.saved = false;
            rev.copied = false;
            rev.trim = (0.0, 1.0);
            rev.min_selection = if duration.is_zero() { 1.0 } else { (MIN_SELECTION.as_secs_f64() / duration.as_secs_f64()).min(1.0) };
            // Default to the capture's rate family so the common case needs no resampling.
            rev.sample_rate = if session.format.sample_rate.is_multiple_of(11025) { 44100 } else { 48000 };
        }

        let s = self.rs();
        s.set_title(format!("Recording · {}", text::clock(duration)).into());
        s.set_subtitle(self.details_text(&session, 1.0).into());
        s.set_waveform(SharedString::new());
        s.set_progress(0.0);
        s.set_position_text("0:00".into());
        s.set_duration_text(text::clock(duration).into());
        s.set_playing(false);
        s.set_min_selection(self.rev.borrow().min_selection as f32);
        self.set_status(None);
        self.set_banner(banner_for(&result));
        self.sync_export_choices();
        self.sync_trim();
        self.sync_unsaved();

        let on_stopped: crate::audio::playback::StoppedCallback = Arc::new(move |error| {
            super::post(move |app| app.on_playback_stopped(error));
        });
        match Player::load(&session, 1.0, on_stopped) {
            Ok(player) => {
                self.rev.borrow_mut().player = Some(Rc::new(player));
                s.set_can_play(true);
            }
            Err(e) => {
                s.set_can_play(false);
                self.set_banner(Some(Notice::new("Preview isn't available", format!("{} You can still save the recording.", e.describe()), Severity::Warning)));
            }
        }
        self.apply_trim_to_player();
        let _ = total;

        self.start_analysis(session);
        self.set_screen(Screen::Review);
    }

    fn details_text(&self, session: &SpoolSession, gain: f32) -> String {
        let mut details = format!("{} · {}", session.device_name, session.format.describe());
        if requires_downmix(session.format.channels) {
            details += " · Saved as stereo";
        }
        if gain < 1.0 {
            details += &format!(" · Lowered {:.1} dB to avoid clipping", -20.0 * gain.log10());
        }
        details
    }

    fn start_analysis(self: &Rc<Self>, session: SpoolSession) {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut rev = self.rev.borrow_mut();
            rev.analysis_cancel.store(true, Ordering::Relaxed);
            rev.analysis_cancel = cancel.clone();
            rev.analyzing = true;
        }
        self.rs().set_analyzing(true);
        self.sync_trim();
        let id = session.id;
        std::thread::spawn(move || {
            let result = SpoolReader::open(&session.raw_path, session.format)
                .and_then(|reader| waveform::analyze(reader, session.format.channel_mask, BUCKETS, &cancel));
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            super::post(move |app| app.analysis_done(id, result));
        });
    }

    fn analysis_done(self: &Rc<Self>, id: uuid::Uuid, result: std::io::Result<Option<WaveformData>>) {
        let Some(session) = self.rev.borrow().session.clone().filter(|s| s.id == id) else { return };
        self.rev.borrow_mut().analyzing = false;
        self.rs().set_analyzing(false);
        match result {
            Ok(Some(data)) => {
                let gain = clip_safe_gain(data.mixed_peak);
                // Show exactly what will be exported (post-downmix, post-gain).
                self.rs().set_waveform(waveform::envelope_path(&data.peaks, gain).into());
                if let Some(p) = &self.rev.borrow().player {
                    p.set_gain(gain);
                }
                self.rs().set_subtitle(self.details_text(&session, gain).into());
                let mut rev = self.rev.borrow_mut();
                rev.gain = gain;
                rev.analysis = Some(data);
            }
            Ok(None) => {}
            Err(e) => {
                if self.rev.borrow().banner.is_none() {
                    self.set_banner(Some(Notice::new(
                        "Couldn't draw the waveform",
                        format!("{} Playback and saving may still work.", crate::errors::AudioError::from(e).describe()),
                        Severity::Warning,
                    )));
                }
            }
        }
        self.sync_trim();
        let queued = self.rev.borrow_mut().queued.take();
        if let Some((kind, path, close_after)) = queued {
            self.rev.borrow_mut().exporting = false;
            self.run_export(kind, path, close_after);
        }
    }

    /// Deletes the spool and returns to the recording screen without asking.
    pub(super) fn close_review_session(self: &Rc<Self>) {
        self.release_review(true);
        self.set_screen(Screen::Capture);
    }

    fn release_review(&self, delete_spool: bool) {
        let session = {
            let mut rev = self.rev.borrow_mut();
            rev.analysis_cancel.store(true, Ordering::Relaxed);
            rev.analyzing = false;
            rev.analysis = None;
            rev.position_timer.stop();
            rev.player = None;
            rev.hold = None;
            rev.result = None;
            rev.queued = None;
            rev.session.take()
        };
        if delete_spool {
            if let Some(s) = &session {
                self.spools.delete(s);
            }
        }
        let s = self.rs();
        s.set_playing(false);
        s.set_waveform(SharedString::new());
        s.set_analyzing(false);
        self.set_banner(None);
        self.set_status(None);
    }

    /// On exit an unsaved spool is deliberately left on disk, so it's offered for recovery next launch.
    pub(super) fn release_review_for_exit(&self) {
        let delete = {
            let rev = self.rev.borrow();
            rev.session.is_some() && (rev.saved || rev.copied)
        };
        self.release_review(delete);
    }

    pub(super) fn show_finish_first(self: &Rc<Self>) {
        let gesture = self.ui.global::<crate::ui::CaptureState>().get_hotkey_gesture();
        self.set_status(Some(
            Notice::new("Finish with this recording first", format!("Copy, save or discard it, then press {gesture} to record again."), Severity::Info)
                .lasting(STATUS_LIFETIME),
        ));
    }

    // ------------------------------------------------------------------ Playback

    fn play_pause(self: &Rc<Self>) {
        let Some(player) = self.rev.borrow().player.clone() else { return };
        if player.is_playing() {
            player.pause();
            self.rev.borrow().position_timer.stop();
            self.rs().set_playing(false);
            self.update_position();
        } else {
            player.play();
            self.rs().set_playing(true);
            let weak = Rc::downgrade(self);
            self.rev.borrow().position_timer.start(TimerMode::Repeated, ms(33), move || {
                if let Some(app) = weak.upgrade() {
                    app.update_position();
                }
            });
        }
    }

    fn pause_playback(&self) {
        if let Some(p) = &self.rev.borrow().player {
            if p.is_playing() {
                p.pause();
            }
        }
        self.rev.borrow().position_timer.stop();
        self.rs().set_playing(false);
    }

    fn on_playback_stopped(self: &Rc<Self>, error: Option<crate::errors::AudioError>) {
        self.rev.borrow().position_timer.stop();
        self.rs().set_playing(false);
        self.update_position();
        if error.is_some() {
            self.set_banner(Some(Notice::new(
                "Playback stopped",
                "The output device changed or was disconnected. Press Play to continue on the current device.",
                Severity::Warning,
            )));
        }
    }

    fn update_position(&self) {
        let Some(player) = self.rev.borrow().player.clone() else { return };
        let total = player.total_frames().max(1);
        let pos = player.position();
        let s = self.rs();
        s.set_progress(pos as f32 / total as f32);
        let t = text::clock(text::frames_to_duration(pos, player.sample_rate()));
        if s.get_position_text() != t.as_str() {
            s.set_position_text(t.into());
        }
    }

    fn seek_to(&self, fraction: f32) {
        let Some(player) = self.rev.borrow().player.clone() else { return };
        player.seek((fraction.clamp(0.0, 1.0) as f64 * player.total_frames() as f64) as u64);
        self.update_position();
    }

    fn seek_by(&self, delta: f32) {
        let progress = self.rs().get_progress();
        self.seek_to(progress + delta);
    }

    // ------------------------------------------------------------------ Trimming

    fn set_trim(&self, start: f64, end: f64) {
        {
            let mut rev = self.rev.borrow_mut();
            let min = rev.min_selection;
            let start = start.clamp(0.0, (1.0 - min).max(0.0));
            let end = end.clamp((start + min).min(1.0), 1.0);
            rev.trim = (start, end);
        }
        self.apply_trim_to_player();
        self.sync_trim();
    }

    fn set_trim_at_playhead(&self, start: bool) {
        let p = self.rs().get_progress() as f64;
        let (a, b) = self.rev.borrow().trim;
        let min = self.rev.borrow().min_selection;
        if start {
            self.set_trim(p.min((b - min).max(0.0)), b);
        } else {
            self.set_trim(a, p.max((a + min).min(1.0)));
        }
    }

    fn trim_silence(&self) {
        let bounds = self.rev.borrow().silence_bounds();
        if let Some((a, b)) = bounds {
            self.set_trim(a, b);
        }
    }

    fn reset_trim(&self) {
        self.set_trim(0.0, 1.0);
    }

    fn apply_trim_to_player(&self) {
        let rev = self.rev.borrow();
        if let Some(p) = &rev.player {
            let (a, b) = rev.trim_frames();
            p.set_range(a, b);
        }
    }

    fn sync_trim(&self) {
        let rev = self.rev.borrow();
        let s = self.rs();
        let Some(session) = &rev.session else { return };
        let rate = session.format.sample_rate;
        let total = rev.total_frames();
        let (a, b) = rev.trim;
        s.set_trim_start(a as f32);
        s.set_trim_end(b as f32);
        s.set_trimmed(rev.is_trimmed());
        let at = |f: f64| text::frames_to_duration((f * total as f64) as u64, rate);
        s.set_trim_start_label(text::precise(at(a)).into());
        s.set_trim_end_label(text::precise(at(b)).into());
        s.set_trim_text(if rev.is_trimmed() {
            format!("Keeping {} of {}  ·  {} → {}", text::precise(at(b - a)), text::precise(at(1.0)), text::precise(at(a)), text::precise(at(b))).into()
        } else {
            "Drag the handles at either end to trim. Keys: [ and ] trim at the playhead.".into()
        });

        let (can, label) = if rev.analyzing {
            (false, "Trim silence")
        } else {
            match (&rev.analysis, rev.silence_bounds()) {
                (Some(d), _) if d.sound.is_none() => (false, "No sound found"),
                (Some(_), Some((x, y))) if (x - a).abs() > 0.0005 || (y - b).abs() > 0.0005 => (true, "Trim silence"),
                (Some(_), _) => (false, "No silence to trim"),
                (None, _) => (false, "Trim silence"),
            }
        };
        s.set_can_trim_silence(can);
        s.set_trim_silence_text(label.into());
    }

    // ------------------------------------------------------------------ Export choices

    fn sync_export_choices(&self) {
        let rev = self.rev.borrow();
        let s = self.rs();
        s.set_format(if rev.format == ExportFormat::Mp3 { 0 } else { 1 });
        s.set_format_label(rev.format.label().into());
        s.set_sample_rate_index(SAMPLE_RATES.iter().position(|&r| r == rev.sample_rate).unwrap_or(1) as i32);
        let (labels, index, title): (Vec<SharedString>, usize, &str) = match rev.format {
            ExportFormat::Mp3 => (
                vec!["128 kbps · smaller".into(), "192 kbps".into(), "320 kbps · best".into()],
                BIT_RATES.iter().position(|&k| k == rev.kbps).unwrap_or(1),
                "Quality",
            ),
            ExportFormat::Wav => (
                vec!["16-bit · CD".into(), "24-bit · studio".into()],
                BIT_DEPTHS.iter().position(|&b| b == rev.bit_depth).unwrap_or(1),
                "Bit depth",
            ),
        };
        super::set_model(&rev.qualities, labels);
        s.set_quality_index(index as i32);
        s.set_quality_label(title.into());
        s.set_prefer_copy(rev.prefer_copy);
    }

    fn set_format(&self, index: i32) {
        let format = if index == 0 { ExportFormat::Mp3 } else { ExportFormat::Wav };
        self.rev.borrow_mut().format = format;
        {
            let mut settings = self.settings.borrow_mut();
            settings.current.export_format = format.extension().into();
            settings.save();
        }
        self.sync_export_choices();
    }

    fn set_sample_rate(&self, index: i32) {
        if let Some(&rate) = SAMPLE_RATES.get(index as usize) {
            self.rev.borrow_mut().sample_rate = rate;
            self.sync_export_choices();
        }
    }

    fn set_quality(&self, index: i32) {
        let format = self.rev.borrow().format;
        let mut settings = self.settings.borrow_mut();
        match format {
            ExportFormat::Mp3 => {
                if let Some(&k) = BIT_RATES.get(index as usize) {
                    self.rev.borrow_mut().kbps = k;
                    settings.current.mp3_bit_rate = k;
                }
            }
            ExportFormat::Wav => {
                if let Some(&b) = BIT_DEPTHS.get(index as usize) {
                    self.rev.borrow_mut().bit_depth = b;
                    settings.current.bit_depth = b;
                }
            }
        }
        settings.save();
        drop(settings);
        self.sync_export_choices();
    }

    fn set_prefer_copy(&self, prefer: bool) {
        self.rev.borrow_mut().prefer_copy = prefer;
        let mut settings = self.settings.borrow_mut();
        settings.current.prefer_copy = prefer;
        settings.save();
        drop(settings);
        self.rs().set_prefer_copy(prefer);
    }

    fn sync_unsaved(&self) {
        let rev = self.rev.borrow();
        self.rs().set_unsaved(!rev.saved && !rev.copied);
    }

    // ------------------------------------------------------------------ Save / Copy

    fn options(&self, format: ExportFormat) -> ExportOptions {
        let rev = self.rev.borrow();
        let (start, end) = rev.trim_frames();
        ExportOptions {
            format,
            sample_rate: rev.sample_rate,
            bit_depth: if format == ExportFormat::Wav { rev.bit_depth } else { 16 },
            mp3_kbps: rev.kbps,
            start_frame: start,
            end_frame: end,
        }
    }

    pub(super) fn save(self: &Rc<Self>, close_after: bool) {
        let (session, format, exporting) = {
            let rev = self.rev.borrow();
            (rev.session.clone(), rev.format, rev.exporting)
        };
        let Some(session) = session else { return };
        if exporting {
            return;
        }
        let weak = Rc::downgrade(self);
        let label = match format {
            ExportFormat::Mp3 => "MP3 audio",
            ExportFormat::Wav => "WAV audio",
        };
        let window = self.ui.as_weak();
        let _ = slint::spawn_local(async move {
            let Some(ui) = window.upgrade() else { return };
            let path = platform::pick_save_path(ui.window(), session.base_file_name(), format.extension(), label).await;
            let Some(app) = weak.upgrade() else { return };
            let same = app.rev.borrow().session.as_ref().is_some_and(|s| s.id == session.id);
            match path {
                Some(path) if same => app.run_export(ExportKind::Save, Some(path), close_after),
                _ => {} // cancelled
            }
        });
    }

    fn copy(self: &Rc<Self>) {
        if self.rev.borrow().session.is_none() || self.rev.borrow().exporting {
            return;
        }
        self.run_export(ExportKind::Copy, None, false);
    }

    /// Shared busy/progress/error handling for Save and Copy. Runs the encoder on a worker thread.
    fn run_export(self: &Rc<Self>, kind: ExportKind, path: Option<PathBuf>, close_after: bool) {
        self.pause_playback();
        let Some(session) = self.rev.borrow().session.clone() else { return };
        let format = self.rev.borrow().format;
        {
            let mut rev = self.rev.borrow_mut();
            rev.exporting = true;
            if rev.analyzing {
                // The clip-safe gain needs the downmix peak: run as soon as the waveform pass ends.
                rev.queued = Some((kind, path, close_after));
                drop(rev);
                self.set_status(None);
                let s = self.rs();
                s.set_exporting(true);
                s.set_export_progress(0.0);
                s.set_export_text("Preparing…".into());
                return;
            }
        }
        self.set_status(None);
        let s = self.rs();
        s.set_exporting(true);
        s.set_export_progress(0.0);
        let verb = match format {
            ExportFormat::Mp3 => "Encoding MP3",
            ExportFormat::Wav => "Writing WAV",
        };
        s.set_export_text(format!("{verb}…").into());

        let mixed_peak = self.rev.borrow().analysis.as_ref().map(|a| a.mixed_peak).unwrap_or(1.0);
        let options = self.options(format);
        let fallback_options = ExportOptions { format: ExportFormat::Wav, bit_depth: 16, ..options.clone() };
        let id = session.id;
        let base = session.base_file_name();
        let verb = verb.to_string();
        std::thread::spawn(move || {
            let cancel = AtomicBool::new(false);
            let mut progress = {
                let verb = verb.clone();
                move |p: f64| {
                    let verb = verb.clone();
                    super::post(move |app| {
                        let s = app.rs();
                        s.set_export_progress(p as f32);
                        s.set_export_text(format!("{verb}… {:.0}%", p * 100.0).into());
                    });
                }
            };
            let outcome = match kind {
                ExportKind::Save => export(&session, mixed_peak, &options, path.as_ref().unwrap(), &mut progress, &cancel).map(|r| (r, None)),
                ExportKind::Copy => {
                    let dest = platform::clipboard_temp_path(&format!("{base}.{}", options.format.extension()));
                    match export(&session, mixed_peak, &options, &dest, &mut progress, &cancel) {
                        Err(ExportError::Mp3Unavailable) => {
                            // Windows "N" editions ship without the MP3 encoder: fall back to a 16-bit WAV.
                            let dest = platform::clipboard_temp_path(&format!("{base}.wav"));
                            export(&session, mixed_peak, &fallback_options, &dest, &mut progress, &cancel)
                                .map(|r| (r, Some("This PC has no MP3 encoder, so a WAV was copied instead.".to_string())))
                        }
                        other => other.map(|r| (r, None)),
                    }
                }
            };
            super::post(move |app| app.export_done(id, kind, options, outcome, close_after));
        });
    }

    fn export_done(self: &Rc<Self>, id: uuid::Uuid, kind: ExportKind, options: ExportOptions, outcome: Result<(ExportResult, Option<String>), ExportError>, close_after: bool) {
        self.rev.borrow_mut().exporting = false;
        self.rs().set_exporting(false);
        if !self.rev.borrow().session.as_ref().is_some_and(|s| s.id == id) {
            return;
        }
        match outcome {
            Ok((result, note)) => match kind {
                ExportKind::Save => {
                    self.rev.borrow_mut().saved = true;
                    self.set_prefer_copy(false);
                    self.sync_unsaved();
                    let quality = match options.format {
                        ExportFormat::Mp3 => format!("MP3 · {} kbps", options.mp3_kbps),
                        ExportFormat::Wav => format!("WAV · {} · {}-bit", crate::audio::format::rate_label(options.sample_rate), options.bit_depth),
                    };
                    let name = result.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.set_status(Some(
                        Notice::new(format!("Saved {name}"), format!("{quality} · {}{}", text::size(result.file_size), if result.downmixed { " · downmixed to stereo" } else { "" }), Severity::Success)
                            .with_action(NoticeAction::RevealFile(result.path.clone()), "Show in folder"),
                    ));
                    if close_after {
                        self.quit();
                    }
                }
                ExportKind::Copy => {
                    let label = if note.is_some() { "WAV" } else { options.format.label() };
                    match platform::copy_file_to_clipboard(self.ui.window(), &result.path) {
                        Ok(()) => {
                            self.rev.borrow_mut().copied = true;
                            self.set_prefer_copy(true);
                            self.sync_unsaved();
                            self.set_status(Some(
                                Notice::new(
                                    format!("Copied {} {label}", text::size(result.file_size)),
                                    note.unwrap_or_else(|| "Paste it into Discord, WhatsApp, Teams or a folder.".into()),
                                    Severity::Success,
                                )
                                .lasting(STATUS_LIFETIME),
                            ));
                        }
                        Err(e) => self.set_status(Some(Notice::new("Couldn't use the clipboard", e, Severity::Error))),
                    }
                }
            },
            Err(ExportError::Cancelled) => {}
            Err(ExportError::Mp3Unavailable) => self.set_status(Some(Notice::new(
                "MP3 isn't available on this PC",
                "Windows here has no MP3 encoder. Choose WAV, or install the Media Feature Pack from Settings › Apps › Optional features.",
                Severity::Error,
            ))),
            Err(ExportError::TooLongForWav) => self.set_status(Some(Notice::new(
                "Too long for a single WAV file",
                "WAV files are limited to 4 GB. Choose MP3, 16-bit or 44.1 kHz to make the file smaller.",
                Severity::Error,
            ))),
            Err(ExportError::PermissionDenied) => self.set_status(Some(Notice::new(
                "Couldn't save there",
                "RecordThis doesn't have permission to write to that folder. Choose another location; your recording is still here.",
                Severity::Error,
            ))),
            Err(ExportError::Other(message)) => {
                self.set_status(Some(Notice::new("Something went wrong", format!("{message} Your recording is still here, so try again."), Severity::Error)))
            }
        }
    }

    // ------------------------------------------------------------------ Discard / New recording

    /// Not saved: hand the recording to the recording screen, which offers Undo for 5 seconds
    /// before deleting it. Saved: nothing to lose, so just start over.
    fn discard(self: &Rc<Self>) {
        if self.rev.borrow().exporting {
            return;
        }
        let (saved, result) = {
            let rev = self.rev.borrow();
            (rev.saved, rev.result.clone())
        };
        match result {
            Some(result) if !saved => {
                self.release_review(false);
                self.set_screen(Screen::Capture);
                self.offer_undo_discard(result);
            }
            _ => self.close_review_session(),
        }
    }

    // ------------------------------------------------------------------ Notices

    fn set_banner(&self, notice: Option<Notice>) {
        self.rs().set_banner(notice_data(notice.as_ref()));
        self.rev.borrow_mut().banner = notice;
    }

    fn banner_action(&self) {
        let action = self.rev.borrow().banner.as_ref().map(|n| n.action.clone());
        if let Some(a) = action {
            run_shell_action(&a);
        }
    }

    fn set_status(&self, notice: Option<Notice>) {
        let lifetime = notice.as_ref().and_then(|n| n.lifetime);
        self.rs().set_status(notice_data(notice.as_ref()));
        let mut rev = self.rev.borrow_mut();
        rev.status = notice;
        rev.status_timer.stop();
        if let Some(lifetime) = lifetime {
            // A plain closure: timers fire on the UI thread.
            rev.status_timer.start(TimerMode::SingleShot, lifetime, || super::with_app(|app| app.set_status(None)));
        }
    }

    fn status_action(&self) {
        let action = self.rev.borrow().status.as_ref().map(|n| n.action.clone());
        if let Some(a) = action {
            run_shell_action(&a);
        }
    }
}

fn banner_for(result: &CaptureResult) -> Option<Notice> {
    let device = &result.session.device_name;
    Some(match result.reason {
        StopReason::DeviceLost => Notice::new(
            "Recording stopped: device disconnected",
            format!("{device} was unplugged or disabled. Everything captured up to that moment is here."),
            Severity::Warning,
        ),
        StopReason::SystemSuspend => Notice::new(
            "Recording stopped before sleep",
            "Your PC went to sleep, so RecordThis finished the recording first. Nothing captured before that was lost.",
            Severity::Info,
        ),
        StopReason::Error => Notice::new(
            "Recording stopped unexpectedly",
            format!(
                "{} Everything captured before the problem is here.",
                result.error.as_ref().map(|e| e.describe()).unwrap_or_else(|| "An error occurred.".into())
            ),
            Severity::Warning,
        ),
        StopReason::Recovered => Notice::new(
            "Recovered recording",
            "Rebuilt from the scratch file a previous session left behind. Save it or discard it.",
            Severity::Info,
        ),
        StopReason::User => return None,
    })
}
