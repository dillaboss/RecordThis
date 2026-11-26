//! RecordThis: record a microphone or everything a playback device plays (WASAPI loopback),
//! straight to disk, then trim and save or copy it.

pub mod app;
pub mod audio;
pub mod errors;
pub mod notice;
pub mod platform;
pub mod storage;

pub mod ui {
    slint::include_modules!();
}
