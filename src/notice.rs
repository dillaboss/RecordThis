use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NoticeAction {
    None,
    OpenSoundSettings,
    OpenMicrophonePrivacy,
    RevealFile(std::path::PathBuf),
    UndoDiscard,
}

/// A message with at most one follow-up action, optionally self-dismissing.
#[derive(Clone, Debug)]
pub struct Notice {
    pub title: String,
    pub message: String,
    pub severity: Severity,
    pub action: NoticeAction,
    pub action_text: String,
    pub lifetime: Option<Duration>,
}

impl Notice {
    pub fn new(title: impl Into<String>, message: impl Into<String>, severity: Severity) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            severity,
            action: NoticeAction::None,
            action_text: String::new(),
            lifetime: None,
        }
    }

    pub fn with_action(mut self, action: NoticeAction, text: impl Into<String>) -> Self {
        self.action = action;
        self.action_text = text.into();
        self
    }

    pub fn lasting(mut self, lifetime: Duration) -> Self {
        self.lifetime = Some(lifetime);
        self
    }
}
