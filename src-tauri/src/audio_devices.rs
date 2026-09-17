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

    fn find_input(name: &str) -> Option<cpal::Device> {
        cpal::default_host().input_devices().ok()?.find(|d| d.name().ok().as_deref() == Some(name))
    }

    fn find_output(name: &str) -> Option<cpal::Device> {
        cpal::default_host().output_devices().ok()?.find(|d| d.name().ok().as_deref() == Some(name))
    }

    /// The microphone to open now: the preferred one if present, else the default.
    pub fn resolve_input() -> Option<cpal::Device> {
        if let Some(name) = prefs().input {
            if let Some(d) = find_input(&name) {
                return Some(d);
            }
        }
        cpal::default_host().default_input_device()
    }

    pub fn resolve_output() -> Option<cpal::Device> {
        if let Some(name) = prefs().output {
            if let Some(d) = find_output(&name) {
                return Some(d);
            }
        }
        cpal::default_host().default_output_device()
    }

    pub fn resolved_input_name() -> String {
        resolve_input().and_then(|d| d.name().ok()).unwrap_or_default()
    }

    pub fn resolved_output_name() -> String {
        resolve_output().and_then(|d| d.name().ok()).unwrap_or_default()
    }

    pub fn list() -> DeviceList {
        let host = cpal::default_host();
        let names = |it: Option<Box<dyn Iterator<Item = cpal::Device>>>| -> Vec<String> {
            it.map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default()
        };
        DeviceList {
            inputs: names(host.input_devices().ok().map(|d| Box::new(d) as Box<dyn Iterator<Item = cpal::Device>>)),
            outputs: names(host.output_devices().ok().map(|d| Box::new(d) as Box<dyn Iterator<Item = cpal::Device>>)),
            default_input: host.default_input_device().and_then(|d| d.name().ok()).unwrap_or_default(),
            default_output: host.default_output_device().and_then(|d| d.name().ok()).unwrap_or_default(),
            prefs: prefs(),
        }
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
