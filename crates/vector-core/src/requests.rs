//! DM requests: a GUI shields its user from someone who wrote first until they answer or
//! accept. Off unless a GUI turns it on, so SDK clients and bots take every DM and invite
//! as they always have.

use std::sync::atomic::{AtomicBool, Ordering};

static SHIELD: AtomicBool = AtomicBool::new(false);

/// Called once at startup by a client with a person behind it.
pub fn enable_shield() {
    SHIELD.store(true, Ordering::Relaxed);
}

/// Whether a stranger's wallpaper, attachment probes and blocked invites are held back.
pub fn shielded() -> bool {
    SHIELD.load(Ordering::Relaxed)
}
