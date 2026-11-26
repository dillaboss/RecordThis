//! WASAPI shared-mode streams. Capture uses the engine's own mix format, so
//! AUDCLNT_E_UNSUPPORTED_FORMAT can't come from us asking for something the engine doesn't mix in.
//! Render lets the engine convert (AUTOCONVERTPCM) so preview works at any rate/channel count.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::devices::{device_by_id, enumerator, parse_format};
use crate::audio::format::CaptureFormat;
use crate::errors::AudioError;
use crate::platform::DeviceInfo;

const STREAMFLAGS_LOOPBACK: u32 = 0x0002_0000;
const STREAMFLAGS_EVENTCALLBACK: u32 = 0x0004_0000;
const STREAMFLAGS_SRC_DEFAULT_QUALITY: u32 = 0x0800_0000;
const STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x8000_0000;
const BUFFERFLAGS_SILENT: u32 = 0x2;
/// 100 ms, in 100-ns units.
const BUFFER_DURATION: i64 = 1_000_000;

struct Event(HANDLE);

impl Event {
    fn new() -> Result<Self, AudioError> {
        Ok(Self(unsafe { CreateEventW(None, false, false, None)? }))
    }

    fn wait(&self, timeout: Duration) {
        let _ = unsafe { WaitForSingleObject(self.0, timeout.as_millis().min(u32::MAX as u128) as u32) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub struct CaptureStream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: Event,
    format: CaptureFormat,
    started: bool,
}

impl CaptureStream {
    /// Loopback for render endpoints, standard capture for microphones.
    pub fn open(device: &DeviceInfo) -> Result<Self, AudioError> {
        let mm = device_by_id(&device.id)?;
        let client: IAudioClient = unsafe { mm.Activate(CLSCTX_ALL, None)? };
        let mix = unsafe { client.GetMixFormat()? };
        let format = unsafe { parse_format(mix) };
        let flags = STREAMFLAGS_EVENTCALLBACK | if device.is_output { STREAMFLAGS_LOOPBACK } else { 0 };
        let init = unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_DURATION, 0, mix, None) };
        unsafe { CoTaskMemFree(Some(mix as *const _)) };
        let format = format?;
        init?;
        let event = Event::new()?;
        unsafe { client.SetEventHandle(event.0)? };
        let capture: IAudioCaptureClient = unsafe { client.GetService()? };
        Ok(Self { client, capture, event, format, started: false })
    }

    pub fn format(&self) -> CaptureFormat {
        self.format
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        unsafe { self.client.Start()? };
        self.started = true;
        Ok(())
    }

    /// Waits up to `timeout` for the engine, then appends every available packet to `out`.
    /// Loopback endpoints deliver nothing while silent; the caller's watchdog fills those gaps.
    pub fn read(&mut self, out: &mut Vec<u8>, timeout: Duration) -> Result<(), AudioError> {
        self.event.wait(timeout);
        let block = self.format.block_align();
        loop {
            let packet = unsafe { self.capture.GetNextPacketSize()? };
            if packet == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            unsafe { self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
            let bytes = frames as usize * block;
            if flags & BUFFERFLAGS_SILENT != 0 || data.is_null() {
                out.resize(out.len() + bytes, self.format.silence_byte());
            } else {
                out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, bytes) });
            }
            unsafe { self.capture.ReleaseBuffer(frames)? };
        }
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        if self.started {
            let _ = unsafe { self.client.Stop() };
        }
    }
}

#[repr(C, packed(1))]
struct WaveFormatExtensible {
    format: WAVEFORMATEX,
    valid_bits: u16,
    channel_mask: u32,
    sub_format: windows::core::GUID,
}

/// Float stereo/mono output to the default playback device.
pub struct RenderStream {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: Event,
    channels: usize,
    buffer_frames: usize,
}

impl RenderStream {
    pub fn open(sample_rate: u32, channels: u16) -> Result<Self, AudioError> {
        let device = unsafe { enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)? };
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let block = channels * 4;
        let format = WaveFormatExtensible {
            format: WAVEFORMATEX {
                wFormatTag: 0xFFFE,
                nChannels: channels,
                nSamplesPerSec: sample_rate,
                nAvgBytesPerSec: sample_rate * block as u32,
                nBlockAlign: block,
                wBitsPerSample: 32,
                cbSize: 22,
            },
            valid_bits: 32,
            channel_mask: if channels == 1 { 0x4 } else { 0x3 },
            sub_format: windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71),
        };
        let flags = STREAMFLAGS_EVENTCALLBACK | STREAMFLAGS_AUTOCONVERTPCM | STREAMFLAGS_SRC_DEFAULT_QUALITY;
        unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_DURATION, 0, &format as *const _ as *const WAVEFORMATEX, None)? };
        let event = Event::new()?;
        unsafe { client.SetEventHandle(event.0)? };
        let buffer_frames = unsafe { client.GetBufferSize()? } as usize;
        let render: IAudioRenderClient = unsafe { client.GetService()? };
        Ok(Self { client, render, event, channels: channels as usize, buffer_frames })
    }

    pub fn buffer_frames(&self) -> usize {
        self.buffer_frames
    }

    pub fn padding(&self) -> Result<usize, AudioError> {
        Ok(unsafe { self.client.GetCurrentPadding()? } as usize)
    }

    pub fn wait(&self, timeout: Duration) {
        self.event.wait(timeout);
    }

    /// `samples` must fit in the free part of the buffer.
    pub fn write(&mut self, samples: &[f32]) -> Result<(), AudioError> {
        let frames = samples.len() / self.channels;
        if frames == 0 {
            return Ok(());
        }
        let data = unsafe { self.render.GetBuffer(frames as u32)? };
        unsafe {
            std::ptr::copy_nonoverlapping(samples.as_ptr() as *const u8, data, frames * self.channels * 4);
            self.render.ReleaseBuffer(frames as u32, 0)?;
        }
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        unsafe { self.client.Start()? };
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), AudioError> {
        unsafe { self.client.Stop()? };
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), AudioError> {
        unsafe { self.client.Reset()? };
        Ok(())
    }
}

impl Drop for RenderStream {
    fn drop(&mut self) {
        let _ = unsafe { self.client.Stop() };
    }
}
