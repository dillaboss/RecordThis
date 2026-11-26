//! Application controller: owns the services, drives the Slint UI and implements the
//! Record ⇄ Review state machine. Everything here runs on the UI thread; worker threads post
//! back with [`post`].

mod capture;
mod review;
pub mod text;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Model, SharedString};

use crate::audio::capture::{CaptureEngine, CaptureEvent};
use crate::notice::{Notice, NoticeAction, Severity};
use crate::platform::{self, DeviceEvent, DeviceWatcher, SystemHooks};
use crate::storage::settings::Settings;
use crate::storage::spool::SpoolManager;
use crate::ui::{AppState, AppWindow, CaptureState, NoticeData, Theme};

pub use capture::CaptureUi;
pub use review::ReviewUi;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Capture,
    Review,
}

pub struct App {
    pub ui: AppWindow,
    pub engine: Arc<CaptureEngine>,
    pub spools: Arc<SpoolManager>,
    pub settings: RefCell<Settings>,
    pub screen: Cell<Screen>,
    pub cap: RefCell<CaptureUi>,
    pub rev: RefCell<ReviewUi>,
    pub hooks: RefCell<Option<SystemHooks>>,
    pub watcher: RefCell<Option<DeviceWatcher>>,
    pub closing: Cell<bool>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Runs `f` with the app, if it's still alive.
pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

/// Runs `f` on the UI thread (from any thread).
pub fn post(f: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(f));
}

pub fn notice_data(n: Option<&Notice>) -> NoticeData {
    match n {
        None => NoticeData::default(),
        Some(n) => NoticeData {
            visible: true,
            title: n.title.as_str().into(),
            message: n.message.as_str().into(),
            severity: match n.severity {
                Severity::Info => 0,
                Severity::Success => 1,
                Severity::Warning => 2,
                Severity::Error => 3,
            },
            action_text: n.action_text.as_str().into(),
            secondary_text: SharedString::new(),
            closable: n.action != NoticeAction::UndoDiscard,
        },
    }
}

/// Opens whatever Windows surface a notice points to.
pub fn run_shell_action(action: &NoticeAction) {
    match action {
        NoticeAction::OpenSoundSettings => platform::open_sound_settings(),
        NoticeAction::OpenMicrophonePrivacy => platform::open_microphone_privacy(),
        NoticeAction::RevealFile(path) => platform::reveal_in_folder(path),
        NoticeAction::None | NoticeAction::UndoDiscard => {}
    }
}

