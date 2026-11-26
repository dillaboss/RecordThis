//! MP3 through Windows' built-in Media Foundation encoder (as the C# version did with NAudio).
//! "N" editions of Windows without the Media Feature Pack have no encoder; that's reported as
//! [`ExportError::Mp3Unavailable`] so Copy can fall back to WAV.

use std::ffi::c_void;
use std::path::Path;
use std::sync::OnceLock;

use windows::core::{Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use super::wide;
use crate::errors::ExportError;

const MF_VERSION: u32 = 0x0002_0070;

type Out = *mut *mut c_void;

/// Media Foundation entry points, loaded at run time. Linking them normally would stop the app
/// from even starting on Windows "N" editions, which don't ship mfplat.dll.
struct Mf {
    startup: unsafe extern "system" fn(u32, u32) -> HRESULT,
    shutdown: unsafe extern "system" fn() -> HRESULT,
    create_attributes: unsafe extern "system" fn(Out, u32) -> HRESULT,
    create_media_type: unsafe extern "system" fn(Out) -> HRESULT,
    create_memory_buffer: unsafe extern "system" fn(u32, Out) -> HRESULT,
    create_sample: unsafe extern "system" fn(Out) -> HRESULT,
    create_sink_writer: unsafe extern "system" fn(PCWSTR, *mut c_void, *mut c_void, Out) -> HRESULT,
    output_types: unsafe extern "system" fn(*const GUID, u32, *mut c_void, Out) -> HRESULT,
}

// Each pointer is transmuted to the field type it's assigned to.
#[allow(clippy::missing_transmute_annotations)]
fn mf() -> Option<&'static Mf> {
    static MF: OnceLock<Option<Mf>> = OnceLock::new();
    MF.get_or_init(|| unsafe {
        let load = |name: &str| LoadLibraryW(PCWSTR(wide(name).as_ptr())).ok();
        let (plat, readwrite, core) = (load("mfplat.dll")?, load("mfreadwrite.dll")?, load("mf.dll")?);
        macro_rules! get {
            ($module:expr, $name:literal) => {
                std::mem::transmute(GetProcAddress($module, windows::core::s!($name))?)
            };
        }
        Some(Mf {
            startup: get!(plat, "MFStartup"),
            shutdown: get!(plat, "MFShutdown"),
            create_attributes: get!(plat, "MFCreateAttributes"),
            create_media_type: get!(plat, "MFCreateMediaType"),
            create_memory_buffer: get!(plat, "MFCreateMemoryBuffer"),
            create_sample: get!(plat, "MFCreateSample"),
            create_sink_writer: get!(readwrite, "MFCreateSinkWriterFromURL"),
            output_types: get!(core, "MFTranscodeGetAudioOutputAvailableTypes"),
        })
    })
    .as_ref()
}

/// Calls a creator function and wraps the returned COM pointer.
unsafe fn create<T: Interface>(f: impl FnOnce(Out) -> HRESULT) -> windows::core::Result<T> {
    let mut raw: *mut c_void = std::ptr::null_mut();
    f(&mut raw).ok()?;
    if raw.is_null() {
        return Err(windows::core::Error::from_hresult(HRESULT(0x8000_4003u32 as i32))); // E_POINTER
    }
    Ok(unsafe { T::from_raw(raw) })
}

pub struct Mp3Writer {
    writer: Option<IMFSinkWriter>,
    started: bool,
    _com: super::ThreadGuard,
    stream: u32,
    sample_rate: u32,
    channels: u16,
    time: i64,
    frames_written: u64,
}

