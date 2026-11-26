//! Endpoint enumeration and hot-plug notifications (IMMNotificationClient). Notifications arrive
//! on MMDevAPI worker threads; the handler only forwards events, and the UI re-queries later.

use std::sync::Arc;

use windows::core::{implement, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eMultimedia, eRender, EDataFlow, ERole, IAudioClient, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

use super::{ensure_ui_com, wide};
use crate::audio::format::{CaptureFormat, SampleEncoding};
use crate::errors::AudioError;
use crate::platform::{DeviceEvent, DeviceInfo};

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_PCM: windows::core::GUID = windows::core::GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_IEEE_FLOAT: windows::core::GUID = windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

pub(super) fn enumerator() -> Result<IMMDeviceEnumerator, AudioError> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

pub(super) fn take_pwstr(p: PWSTR) -> String {
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

fn device_id(device: &IMMDevice) -> Result<String, AudioError> {
    Ok(take_pwstr(unsafe { device.GetId()? }))
}

fn friendly_name(device: &IMMDevice) -> Result<String, AudioError> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ)? };
    let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName)? };
    Ok(value.to_string())
}

pub(super) fn device_by_id(id: &str) -> Result<IMMDevice, AudioError> {
    let w = wide(id);
    Ok(unsafe { enumerator()?.GetDevice(PCWSTR(w.as_ptr()))? })
}

/// Output devices (recorded through loopback) first, then inputs; the Windows default leads each group.
pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    ensure_ui_com();
    let e = enumerator()?;
    let mut all = Vec::new();
    for (flow, is_output) in [(eRender, true), (eCapture, false)] {
        let default_id = unsafe { e.GetDefaultAudioEndpoint(flow, eMultimedia) }.ok().and_then(|d| device_id(&d).ok());
        let mut group = Vec::new();
        let Ok(collection) = (unsafe { e.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }) else { continue };
        let count = unsafe { collection.GetCount() }.unwrap_or(0);
        for i in 0..count {
            // An endpoint can vanish mid-enumeration; skip it.
            let Ok(device) = (unsafe { collection.Item(i) }) else { continue };
            let (Ok(id), Ok(name)) = (device_id(&device), friendly_name(&device)) else { continue };
            group.push(DeviceInfo { is_default: Some(&id) == default_id.as_ref(), id, name, is_output });
        }
        group.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        all.extend(group);
    }
    Ok(all)
}

/// Reads a WAVEFORMATEX / WAVEFORMATEXTENSIBLE. WASAPI mix formats are almost always EXTENSIBLE,
/// whose real sample type lives in SubFormat.
pub(super) unsafe fn parse_format(ptr: *const WAVEFORMATEX) -> Result<CaptureFormat, AudioError> {
    let wf = unsafe { std::ptr::read_unaligned(ptr) };
    let (encoding, mask) = match wf.wFormatTag {
        WAVE_FORMAT_PCM => (SampleEncoding::Pcm, 0),
        WAVE_FORMAT_IEEE_FLOAT => (SampleEncoding::IeeeFloat, 0),
        WAVE_FORMAT_EXTENSIBLE if wf.cbSize >= 22 => {
            // WAVEFORMATEXTENSIBLE: WAVEFORMATEX (18) | wValidBitsPerSample (2) | dwChannelMask (4) | SubFormat (16)
            let base = ptr as *const u8;
            let mask = unsafe { std::ptr::read_unaligned(base.add(20) as *const u32) };
            let sub = unsafe { std::ptr::read_unaligned(base.add(24) as *const windows::core::GUID) };
            if sub == SUBTYPE_IEEE_FLOAT {
                (SampleEncoding::IeeeFloat, mask)
            } else if sub == SUBTYPE_PCM {
                (SampleEncoding::Pcm, mask)
            } else {
                return Err(AudioError::hr(crate::errors::AUDCLNT_E_UNSUPPORTED_FORMAT, format!("The device uses an unsupported sample type ({sub:?})")));
            }
        }
        tag => return Err(AudioError::hr(crate::errors::AUDCLNT_E_UNSUPPORTED_FORMAT, format!("The device uses an unsupported sample format (0x{tag:04X})"))),
    };
    CaptureFormat::new(wf.nSamplesPerSec, wf.nChannels, wf.wBitsPerSample, encoding, mask)
        .map_err(|m| AudioError::hr(crate::errors::AUDCLNT_E_UNSUPPORTED_FORMAT, m))
}

/// The engine's shared-mode mix format for an endpoint.
pub fn mix_format(id: &str) -> Option<CaptureFormat> {
    ensure_ui_com();
    let device = device_by_id(id).ok()?;
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.ok()?;
    let ptr = unsafe { client.GetMixFormat() }.ok()?;
    let format = unsafe { parse_format(ptr) };
    unsafe { CoTaskMemFree(Some(ptr as *const _)) };
    format.ok()
}

#[implement(IMMNotificationClient)]
struct Notifier {
    callback: Arc<dyn Fn(DeviceEvent) + Send + Sync>,
}

impl IMMNotificationClient_Impl for Notifier_Impl {
    fn OnDeviceStateChanged(&self, id: &PCWSTR, state: DEVICE_STATE) -> windows::core::Result<()> {
        if state == DEVICE_STATE_ACTIVE {
            (self.callback)(DeviceEvent::Changed);
        } else {
            (self.callback)(DeviceEvent::Unavailable(unsafe { id.to_string() }.unwrap_or_default()));
        }
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        (self.callback)(DeviceEvent::Changed);
        Ok(())
    }

    fn OnDeviceRemoved(&self, id: &PCWSTR) -> windows::core::Result<()> {
        (self.callback)(DeviceEvent::Unavailable(unsafe { id.to_string() }.unwrap_or_default()));
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, _flow: EDataFlow, role: ERole, _id: &PCWSTR) -> windows::core::Result<()> {
        if role == eMultimedia {
            (self.callback)(DeviceEvent::Changed);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, key: &PROPERTYKEY) -> windows::core::Result<()> {
        // Fires constantly (volume, jack state...). Only a rename changes what we show.
        if key.fmtid == PKEY_Device_FriendlyName.fmtid && key.pid == PKEY_Device_FriendlyName.pid {
            (self.callback)(DeviceEvent::Changed);
        }
        Ok(())
    }
}

pub struct DeviceWatcher {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl DeviceWatcher {
    pub fn new(callback: Arc<dyn Fn(DeviceEvent) + Send + Sync>) -> Option<Self> {
        ensure_ui_com();
        let enumerator = enumerator().ok()?;
        let client: IMMNotificationClient = Notifier { callback }.into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }.ok()?;
        Some(Self { enumerator, client })
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        let _ = unsafe { self.enumerator.UnregisterEndpointNotificationCallback(&self.client) };
    }
}