pub fn run() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let spools = Arc::new(SpoolManager::new());
    let engine = Arc::new(CaptureEngine::new(spools.clone()));
    let settings = Settings::load();

    let app = Rc::new(App {
        cap: RefCell::new(CaptureUi::new(&settings.current)),
        rev: RefCell::new(ReviewUi::new(&settings.current)),
        settings: RefCell::new(settings),
        ui,
        engine,
        spools,
        screen: Cell::new(Screen::Capture),
        hooks: RefCell::new(None),
        watcher: RefCell::new(None),
        closing: Cell::new(false),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    app.apply_accent();
    app.wire_callbacks();
    app.init_capture();
    app.init_review();

    // Global Ctrl+Alt+R and finalize-before-sleep. Both arrive on a background thread.
    let suspend_engine = app.engine.clone();
    let hooks = SystemHooks::install(
        Box::new(|| post(|app| app.on_hotkey())),
        Box::new(move || {
            // Runs on the power-notification thread: blocking here holds off sleep until the
            // spool is flushed (bounded by the engine's 2 s stop timeout). Only a recording is
            // stopped; a level monitor is left alone.
            if suspend_engine.is_recording() {
                if let Some(result) = suspend_engine.stop(crate::audio::capture::StopReason::SystemSuspend) {
                    post(move |app| app.handle_stopped(result));
                }
            }
        }),
    );
    let gesture_ok = hooks.hotkey_registered();
    *app.hooks.borrow_mut() = Some(hooks);
    app.ui.global::<CaptureState>().set_hotkey_available(gesture_ok);

    *app.watcher.borrow_mut() = DeviceWatcher::new(Arc::new(|event: DeviceEvent| {
        // MMDevAPI callback thread: never block it.
        post(move |app| app.on_device_event(event));
    }));

    {
        let weak = Rc::downgrade(&app);
        app.ui.window().on_close_requested(move || match weak.upgrade() {
            Some(app) => app.on_close_requested(),
            None => slint::CloseRequestResponse::HideWindow,
        });
    }

    app.update_title();
    app.ui.show()?;
    app.after_show();
    slint::run_event_loop()?;

    app.shutdown();
    APP.with(|a| *a.borrow_mut() = None);
    Ok(())
}

impl App {
    fn wire_callbacks(self: &Rc<Self>) {
        let app_state = self.ui.global::<AppState>();
        let weak = Rc::downgrade(self);
        app_state.on_close_choice(move |choice| {
            if let Some(app) = weak.upgrade() {
                app.on_close_choice(choice);
            }
        });
        let weak = Rc::downgrade(self);
        app_state.on_theme_changed(move |dark| {
            if let Some(app) = weak.upgrade() {
                app.style_window(dark);
            }
        });
        self.wire_capture();
        self.wire_review();
    }

    fn apply_accent(&self) {
        if let Some(accent) = platform::accent_color() {
            let theme = self.ui.global::<Theme>();
            theme.set_accent_on_dark(accent.on_dark);
            theme.set_accent_on_light(accent.on_light);
            theme.set_text_on_accent_dark(readable_on(accent.on_dark));
            theme.set_text_on_accent_light(readable_on(accent.on_light));
        }
    }

    fn after_show(self: &Rc<Self>) {
        let dark = self.ui.global::<Theme>().get_dark();
        self.style_window(dark);
        self.capture_screen_shown();
    }

    fn style_window(&self, dark: bool) {
        let bg = if dark { slint::Color::from_rgb_u8(0x20, 0x20, 0x20) } else { slint::Color::from_rgb_u8(0xf3, 0xf3, 0xf3) };
        platform::style_window(self.ui.window(), dark, bg);
    }

    pub fn set_screen(self: &Rc<Self>, screen: Screen) {
        if self.screen.get() == screen {
            return;
        }
        self.screen.set(screen);
        self.ui.global::<AppState>().set_screen(match screen {
            Screen::Capture => 0,
            Screen::Review => 1,
        });
        match screen {
            Screen::Capture => self.capture_screen_shown(),
            Screen::Review => self.stop_monitoring(),
        }
        self.update_title();
    }

    pub fn update_title(&self) {
        let recording = self.cap.borrow().recording;
        let title = if recording {
            format!("● Recording {} — RecordThis", text::clock(self.engine.recorded_duration()))
        } else if self.screen.get() == Screen::Review {
            "Review — RecordThis".to_string()
        } else {
            "RecordThis".to_string()
        };
        if self.ui.get_window_title() != title.as_str() {
            self.ui.set_window_title(title.into());
        }
    }

    /// Global shortcut. On the recording screen it starts or stops recording. On the review screen
    /// it starts a new recording only if nothing would be lost; otherwise it asks to finish first.
    fn on_hotkey(self: &Rc<Self>) {
        if self.closing.get() {
            return;
        }
        if self.screen.get() == Screen::Review {
            if self.rev.borrow().needs_attention() {
                self.show_finish_first();
                platform::bring_to_front(self.ui.window());
                return;
            }
            self.close_review_session();
            self.toggle_recording();
            return;
        }
        let was_recording = self.cap.borrow().recording;
        self.toggle_recording();
        if was_recording {
            platform::bring_to_front(self.ui.window()); // stopping lands on the review screen
        }
    }

    /// Never lose audio on close: finalize an active recording, then offer to save an unsaved one.
    fn on_close_requested(self: &Rc<Self>) -> slint::CloseRequestResponse {
        if self.closing.get() {
            return slint::CloseRequestResponse::HideWindow;
        }
        if self.cap.borrow().recording {
            if let Some(result) = self.engine.stop(crate::audio::capture::StopReason::User) {
                self.handle_stopped(result);
            }
        }
        if self.screen.get() == Screen::Review && self.rev.borrow().unsaved_and_unused() {
            if self.rev.borrow().exporting {
                return slint::CloseRequestResponse::KeepWindowShown;
            }
            self.ui.global::<AppState>().set_close_dialog(true);
            return slint::CloseRequestResponse::KeepWindowShown;
        }
        self.closing.set(true);
        slint::CloseRequestResponse::HideWindow
    }

    fn on_close_choice(self: &Rc<Self>, choice: i32) {
        self.ui.global::<AppState>().set_close_dialog(false);
        match choice {
            0 => self.save(true),
            1 => {
                self.close_review_session();
                self.quit();
            }
            _ => {}
        }
    }

    pub fn quit(self: &Rc<Self>) {
        self.closing.set(true);
        let _ = self.ui.hide();
        let _ = slint::quit_event_loop();
    }

    fn shutdown(&self) {
        self.engine.stop(crate::audio::capture::StopReason::User);
        self.finalize_pending_discard();
        self.release_review_for_exit();
        *self.hooks.borrow_mut() = None;
        *self.watcher.borrow_mut() = None;
    }
}

/// Black or white text, whichever reads better on `c`.
fn readable_on(c: slint::Color) -> slint::Color {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let l = 0.2126 * lin(c.red()) + 0.7152 * lin(c.green()) + 0.0722 * lin(c.blue());
    if l > 0.36 { slint::Color::from_rgb_u8(0, 0, 0) } else { slint::Color::from_rgb_u8(255, 255, 255) }
}

/// Model helper: replaces the contents of a VecModel only when they changed.
pub fn set_model<T: Clone + PartialEq + 'static>(model: &Rc<slint::VecModel<T>>, items: Vec<T>) {
    let same = model.row_count() == items.len() && items.iter().enumerate().all(|(i, it)| model.row_data(i).as_ref() == Some(it));
    if !same {
        model.set_vec(items);
    }
}

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

pub fn capture_event_handler() -> crate::audio::capture::EventCallback {
    Arc::new(|event: CaptureEvent| match event {
        CaptureEvent::Stopped(result) => post(move |app| app.handle_stopped(result)),
        CaptureEvent::MonitorFailed(error) => post(move |app| app.monitor_failed(error)),
    })
}
