//! Recording screen: source tabs, device list with hot-plug, live meter, start/stop, crash
//! recovery and the undo-discard banner.

use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use uuid::Uuid;

use super::{capture_event_handler, ms, notice_data, run_shell_action, set_model, text, App, Screen};
use crate::audio::capture::{CaptureResult, Mode, StopReason};
use crate::errors::AudioError;
use crate::notice::{Notice, NoticeAction, Severity};
use crate::platform::{self, DeviceEvent, DeviceInfo};
use crate::storage::settings::AppSettings;
use crate::storage::spool::SpoolSession;
use crate::ui::{CaptureState, DeviceItem, NoticeData};

const METER_FLOOR_DB: f32 = -60.0;
const DECAY_PER_TICK: f32 = 0.963; // ≈ 20 dB/s release at 60 fps
const HOLD_TIME: Duration = Duration::from_millis(1200);
const CLIP_HOLD: Duration = Duration::from_millis(1500);
const UNDO_WINDOW: Duration = Duration::from_secs(5);

pub struct CaptureUi {
    pub all_devices: Vec<DeviceInfo>,
    pub visible: Vec<DeviceInfo>,
    pub selected: Option<usize>,
    pub output_tab: bool,
    pub recording: bool,
    pub busy: bool,
    pub mic_testing: bool,
    pub handled_sessions: HashSet<Uuid>,
    pub recovery_queue: VecDeque<SpoolSession>,
    pub pending_recovery: Option<SpoolSession>,
    pub pending_discard: Option<CaptureResult>,
    pub refresh_pending: bool,
    pub notice: Option<Notice>,
    meter: MeterState,
    devices_model: Rc<VecModel<DeviceItem>>,
    meter_timer: Timer,
    refresh_timer: Timer,
    notice_timer: Timer,
}

#[derive(Default)]
struct MeterState {
    display: [f32; 2],
    hold: [f32; 2],
    hold_until: [Option<Instant>; 2],
    clip_until: Option<Instant>,
    live: bool,
}

impl CaptureUi {
    pub fn new(settings: &AppSettings) -> Self {
        Self {
            all_devices: Vec::new(),
            visible: Vec::new(),
            selected: None,
            output_tab: settings.source_kind != "Input",
            recording: false,
            busy: false,
            mic_testing: false,
            handled_sessions: HashSet::new(),
            recovery_queue: VecDeque::new(),
            pending_recovery: None,
            pending_discard: None,
            refresh_pending: false,
            notice: None,
            meter: MeterState::default(),
            devices_model: Rc::new(VecModel::default()),
            meter_timer: Timer::default(),
            refresh_timer: Timer::default(),
            notice_timer: Timer::default(),
        }
    }

    pub fn selected_device(&self) -> Option<DeviceInfo> {
        self.selected.and_then(|i| self.visible.get(i)).cloned()
    }
}

