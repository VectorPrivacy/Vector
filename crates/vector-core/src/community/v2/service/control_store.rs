//! Every Control wrap this device has read, kept per plane view, so a sync tells each relay what
//! it holds already and asks only for the rest.
//!
//! A wrap's id is held only together with its verdict (its body once it opened here, or a
//! refusal), and only after the id and signature were checked against the event itself. Relays
//! are told "I hold this id", so an id held without the wrap behind it is an edition never asked
//! for again.

use std::collections::HashSet;

use nostr_sdk::prelude::*;

use super::super::community::CommunityV2;
use super::super::{control, stream};
use super::control_sync::{Absorbed, Held};

/// What a wrap is opened under: bump it with any change to what opens. Refusals judged under
/// another value are forgotten and judged again.
pub(super) const OPEN_RULES: u32 = 1;

/// A real plane holds hundreds to a few thousand wraps. Past these a view takes no more. No more
/// bodies than a fold reads, or a full view would fold short for good instead of being flooded.
const MAX_BODIES: usize = super::FOLLOW_MAX_PAGES * super::FOLLOW_PAGE;
const MAX_BODY_BYTES: usize = 64 << 20;
const MAX_REFUSALS: usize = 100_000;

#[cfg(test)]
std::thread_local! {
    static CAPS: std::cell::Cell<Option<(usize, usize, usize)>> = const { std::cell::Cell::new(None) };
}

/// (bodies, body bytes, refusals) a test holds a view to.
#[cfg(test)]
pub(super) fn set_caps_for_test(caps: Option<(usize, usize, usize)>) {
    CAPS.with(|c| c.set(caps));
}

pub(super) fn caps() -> (usize, usize, usize) {
    #[cfg(test)]
    if let Some(caps) = CAPS.with(|c| c.get()) {
        return caps;
    }
    (MAX_BODIES, MAX_BODY_BYTES, MAX_REFUSALS)
}

/// Refusals forgotten per day, oldest first, so a wrong one is judged again.
pub(super) const REJUDGED_DAILY: usize = 200;

/// Whether follows use the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Off,
    /// Kept and read beside the network read, and the two compared.
    Shadow,
    On,
}

