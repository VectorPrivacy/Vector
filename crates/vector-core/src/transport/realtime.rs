//! Calls and Mini App realtime ride iroh, which never goes through the chosen network. Under any
//! kind but Clearnet they need the user's consent, recorded on the session for the kind in use.

use std::sync::atomic::{AtomicBool, Ordering};

use super::Kind;

/// Exact: the frontend matches it to show the consent prompt.
pub const CONSENT_REQUIRED: &str = "realtime_consent_required";

static ALLOW_ALWAYS: AtomicBool = AtomicBool::new(false);

/// False unless the caller's session is live. Clearnet: true. Unknown: false.
pub fn allowed() -> bool {
    check().is_ok()
}

pub fn check() -> Result<(), String> {
    if !crate::db::session_is_live() {
        return Err("Vector is still connecting.".into());
    }
    match super::preference() {
        None => Err("Vector is still connecting.".into()),
        Some(Kind::Clearnet) => Ok(()),
        Some(k) => {
            if ALLOW_ALWAYS.load(Ordering::Acquire) || super::prefs::current().consent() == Some(k) {
                Ok(())
            } else {
                Err(CONSENT_REQUIRED.into())
            }
        }
    }
}

/// Record consent for the kind in use, on the live session: any account change or kind change
/// drops it.
pub fn grant() {
    if let Some(k) = super::preference().filter(|k| *k != Kind::Clearnet) {
        super::prefs::live().set_consent(Some(k));
        super::host::notify(super::host::TransportEvent::Changed);
    }
}

/// Process-wide opt-in, for SDK bots that accept realtime outside the network.
pub fn allow_always(on: bool) {
    ALLOW_ALWAYS.store(on, Ordering::Release);
}
