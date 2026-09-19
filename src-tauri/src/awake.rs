//! Keeps the machine awake for as long as something needs it: a Mini App window
//! must not be napped, a call must not be slept through, a video call must keep
//! the display on. Holds are counted and the strongest one wins, so a call ending
//! never wakes nothing and never drops a Mini App's hold.

use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    /// No App Nap; the machine may still sleep.
    NoNap,
    /// No idle system sleep; the display may still go dark.
    System,
    /// Not even the display.
    Display,
}

static HOLDS: Mutex<Vec<Level>> = Mutex::new(Vec::new());

/// A hold on the machine for as long as this lives.
pub struct Hold(Level);

pub fn hold(level: Level) -> Hold {
    let mut holds = HOLDS.lock().unwrap_or_else(|e| e.into_inner());
    holds.push(level);
    imp::apply(holds.iter().max().copied());
    Hold(level)
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut holds = HOLDS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = holds.iter().position(|l| *l == self.0) {
            holds.swap_remove(i);
        }
        imp::apply(holds.iter().max().copied());
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Level;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    use std::sync::Mutex;

    struct Activity(Retained<AnyObject>, Level);
    // The token only ever goes back to NSProcessInfo, which accepts it from any thread.
    unsafe impl Send for Activity {}

    static ACTIVITY: Mutex<Option<Activity>> = Mutex::new(None);
    /// NSActivityUserInitiatedAllowingIdleSystemSleep.
    const NO_NAP: u64 = 0x00EF_FFFF;
    /// NSActivityUserInitiated: the above plus NSActivityIdleSystemSleepDisabled.
    const SYSTEM: u64 = 0x00FF_FFFF;
    /// NSActivityIdleDisplaySleepDisabled.
    const DISPLAY: u64 = SYSTEM | (1 << 40);

    pub fn apply(level: Option<Level>) {
        let mut slot = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().map(|a| a.1) == level {
            return;
        }
        unsafe {
            let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
            if let Some(Activity(token, _)) = slot.take() {
                let _: () = msg_send![info, endActivity: &*token];
            }
            let Some(level) = level else { return };
            let (options, reason) = match level {
                Level::NoNap => (NO_NAP, "Mini App window open"),
                Level::System => (SYSTEM, "Call in progress"),
                Level::Display => (DISPLAY, "Video call in progress"),
            };
            let reason = NSString::from_str(reason);
            let token: *mut AnyObject = msg_send![info, beginActivityWithOptions: options as usize, reason: &*reason];
            if let Some(token) = Retained::retain(token) {
                *slot = Some(Activity(token, level));
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::Level;
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Mutex, OnceLock};
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };

    // ES_CONTINUOUS is per thread and lasts as long as that thread does, so one
    // thread owns it for the process's life.
    static TX: OnceLock<Mutex<Sender<Option<Level>>>> = OnceLock::new();

    pub fn apply(level: Option<Level>) {
        let tx = TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Option<Level>>();
            std::thread::Builder::new()
                .name("awake".into())
                .spawn(move || {
                    while let Ok(level) = rx.recv() {
                        let flags = match level {
                            Some(Level::Display) => ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
                            Some(Level::System) => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
                            _ => ES_CONTINUOUS,
                        };
                        unsafe {
                            SetThreadExecutionState(flags);
                        }
                    }
                })
                .expect("awake thread");
            Mutex::new(tx)
        });
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(level);
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Level;
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;

    // logind's inhibitor lives as long as the process holding it, so a sleeping
    // child holds it and is killed to let go. Desktops without systemd get nothing,
    // which is what they got before.
    static CHILD: Mutex<Option<(Child, Level)>> = Mutex::new(None);

    pub fn apply(level: Option<Level>) {
        let mut slot = CHILD.lock().unwrap_or_else(|e| e.into_inner());
        let wanted = match level {
            Some(Level::System) | Some(Level::Display) => level,
            _ => None,
        };
        if slot.as_ref().map(|c| c.1) == wanted {
            return;
        }
        if let Some((mut child, _)) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let Some(level) = wanted else { return };
        let why = if level == Level::Display { "Video call in progress" } else { "Call in progress" };
        let child = Command::new("systemd-inhibit")
            .args(["--what=idle:sleep", "--who=Vector", "--mode=block", &format!("--why={why}"), "sleep", "infinity"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(child) = child {
            *slot = Some((child, level));
        }
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod imp {
    pub fn apply(_level: Option<super::Level>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strongest_live_hold_wins_and_dropping_one_keeps_the_rest() {
        let a = hold(Level::NoNap);
        let b = hold(Level::Display);
        assert_eq!(HOLDS.lock().unwrap().iter().max().copied(), Some(Level::Display));
        drop(b);
        assert_eq!(HOLDS.lock().unwrap().iter().max().copied(), Some(Level::NoNap));
        drop(a);
        assert!(HOLDS.lock().unwrap().is_empty());
    }
}