#[cfg(test)]
std::thread_local! {
    static MODE: std::cell::Cell<Option<Mode>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn set_mode_for_test(mode: Option<Mode>) {
    MODE.with(|m| m.set(mode));
}

pub(super) fn mode() -> Mode {
    #[cfg(test)]
    if let Some(mode) = MODE.with(|m| m.get()) {
        return mode;
    }
    static FROM_ENV: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    *FROM_ENV.get_or_init(|| match std::env::var("VECTOR_CONTROL_STORE").as_deref() {
        Ok("off") => Mode::Off,
        Ok("shadow") => Mode::Shadow,
        _ => Mode::On,
    })
}

/// The vault key follows the account on screen, not the task: after a swap this task's database
/// is still its own, and the key is not. Nothing is sealed, opened or dropped then.
pub(super) fn swapped_out() -> Result<(), String> {
    if crate::db::writes_to_live_account() {
        Ok(())
    } else {
        Err("the account changed".into())
    }
}

/// A community's Control Plane as this member reads it now, and what is held of it.
pub(super) struct View {
    cid_hex: String,
    id: [u8; 32],
    plane: control::ControlPlane,
}

impl View {
    pub fn of(community: &CommunityV2) -> View {
        let plane = control::ControlPlane::of(community);
        View { cid_hex: crate::simd::hex::bytes_to_hex_32(&community.id().0), id: plane.view_id(), plane }
    }

    /// Every wrap held, refused ones too: what a relay is told this device has.
    pub fn held(&self) -> Result<Vec<(EventId, u64)>, String> {
        Ok(crate::db::community::control_wrap_ids(&self.cid_hex, &self.id)?.into_iter().map(|(id, at)| (EventId::from_byte_array(id), at)).collect())
    }

    /// (bodies, refusals, body bytes).
    pub fn counts(&self) -> Result<(usize, usize, usize), String> {
        crate::db::community::control_wrap_counts(&self.cid_hex, &self.id)
    }

    pub fn at_cap(&self) -> bool {
        let (max_bodies, max_bytes, max_refusals) = caps();
        self.counts().is_ok_and(|(bodies, refusals, bytes)| bodies >= max_bodies || bytes >= max_bytes || refusals >= max_refusals)
    }

    /// Judge `wraps` and hold each with its verdict, all in one write. A wrap already held, or
    /// not of this plane, is passed over.
    pub fn absorb(&self, wraps: &[Event]) -> Result<Absorbed, String> {
        let plane = &self.plane;
        absorb_wraps(&self.cid_hex, &self.id, plane.pk(), wraps, &|fit| {
            let _batch = plane.check_page(fit);
            fit.iter().map(|w| plane.open_edition(w).is_ok()).collect()
        })
    }

    /// The next `limit` wraps below `below` (a wrap's timestamp and id), newest first.
    pub fn page(&self, below: Option<(u64, EventId)>, limit: usize) -> Result<Vec<Event>, String> {
        held_page(&self.cid_hex, &self.id, below, limit)
    }

    /// Stop holding `id`: a stored wrap that did not open after all.
    pub fn forget(&self, id: &EventId) {
        forget_wrap(&self.cid_hex, &self.id, id);
    }

    /// Forget the refusals due to be judged again: all judged under other rules, and once a day
    /// (`due`) the longest standing few.
    pub fn rejudge(&self, due: bool) {
        let _ = crate::db::community::forget_control_refusals(&self.cid_hex, &self.id, OPEN_RULES, if due { REJUDGED_DAILY } else { 0 });
    }
}

/// Judge `wraps` for the view `id` of a plane at `pk` and hold each with its verdict, all in one
/// write: a body for each `opens` passes, a refusal for the rest. A wrap already held, or not of
/// this plane, is passed over; one whose id or signature is not its own is never held.
pub(super) fn absorb_wraps(cid_hex: &str, id: &[u8; 32], pk: PublicKey, wraps: &[Event], opens: &dyn Fn(&[&Event]) -> Vec<bool>) -> Result<Absorbed, String> {
    swapped_out()?;
    let held: HashSet<EventId> = crate::db::community::control_wrap_ids(cid_hex, id)?.into_iter().map(|(id, _)| EventId::from_byte_array(id)).collect();
    let mut out = Absorbed::default();
    let mut taken: HashSet<EventId> = HashSet::new();
    let mut fit: Vec<&Event> = Vec::new();
    for w in wraps {
        if w.kind.as_u16() != stream::KIND_WRAP || w.pubkey != pk || held.contains(&w.id) || !taken.insert(w.id) {
            continue;
        }
        // The id covers the content and not the signature, and an open on a restricted plane
        // reads the signature: a copy with either wrong is not the event that id names.
        if w.verify().is_err() {
            out.unfit.push(w.id);
            continue;
        }
        fit.push(w);
    }
    if fit.is_empty() {
        return Ok(out);
    }
    let (mut bodies, mut refusals, mut bytes) = crate::db::community::control_wrap_counts(cid_hex, id)?;
    let (max_bodies, max_bytes, max_refusals) = caps();
    let mut rows: Vec<crate::db::community::ControlWrapRow> = Vec::with_capacity(fit.len());
    for (w, opened) in fit.iter().zip(opens(&fit)) {
        let at = w.created_at.as_secs();
        if opened {
            let body = w.as_json().into_bytes();
            if bodies >= max_bodies || bytes + body.len() > max_bytes {
                out.full = true;
                continue;
            }
            bodies += 1;
            bytes += body.len();
            out.opened += 1;
            rows.push((w.id.to_bytes(), at, Some(body)));
        } else {
            if refusals >= max_refusals {
                out.full = true;
                continue;
            }
            refusals += 1;
            out.refused += 1;
            rows.push((w.id.to_bytes(), at, None));
        }
    }
    crate::db::community::store_control_wraps(cid_hex, id, OPEN_RULES, crate::clock::now_secs(), &rows)?;
    Ok(out)
}

/// The next `limit` wraps of view `id` below `below` (a wrap's timestamp and id), newest first.
/// A body that reads as JSON and not as its event is forgotten, so a sync fetches it again. One
/// that did not decrypt is an error and stays: the key at hand is not the one it was sealed under.
pub(super) fn held_page(cid_hex: &str, id: &[u8; 32], below: Option<(u64, EventId)>, limit: usize) -> Result<Vec<Event>, String> {
    swapped_out()?;
    let rows = crate::db::community::control_wrap_page(cid_hex, id, below.map(|(at, id)| (at, id.to_bytes())), limit)?;
    let mut out = Vec::with_capacity(rows.len());
    for (wrap_id, _, body) in rows {
        if body.first() != Some(&b'{') {
            return Err("a wrap held did not decrypt".into());
        }
        match std::str::from_utf8(&body).ok().and_then(|json| crate::event_json::event(json).ok()).filter(|e| e.id.as_bytes() == &wrap_id) {
            Some(event) => out.push(event),
            None => forget_wrap(cid_hex, id, &EventId::from_byte_array(wrap_id)),
        }
    }
    Ok(out)
}

/// Stop holding `wrap` in view `id`, so a sync asks for it again.
pub(super) fn forget_wrap(cid_hex: &str, id: &[u8; 32], wrap: &EventId) {
    if swapped_out().is_ok() {
        let _ = crate::db::community::drop_control_wrap(cid_hex, id, wrap.as_bytes());
    }
}

impl Held for View {
    fn id(&self) -> [u8; 32] {
        self.id
    }

    fn pk(&self) -> PublicKey {
        self.plane.pk()
    }

    fn name(&self) -> &'static str {
        "Control Plane"
    }

    // Per view: a past epoch's plane (the Audit Log reads them) keeps its own relays' reads.
    fn state_key(&self, cid_hex: &str) -> String {
        format!("ctl_reads:{cid_hex}:{}", crate::simd::hex::bytes_to_hex_32(&self.id))
    }

    fn held(&self) -> Result<Vec<(EventId, u64)>, String> {
        View::held(self)
    }

    fn absorb(&self, wraps: &[Event]) -> Result<Absorbed, String> {
        View::absorb(self, wraps)
    }

    fn rejudge(&self, due: bool) {
        View::rejudge(self, due)
    }

    fn at_cap(&self) -> bool {
        View::at_cap(self)
    }

    fn purge(&self) {
        let _ = crate::db::community::purge_control_wraps(&self.cid_hex);
    }
}