/// Picks the encoder output type matching rate, channels and the closest available bit rate.
unsafe fn select_output_type(mf: &Mf, sample_rate: u32, channels: u16, kbps: u32) -> Result<IMFMediaType, ExportError> {
    let flags = MFT_ENUM_FLAG_ALL.0 as u32 | MFT_ENUM_FLAG_SORTANDFILTER.0 as u32;
    let types: IMFCollection = unsafe { create(|out| (mf.output_types)(&MFAudioFormat_MP3, flags, std::ptr::null_mut(), out)) }
        .map_err(|_| ExportError::Mp3Unavailable)?;
    let count = unsafe { types.GetElementCount() }.unwrap_or(0);
    let wanted = kbps * 1000 / 8;
    let mut best: Option<(u32, IMFMediaType)> = None;
    for i in 0..count {
        let Ok(unknown) = (unsafe { types.GetElement(i) }) else { continue };
        let Ok(t) = unknown.cast::<IMFMediaType>() else { continue };
        let rate = unsafe { t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }.unwrap_or(0);
        let ch = unsafe { t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }.unwrap_or(0);
        let bytes = unsafe { t.GetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND) }.unwrap_or(0);
        if rate != sample_rate || ch != channels as u32 {
            continue;
        }
        let distance = bytes.abs_diff(wanted);
        if best.as_ref().is_none_or(|(d, _)| distance < *d) {
            best = Some((distance, t));
        }
    }
    best.map(|(_, t)| t).ok_or(ExportError::Mp3Unavailable)
}

impl Mp3Writer {
    pub fn create(path: &Path, sample_rate: u32, channels: u16, kbps: u32) -> Result<Self, ExportError> {
        // Exports run on a worker thread: bring up COM and Media Foundation for it.
        let com = super::init_thread();
        let mf = mf().ok_or(ExportError::Mp3Unavailable)?;
        unsafe { (mf.startup)(MF_VERSION, 0 /* MFSTARTUP_FULL */) }.ok().map_err(|_| ExportError::Mp3Unavailable)?;
        let mut this = Self { writer: None, started: true, _com: com, stream: 0, sample_rate, channels, time: 0, frames_written: 0 };
        unsafe {
            let output = select_output_type(mf, sample_rate, channels, kbps)?;

            let attrs: IMFAttributes = create(|out| (mf.create_attributes)(out, 1))?;
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MP3)?;

            let w = wide(&path.to_string_lossy());
            let writer: IMFSinkWriter = create(|out| (mf.create_sink_writer)(PCWSTR(w.as_ptr()), std::ptr::null_mut(), attrs.as_raw(), out))?;
            let stream = writer.AddStream(&output).map_err(|_| ExportError::Mp3Unavailable)?;

            let input: IMFMediaType = create(|out| (mf.create_media_type)(out))?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, channels as u32 * 2)?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, sample_rate * channels as u32 * 2)?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            writer.SetInputMediaType(stream, &input, None).map_err(|_| ExportError::Mp3Unavailable)?;
            writer.BeginWriting()?;

            this.writer = Some(writer);
            this.stream = stream;
            Ok(this)
        }
    }

    pub fn write(&mut self, pcm: &[i16]) -> Result<(), ExportError> {
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes = pcm.len() * 2;
        let frames = (pcm.len() / self.channels as usize) as u64;
        let mf = mf().ok_or(ExportError::Mp3Unavailable)?;
        unsafe {
            let buffer: IMFMediaBuffer = create(|out| (mf.create_memory_buffer)(bytes as u32, out))?;
            let mut data: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, data, bytes);
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes as u32)?;

            let sample: IMFSample = create(|out| (mf.create_sample)(out))?;
            sample.AddBuffer(&buffer)?;
            self.frames_written += frames;
            // Timestamps in 100-ns units, computed from the running frame count so they never drift.
            let end = (self.frames_written as i128 * 10_000_000 / self.sample_rate as i128) as i64;
            sample.SetSampleTime(self.time)?;
            sample.SetSampleDuration(end - self.time)?;
            self.time = end;
            if let Some(w) = &self.writer {
                w.WriteSample(self.stream, &sample)?;
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), ExportError> {
        if let Some(w) = self.writer.take() {
            unsafe { w.Finalize()? };
        }
        Ok(())
    }
}

impl Drop for Mp3Writer {
    fn drop(&mut self) {
        self.writer = None; // release the sink writer before shutting Media Foundation down
        if let (true, Some(mf)) = (self.started, mf()) {
            let _ = unsafe { (mf.shutdown)() };
        }
    }
}
