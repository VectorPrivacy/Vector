//! Which microphone and speaker Vector uses: the system default, or a device the
//! user named in Settings whenever it is present. Every audio path resolves here,
//! so a preference and a fallback behave the same for calls, notes and sounds.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DevicePrefs {
    /// A device name, or None for the system default.
    pub input: Option<String>,
    pub output: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct DeviceList {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub default_input: String,
    pub default_output: String,
    pub prefs: DevicePrefs,
}

const KEY: &str = "audio_devices";

static PREFS: Mutex<Option<DevicePrefs>> = Mutex::new(None);

pub fn prefs() -> DevicePrefs {
    if let Some(p) = PREFS.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return p;
    }
    load()
}

/// Reads the account's saved preference; defaults when unset or logged out.
pub fn load() -> DevicePrefs {
    let p = vector_core::db::get_sql_setting(KEY.to_string())
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str::<DevicePrefs>(&j).ok())
        .unwrap_or_default();
    *PREFS.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
    p
}

pub fn set(p: DevicePrefs) -> Result<(), String> {
    let json = serde_json::to_string(&p).map_err(|e| e.to_string())?;
    vector_core::db::set_sql_setting(KEY.to_string(), json)?;
    *PREFS.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    Ok(())
}

#[cfg(not(target_os = "android"))]
mod desktop {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};

    /// The named device, as cpal opens it. cpal tests a device by opening an AudioUnit on it,
    /// so only devices carrying that name are tested.
    fn find(name: &str, input: bool) -> Option<cpal::Device> {
        cpal::default_host().devices().ok()?.find(|d| {
            d.name().ok().as_deref() == Some(name) && if input { d.supports_input() } else { d.supports_output() }
        })
    }

    /// The microphone to open now: the preferred one if present, else the default.
    pub fn resolve_input() -> Option<cpal::Device> {
        if let Some(name) = prefs().input {
            if let Some(d) = find(&name, true) {
                return Some(d);
            }
        }
        cpal::default_host().default_input_device()
    }

    pub fn resolve_output() -> Option<cpal::Device> {
        if let Some(name) = prefs().output {
            if let Some(d) = find(&name, false) {
                return Some(d);
            }
        }
        cpal::default_host().default_output_device()
    }

    fn resolved_name(preferred: Option<String>, input: bool) -> String {
        match preferred {
            Some(name) if names(input).contains(&name) => name,
            _ => default_name(input),
        }
    }

    pub fn resolved_input_name() -> String {
        resolved_name(prefs().input, true)
    }

    pub fn resolved_output_name() -> String {
        resolved_name(prefs().output, false)
    }

    pub fn list() -> DeviceList {
        // Until access is granted (by a call, a voice message or the mic test) only the
        // default microphone is offered.
        let mics = crate::mic_access::granted();
        DeviceList {
            inputs: if mics { names(true) } else { Vec::new() },
            outputs: names(false),
            default_input: if mics { default_name(true) } else { String::new() },
            default_output: default_name(false),
            prefs: prefs(),
        }
    }

    #[cfg(target_os = "macos")]
    use super::coreaudio::{default_name, names};

    #[cfg(not(target_os = "macos"))]
    fn names(input: bool) -> Vec<String> {
        let host = cpal::default_host();
        let devices = if input { host.input_devices() } else { host.output_devices() };
        devices.map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default()
    }

    #[cfg(not(target_os = "macos"))]
    fn default_name(input: bool) -> String {
        let host = cpal::default_host();
        let device = if input { host.default_input_device() } else { host.default_output_device() };
        device.and_then(|d| d.name().ok()).unwrap_or_default()
    }
}

/// Device names read straight from CoreAudio's properties. cpal's `input_devices()` and
/// `output_devices()` test every device by binding an AudioUnit to it, and macOS asks for the
/// microphone as soon as one is bound to any device with an input stream.
#[cfg(target_os = "macos")]
mod coreaudio {
    use objc2_core_foundation::{CFRetained, CFString};
    use std::ffi::c_void;
    use std::ptr::{null, NonNull};

