//! Keeps every at-rest write on the key the store uses. A writer holds a [`Sealing`] ticket from
//! before it reads the vault (or the enabled flag) until its row is written; a migration holds
//! [`KeyChange`] from before its transaction until the vault and the flag show the result. A value
//! sealed under one key can then never land after the store has moved to another.
//!
//! Tickets are not `Send`, so on native neither can be held across an `.await` in a spawned task:
//! the encrypt and the write it protects stay one synchronous step. The web's tasks need not be
//! `Send`, so there that is a rule, enforced by a panic if it is ever broken.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::{Condvar, Mutex};

struct State {
    writers: usize,
    changing: bool,
    waiting: usize,
}

static STATE: Mutex<State> = Mutex::new(State { writers: 0, changing: false, waiting: 0 });
static TURN: Condvar = Condvar::new();

thread_local! {
    /// Sealing tickets this thread holds; nested writers on one thread never wait on themselves.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Set while this thread runs a migration, so anything it calls passes straight through.
    static CHANGING_HERE: Cell<bool> = const { Cell::new(false) };
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(not(target_arch = "wasm32"))]
fn wait(s: std::sync::MutexGuard<'static, State>) -> std::sync::MutexGuard<'static, State> {
    TURN.wait(s).unwrap_or_else(|e| e.into_inner())
}

/// One thread on the web: a ticket held across an `.await` is the only way to get here, and
/// waiting would never end.
#[cfg(target_arch = "wasm32")]
fn wait(_s: std::sync::MutexGuard<'static, State>) -> std::sync::MutexGuard<'static, State> {
    panic!("at-rest gate contended on a single thread: a ticket was held across an await");
}

/// Held by a writer across its encrypt and its write.
#[must_use]
pub struct Sealing {
    /// Only a thread's outermost ticket outside a migration counts as a writer.
    counted: bool,
    _not_send: PhantomData<*const ()>,
}

/// Wait out any key change, then hold one off until this ticket drops.
pub fn sealing() -> Sealing {
    let counted = !CHANGING_HERE.with(Cell::get) && DEPTH.with(Cell::get) == 0;
    if counted {
        let mut s = lock();
        // A queued key change goes first, so a steady stream of writes can't starve it.
        while s.changing || s.waiting > 0 {
            s = wait(s);
        }
        s.writers += 1;
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    Sealing { counted, _not_send: PhantomData }
}

impl Drop for Sealing {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        if self.counted {
            let mut s = lock();
            s.writers -= 1;
            if s.writers == 0 {
                TURN.notify_all();
            }
        }
    }
}

/// Whether this thread may seal a value right now: inside a writer's ticket or a migration.
pub fn held() -> bool {
    DEPTH.with(Cell::get) > 0 || CHANGING_HERE.with(Cell::get)
}

/// Held by a migration from before its transaction until the vault and the flag are updated.
#[must_use]
pub struct KeyChange {
    _not_send: PhantomData<*const ()>,
}

/// Wait for in-flight writers to land, then hold new ones off until this ticket drops.
pub fn key_change() -> Result<KeyChange, String> {
    if DEPTH.with(Cell::get) > 0 || CHANGING_HERE.with(Cell::get) {
        return Err("A key change can't start inside an at-rest write".to_string());
    }
    let mut s = lock();
    s.waiting += 1;
    while s.changing || s.writers > 0 {
        s = wait(s);
    }
    s.waiting -= 1;
    s.changing = true;
    drop(s);
    CHANGING_HERE.with(|c| c.set(true));
    Ok(KeyChange { _not_send: PhantomData })
}

/// [`key_change`] for an account switch: waits out writers in flight. Nested inside a write or a
/// migration on this thread it holds nothing, since waiting there would wait on itself.
pub fn switching() -> Option<KeyChange> {
    key_change().ok()
}

impl Drop for KeyChange {
    fn drop(&mut self) {
        CHANGING_HERE.with(|c| c.set(false));
        lock().changing = false;
        TURN.notify_all();
    }
}

/// Seal outside a ticket: an error in tests, where every writer runs; a warning elsewhere.
pub(crate) fn require_held(what: &str) {
    if held() {
        return;
    }
    if cfg!(test) {
        panic!("{what} sealed outside the at-rest gate");
    }
    crate::log_warn!("[Encryption] {what} sealed outside the at-rest gate");
}

/// The key a seal uses for the account this work writes to: `None` for a plaintext store. The
/// vault and the flag describe the account on screen; a late task of a swapped-out account asks
/// its own store, and an encrypted one is refused, since its key left with it.
pub(crate) fn sealing_key() -> Result<Option<[u8; 32]>, String> {
    if crate::db::writes_to_live_account() {
        if !crate::state::is_encryption_enabled_fast() {
            return Ok(None);
        }
        return crate::state::ENCRYPTION_KEY
            .get()
            .map(Some)
            .ok_or_else(|| "encryption enabled but key vault is empty".to_string());
    }
    let enabled = crate::db::get_sql_setting("encryption_enabled".to_string())?;
    let security = crate::db::get_sql_setting("security_type".to_string())?;
    if crate::state::resolve_encryption_enabled(enabled.as_deref(), security.as_deref()) {
        Err("The account changed before this write; its key is no longer unlocked".to_string())
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn nested_sealing_on_one_thread_does_not_wait() {
        let _a = sealing();
        let _b = sealing();
        assert!(held());
        assert!(key_change().is_err(), "a key change inside a write would wait on itself");
    }

    #[test]
    fn a_key_change_waits_for_writers_and_holds_new_ones_off() {
        let writing = sealing();
        let changed = Arc::new(AtomicBool::new(false));
        let flag = changed.clone();
        let migration = std::thread::spawn(move || {
            let _k = key_change().unwrap();
            flag.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            flag.store(false, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(!changed.load(Ordering::SeqCst), "the key change waits for the write in flight");
        drop(writing);
        while !changed.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        let _after = sealing();
        assert!(!changed.load(Ordering::SeqCst), "a new write waits until the key change is done");
        migration.join().unwrap();
    }
}