fn level_position(peak: f32) -> f32 {
    if peak <= 1e-6 {
        return 0.0;
    }
    let db = 20.0 * peak.log10();
    ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

impl App {
    fn cs(&self) -> CaptureState<'_> {
        self.ui.global::<CaptureState>()
    }

    pub(super) fn wire_capture(self: &Rc<Self>) {
        let s = self.cs();
        s.set_devices(ModelRc::from(self.cap.borrow().devices_model.clone()));
        macro_rules! cb {
            ($method:ident, $body:expr) => {{
                let weak = Rc::downgrade(self);
                s.$method(move || {
                    if let Some(app) = weak.upgrade() {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)(&app);
                    }
                });
            }};
        }
        let weak = Rc::downgrade(self);
        s.on_select_source(move |i| {
            if let Some(app) = weak.upgrade() {
                app.select_source(i == 0);
            }
        });
        let weak = Rc::downgrade(self);
        s.on_select_device(move |i| {
            if let Some(app) = weak.upgrade() {
                app.select_device(i);
            }
        });
        cb!(on_toggle_recording, |app: &Rc<App>| app.toggle_recording());
        cb!(on_toggle_mic_test, |app: &Rc<App>| app.toggle_mic_test());
        cb!(on_recover, |app: &Rc<App>| app.recover());
        cb!(on_discard_recovery, |app: &Rc<App>| app.discard_recovery());
        cb!(on_notice_action, |app: &Rc<App>| app.capture_notice_action());
        cb!(on_dismiss_notice, |app: &Rc<App>| app.set_capture_notice(None));
    }

    pub(super) fn init_capture(self: &Rc<Self>) {
        self.cs().set_source_kind(if self.cap.borrow().output_tab { 0 } else { 1 });
        self.refresh_devices();
        self.check_for_recoverable_sessions();
    }

    // ------------------------------------------------------------------ Devices

    fn select_source(self: &Rc<Self>, output: bool) {
        if self.cap.borrow().recording || self.cap.borrow().output_tab == output {
            return;
        }
        self.stop_monitoring();
        self.cap.borrow_mut().output_tab = output;
        {
            let mut settings = self.settings.borrow_mut();
            settings.current.source_kind = if output { "Output" } else { "Input" }.into();
            settings.save();
        }
        self.cs().set_source_kind(if output { 0 } else { 1 });
        self.apply_source_filter();
        self.start_monitoring_if_wanted();
    }

    fn select_device(self: &Rc<Self>, index: i32) {
        if self.cap.borrow().recording || index < 0 {
            return;
        }
        let changed = self.cap.borrow().selected != Some(index as usize);
        if !changed {
            return;
        }
        let was_testing = self.cap.borrow().mic_testing;
        self.stop_monitoring();
        self.cap.borrow_mut().selected = Some(index as usize);
        self.remember_selection();
        self.update_selection_details();
        self.cap.borrow_mut().mic_testing = was_testing;
        self.start_monitoring_if_wanted();
    }

    fn remember_selection(&self) {
        let Some(device) = self.cap.borrow().selected_device() else { return };
        let mut settings = self.settings.borrow_mut();
        if device.is_output {
            settings.current.last_output_device_id = Some(device.id);
        } else {
            settings.current.last_input_device_id = Some(device.id);
        }
        settings.save();
    }

    pub(super) fn refresh_devices(self: &Rc<Self>) {
        match platform::list_devices() {
            Ok(devices) => {
                let empty = devices.is_empty();
                self.cap.borrow_mut().all_devices = devices;
                self.apply_source_filter();
                if empty {
                    self.set_capture_notice(Some(
                        Notice::new(
                            "No audio devices found",
                            "Connect speakers, headphones or a microphone, or enable one in Sound settings.",
                            Severity::Warning,
                        )
                        .with_action(NoticeAction::OpenSoundSettings, "Open Sound settings"),
                    ));
                }
            }
            Err(e) => self.set_capture_notice(Some(Notice::new("Couldn't list audio devices", e.describe(), Severity::Error))),
        }
    }

    /// Shows only the current tab's devices and picks the best one: current, last used, then the Windows default.
    fn apply_source_filter(self: &Rc<Self>) {
        let (visible, keep_id) = {
            let cap = self.cap.borrow();
            let visible: Vec<DeviceInfo> = cap.all_devices.iter().filter(|d| d.is_output == cap.output_tab).cloned().collect();
            let keep = cap.selected_device().filter(|d| d.is_output == cap.output_tab).map(|d| d.id);
            (visible, keep)
        };
        let remembered = {
            let s = &self.settings.borrow().current;
            if self.cap.borrow().output_tab { s.last_output_device_id.clone() } else { s.last_input_device_id.clone() }
        };
        let pick = |id: &Option<String>| id.as_ref().and_then(|id| visible.iter().position(|d| &d.id == id));
        let selected = pick(&keep_id).or_else(|| pick(&remembered)).or_else(|| visible.iter().position(|d| d.is_default)).or(if visible.is_empty() { None } else { Some(0) });

        let items: Vec<DeviceItem> = visible
            .iter()
            .map(|d| DeviceItem {
                name: d.name.as_str().into(),
                subtitle: format!(
                    "{}{}",
                    if d.is_output { "Everything this device plays" } else { "Microphone or line input" },
                    if d.is_default { " · Windows default" } else { "" }
                )
                .into(),
            })
            .collect();
        let output_tab = self.cap.borrow().output_tab;
        {
            let mut cap = self.cap.borrow_mut();
            set_model(&cap.devices_model, items);
            cap.visible = visible;
            cap.selected = selected;
        }
        let s = self.cs();
        s.set_selected_device(selected.map(|i| i as i32).unwrap_or(-1));
        s.set_placeholder(if output_tab { "No speakers or headphones found" } else { "No microphones found" }.into());
        self.update_selection_details();
    }

    fn update_selection_details(&self) {
        let device = self.cap.borrow().selected_device();
        let format = device.as_ref().and_then(|d| platform::mix_format(&d.id));
        let s = self.cs();
        s.set_format_text(match (&device, format) {
            (None, _) => SharedString::new(),
            (Some(_), Some(f)) => f.describe().into(),
            (Some(_), None) => "Format unavailable".into(),
        });
        s.set_mono(format.is_some_and(|f| f.channels == 1));
    }

    pub(super) fn on_device_event(self: &Rc<Self>, event: DeviceEvent) {
        if let DeviceEvent::Unavailable(id) = &event {
            if self.cap.borrow().recording && self.engine.active_device_id().as_deref() == Some(id.as_str()) {
                // The capture thread also notices (device invalidated); stopping here just makes it prompt.
                let engine = self.engine.clone();
                std::thread::spawn(move || {
                    if let Some(result) = engine.stop(StopReason::DeviceLost) {
                        super::post(move |app| app.handle_stopped(result));
                    }
                });
            }
        }
        // Debounce bursts of notifications, then re-query on this thread.
        let weak = Rc::downgrade(self);
        self.cap.borrow().refresh_timer.start(TimerMode::SingleShot, ms(400), move || {
            if let Some(app) = weak.upgrade() {
                app.on_refresh_due();
            }
        });
    }

    fn on_refresh_due(self: &Rc<Self>) {
        let busy = {
            let cap = self.cap.borrow();
            cap.recording || cap.busy
        };
        if busy {
            self.cap.borrow_mut().refresh_pending = true; // don't disturb the list mid-recording
            return;
        }
        let before = self.cap.borrow().selected_device();
        self.refresh_devices();
        let after = self.cap.borrow().selected_device();
        if before != after || !self.engine.is_monitoring() {
            self.stop_monitoring_keep_test();
            self.start_monitoring_if_wanted();
        }
    }

    // ------------------------------------------------------------------ Live level

    /// System audio is metered whenever the recording screen is idle. Microphones only on request:
    /// opening a Bluetooth headset's mic drops its playback into low-quality call mode and lights
    /// the Windows "microphone in use" indicator.
    fn wants_monitor(&self) -> bool {
        let cap = self.cap.borrow();
        self.screen.get() == Screen::Capture
            && !cap.recording
            && !cap.busy
            && !self.closing.get()
            && cap.selected_device().is_some()
            && (cap.output_tab || cap.mic_testing)
    }

    pub(super) fn capture_screen_shown(self: &Rc<Self>) {
        self.start_monitoring_if_wanted();
    }

    pub(super) fn start_monitoring_if_wanted(self: &Rc<Self>) {
        if !self.wants_monitor() || self.engine.is_monitoring() || self.engine.is_recording() {
            self.sync_meter_ui();
            return;
        }
        let device = self.cap.borrow().selected_device().unwrap();
        match self.engine.start(&device, Mode::Monitor, capture_event_handler()) {
            Ok(_) => {
                self.cap.borrow_mut().meter = MeterState { live: true, ..Default::default() };
                self.engine.read_and_reset_peaks();
                self.start_meter_timer();
            }
            Err(e) => {
                self.cap.borrow_mut().mic_testing = false;
                self.set_capture_notice(Some(e.start_notice(&device.name, !device.is_output)));
            }
        }
        self.sync_meter_ui();
    }

    fn stop_monitoring_keep_test(&self) {
        if self.engine.is_monitoring() {
            self.engine.stop(StopReason::User);
        }
        if !self.cap.borrow().recording {
            self.cap.borrow_mut().meter = MeterState::default();
            self.cap.borrow().meter_timer.stop();
            self.reset_meter_ui();
        }
    }

    pub(super) fn stop_monitoring(&self) {
        self.cap.borrow_mut().mic_testing = false;
        self.stop_monitoring_keep_test();
        self.sync_meter_ui();
    }

    fn toggle_mic_test(self: &Rc<Self>) {
        let testing = self.cap.borrow().mic_testing;
        if testing {
            self.stop_monitoring();
        } else {
            self.cap.borrow_mut().mic_testing = true;
            self.start_monitoring_if_wanted();
        }
    }

    pub(super) fn monitor_failed(self: &Rc<Self>, error: AudioError) {
        self.engine.reap_monitor();
        if error.is_device_loss() {
            // Unplugged, or invalidated by sleep/resume or a format change: reconnect quietly to
            // whatever is selected now. A notice appears only if that fails too.
            self.stop_monitoring_keep_test();
            self.refresh_devices();
            self.start_monitoring_if_wanted();
            return;
        }
        let device = self.cap.borrow().selected_device();
        self.stop_monitoring();
        if let Some(d) = device {
            self.set_capture_notice(Some(error.start_notice(&d.name, !d.is_output)));
        }
    }

    fn sync_meter_ui(&self) {
        let cap = self.cap.borrow();
        let s = self.cs();
        s.set_meter_live(cap.meter.live);
        s.set_mic_testing(cap.mic_testing);
        if !cap.meter.live {
            s.set_level_text(if cap.output_tab || cap.recording { "—".into() } else { "Not checking".into() });
        }
    }

    fn reset_meter_ui(&self) {
        let s = self.cs();
        s.set_level_left(0.0);
        s.set_level_right(0.0);
        s.set_hold_left(0.0);
        s.set_hold_right(0.0);
        s.set_clipping(false);
        s.set_level_text("—".into());
    }

    fn start_meter_timer(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.cap.borrow().meter_timer.start(TimerMode::Repeated, ms(16), move || {
            if let Some(app) = weak.upgrade() {
                app.on_meter_tick();
            }
        });
    }

    fn on_meter_tick(self: &Rc<Self>) {
        let (l, r) = self.engine.read_and_reset_peaks();
        let now = Instant::now();
        let (positions, holds, clipping, loudest) = {
            let mut cap = self.cap.borrow_mut();
            let m = &mut cap.meter;
            for (i, peak) in [l, r].into_iter().enumerate() {
                m.display[i] = peak.max(m.display[i] * DECAY_PER_TICK);
                if peak >= m.hold[i] || m.hold_until[i].is_none_or(|t| now > t) {
                    m.hold[i] = m.display[i];
                    m.hold_until[i] = Some(now + HOLD_TIME);
                }
            }
            if l.max(r) >= 0.999 {
                m.clip_until = Some(now + CLIP_HOLD);
            }
            (
                [level_position(m.display[0]), level_position(m.display[1])],
                [level_position(m.hold[0]), level_position(m.hold[1])],
                m.clip_until.is_some_and(|t| now < t),
                m.display[0].max(m.display[1]),
            )
        };
        let s = self.cs();
        s.set_level_left(positions[0]);
        s.set_level_right(positions[1]);
        s.set_hold_left(holds[0]);
        s.set_hold_right(holds[1]);
        s.set_clipping(clipping);
        let db = if loudest > 1e-6 { 20.0 * loudest.log10() } else { f32::NEG_INFINITY };
        s.set_level_text(if db < METER_FLOOR_DB { "Silent".into() } else { format!("{:.0} dB", db.min(0.0)).replace('-', "−").into() });

        if self.cap.borrow().recording {
            let elapsed = self.engine.recorded_duration();
            let t = text::clock(elapsed);
            if s.get_elapsed_text() != t.as_str() {
                s.set_elapsed_text(t.into());
                self.update_title();
            }
        }
    }

    // ------------------------------------------------------------------ Recording

    pub(super) fn toggle_recording(self: &Rc<Self>) {
        let (recording, busy) = {
            let cap = self.cap.borrow();
            (cap.recording, cap.busy)
        };
        if busy {
            return;
        }
        if recording {
            self.stop_recording(StopReason::User);
            return;
        }
        let Some(device) = self.cap.borrow().selected_device() else { return };

        self.set_capture_notice(None);
        self.stop_monitoring_keep_test();
        self.cap.borrow_mut().busy = true;
        self.cs().set_busy(true);

        let result = self.engine.start(&device, Mode::Record, capture_event_handler());
        self.cap.borrow_mut().busy = false;
        self.cs().set_busy(false);
        match result {
            Ok(Some(session)) => {
                if self.cap.borrow().handled_sessions.contains(&session.id) {
                    return; // the device failed immediately and the stop was already handled
                }
                {
                    let mut cap = self.cap.borrow_mut();
                    cap.recording = true;
                    cap.meter = MeterState { live: true, ..Default::default() };
                }
                let s = self.cs();
                s.set_recording(true);
                s.set_elapsed_text("0:00".into());
                if let Some(f) = self.engine.active_format() {
                    s.set_format_text(f.describe().into());
                    s.set_mono(f.channels == 1);
                }
                self.engine.read_and_reset_peaks();
                self.start_meter_timer();
                self.sync_meter_ui();
                platform::set_recording_badge(self.ui.window(), true);
                self.update_title();
            }
            Ok(None) => {}
            Err(e) => {
                self.set_capture_notice(Some(e.start_notice(&device.name, !device.is_output)));
                if e.is_device_loss() {
                    self.refresh_devices();
                }
                self.start_monitoring_if_wanted();
            }
        }
    }

    /// Stops and finalizes the spool; the review screen opens when this returns.
    pub(super) fn stop_recording(self: &Rc<Self>, reason: StopReason) {
        if !self.cap.borrow().recording {
            return;
        }
        self.cap.borrow_mut().busy = true;
        self.cs().set_busy(true);
        let result = self.engine.stop(reason);
        self.cap.borrow_mut().busy = false;
        self.cs().set_busy(false);
        if let Some(result) = result {
            self.handle_stopped(result);
        }
    }

    /// Idempotent: both stop()'s result and the capture thread's Stopped event land here.
    pub fn handle_stopped(self: &Rc<Self>, result: CaptureResult) {
        if !self.cap.borrow_mut().handled_sessions.insert(result.session.id) {
            return;
        }
        self.engine.reap(result.session.id);
        let refresh = {
            let mut cap = self.cap.borrow_mut();
            cap.recording = false;
            cap.meter = MeterState::default();
            cap.meter_timer.stop();
            std::mem::take(&mut cap.refresh_pending) || result.reason == StopReason::DeviceLost
        };
        let s = self.cs();
        s.set_recording(false);
        s.set_elapsed_text("0:00".into());
        self.reset_meter_ui();
        self.sync_meter_ui();
        platform::set_recording_badge(self.ui.window(), false);
        if refresh {
            self.refresh_devices();
        }
        self.update_selection_details();
        self.update_title();

        if result.session.frame_count() == 0 {
            self.spools.delete(&result.session);
            let notice = match (&result.reason, &result.error) {
                (StopReason::DeviceLost, _) => Notice::new(
                    "Recording stopped",
                    format!("{} was disconnected before any audio was captured. Choose another source.", result.session.device_name),
                    Severity::Warning,
                ),
                (_, Some(e)) => Notice::new("Recording stopped", e.describe(), Severity::Error),
                _ => Notice::new("Nothing was recorded", "The recording ended before any audio was captured.", Severity::Info),
            };
            self.set_capture_notice(Some(notice));
            self.start_monitoring_if_wanted();
            return;
        }

        self.open_review(result);
    }

    // ------------------------------------------------------------------ Recovery

    fn check_for_recoverable_sessions(self: &Rc<Self>) {
        let orphans = self.spools.find_orphans();
        {
            let mut cap = self.cap.borrow_mut();
            for session in orphans {
                let known = cap.pending_recovery.as_ref().is_some_and(|p| p.id == session.id) || cap.recovery_queue.iter().any(|s| s.id == session.id);
                if !known {
                    cap.recovery_queue.push_back(session);
                }
            }
        }
        if self.cap.borrow().pending_recovery.is_none() {
            self.show_next_recovery();
        }
    }

    fn show_next_recovery(&self) {
        let next = self.cap.borrow_mut().recovery_queue.pop_front();
        let remaining = self.cap.borrow().recovery_queue.len();
        let data = match &next {
            None => NoticeData::default(),
            Some(session) => {
                let started = session.started_at.with_timezone(&chrono::Local).format("%b %-d at %-I:%M %p");
                let more = if remaining > 0 { format!(" ({remaining} more after this one.)") } else { String::new() };
                NoticeData {
                    visible: true,
                    title: "Unsaved recording found".into(),
                    message: format!(
                        "{} from {}, started {started}. RecordThis closed before it was saved.{more}",
                        text::clock(session.duration()),
                        session.device_name
                    )
                    .into(),
                    severity: 2,
                    action_text: "Recover it".into(),
                    secondary_text: "Delete it".into(),
                    closable: false,
                }
            }
        };
        self.cap.borrow_mut().pending_recovery = next;
        self.cs().set_recovery(data);
    }

    fn recover(self: &Rc<Self>) {
        if self.cap.borrow().recording {
            return;
        }
        let Some(session) = self.cap.borrow().pending_recovery.clone() else { return };
        self.show_next_recovery();
        self.cap.borrow_mut().handled_sessions.insert(session.id);
        let duration = session.duration();
        self.open_review(CaptureResult { session, reason: StopReason::Recovered, duration, silence_injected: Duration::ZERO, error: None });
    }

    fn discard_recovery(self: &Rc<Self>) {
        if let Some(session) = self.cap.borrow_mut().pending_recovery.take() {
            self.spools.delete(&session);
        }
        self.show_next_recovery();
    }

    // ------------------------------------------------------------------ Notices

    pub(super) fn set_capture_notice(self: &Rc<Self>, notice: Option<Notice>) {
        // The undo chance ends as soon as its notice is gone or replaced.
        let undo_still_showing = notice.as_ref().is_some_and(|n| n.action == NoticeAction::UndoDiscard);
        if !undo_still_showing {
            self.finalize_pending_discard();
        }
        let lifetime = notice.as_ref().and_then(|n| n.lifetime);
        self.cs().set_notice(notice_data(notice.as_ref()));
        let mut cap = self.cap.borrow_mut();
        cap.notice = notice;
        cap.notice_timer.stop();
        if let Some(lifetime) = lifetime {
            let weak = Rc::downgrade(self);
            cap.notice_timer.start(TimerMode::SingleShot, lifetime, move || {
                if let Some(app) = weak.upgrade() {
                    app.set_capture_notice(None);
                }
            });
        }
    }

    fn capture_notice_action(self: &Rc<Self>) {
        let Some(notice) = self.cap.borrow().notice.clone() else { return };
        if notice.action == NoticeAction::UndoDiscard {
            let discarded = self.cap.borrow_mut().pending_discard.take();
            self.set_capture_notice(None);
            if let Some(result) = discarded {
                self.open_review(CaptureResult { reason: StopReason::User, error: None, ..result });
            }
            return;
        }
        run_shell_action(&notice.action);
    }

    /// The review screen discarded a recording: keep the spool for 5 seconds so it can be undone.
    pub(super) fn offer_undo_discard(self: &Rc<Self>, discarded: CaptureResult) {
        self.finalize_pending_discard();
        self.cap.borrow_mut().pending_discard = Some(discarded);
        self.set_capture_notice(Some(
            Notice::new("Recording discarded", "", Severity::Info).with_action(NoticeAction::UndoDiscard, "Undo").lasting(UNDO_WINDOW),
        ));
    }

    pub(super) fn finalize_pending_discard(&self) {
        let pending = self.cap.borrow_mut().pending_discard.take();
        if let Some(discarded) = pending {
            self.spools.delete(&discarded.session);
        }
    }
}