    #[repr(C)]
    struct Address {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyDataSize(id: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: *mut u32) -> i32;
        fn AudioObjectGetPropertyData(id: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
    }

    const SYSTEM_OBJECT: u32 = 1;
    const DEVICES: u32 = u32::from_be_bytes(*b"dev#");
    const DEFAULT_INPUT: u32 = u32::from_be_bytes(*b"dIn ");
    const DEFAULT_OUTPUT: u32 = u32::from_be_bytes(*b"dOut");
    const STREAMS: u32 = u32::from_be_bytes(*b"stm#");
    const NAME: u32 = u32::from_be_bytes(*b"lnam");
    const GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const INPUT: u32 = u32::from_be_bytes(*b"inpt");
    const OUTPUT: u32 = u32::from_be_bytes(*b"outp");

    fn read<T: Copy>(id: u32, address: &Address, init: T) -> Option<T> {
        let mut value = init;
        let mut size = std::mem::size_of::<T>() as u32;
        let status = unsafe { AudioObjectGetPropertyData(id, address, 0, null(), &mut size, (&mut value as *mut T).cast()) };
        (status == 0).then_some(value)
    }

    fn device_ids() -> Vec<u32> {
        let address = Address { selector: DEVICES, scope: GLOBAL, element: 0 };
        let mut size = 0u32;
        if unsafe { AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, null(), &mut size) } != 0 {
            return Vec::new();
        }
        let mut ids = vec![0u32; size as usize / 4];
        let status = unsafe { AudioObjectGetPropertyData(SYSTEM_OBJECT, &address, 0, null(), &mut size, ids.as_mut_ptr().cast()) };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / 4);
        ids
    }

    fn has_streams(id: u32, input: bool) -> bool {
        let address = Address { selector: STREAMS, scope: if input { INPUT } else { OUTPUT }, element: 0 };
        let mut size = 0u32;
        unsafe { AudioObjectGetPropertyDataSize(id, &address, 0, null(), &mut size) == 0 && size > 0 }
    }

    /// The same property cpal names a device by, so a saved name finds the device there.
    fn name(id: u32) -> Option<String> {
        let address = Address { selector: NAME, scope: OUTPUT, element: 0 };
        let raw = read::<*const CFString>(id, &address, null())?;
        // A copied CFString: ours to release.
        let name = unsafe { CFRetained::from_raw(NonNull::new(raw as *mut CFString)?) };
        Some(name.to_string())
    }

    pub fn names(input: bool) -> Vec<String> {
        device_ids().into_iter().filter(|&id| has_streams(id, input)).filter_map(name).collect()
    }

    pub fn default_name(input: bool) -> String {
        let address = Address { selector: if input { DEFAULT_INPUT } else { DEFAULT_OUTPUT }, scope: GLOBAL, element: 0 };
        read(SYSTEM_OBJECT, &address, 0u32).filter(|&id| id != 0).and_then(name).unwrap_or_default()
    }
}

#[cfg(target_os = "android")]
mod desktop {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};

    // Android routes audio itself; the app only ever asks for the defaults.
    pub fn resolve_input() -> Option<cpal::Device> {
        cpal::default_host().default_input_device()
    }
    pub fn resolve_output() -> Option<cpal::Device> {
        cpal::default_host().default_output_device()
    }
    pub fn resolved_input_name() -> String {
        resolve_input().and_then(|d| d.name().ok()).unwrap_or_default()
    }
    pub fn resolved_output_name() -> String {
        resolve_output().and_then(|d| d.name().ok()).unwrap_or_default()
    }
    pub fn list() -> DeviceList {
        DeviceList { prefs: prefs(), ..Default::default() }
    }
}

pub use desktop::{list, resolve_input, resolve_output, resolved_input_name, resolved_output_name};
