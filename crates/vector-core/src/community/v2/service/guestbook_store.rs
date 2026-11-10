//! The Guestbook as a pass reads into it ([`Held`]): which of its plane's wraps this device has
//! read, by id, beside the entries themselves in the Guestbook store.
//!
//! A wrap's id is recorded only after its entry is stored (or it was refused), so a relay is
//! never told "I hold this" of an entry that a crash kept out.

use std::collections::HashSet;
use std::sync::Mutex;

use nostr_sdk::prelude::*;

use super::super::community::CommunityV2;
use super::super::{derive, guestbook, stream};
use super::control_sync::{Absorbed, Held};

/// What a Guestbook wrap is opened under: bump it with any change to what opens or parses.
/// Wraps refused under another value are forgotten and judged again.
const OPEN_RULES: u32 = 1;

/// Refusals forgotten per day, oldest first, so a wrong one is judged again.
const REJUDGED_DAILY: usize = 200;

pub(super) struct GuestbookView<'a> {
    community: &'a CommunityV2,
    cid_hex: String,
    group: derive::GroupKey,
    plane: [u8; 32],
    /// Entries a pass stored that the Guestbook did not hold.
    added: Mutex<Vec<guestbook::GuestbookEvent>>,
}

impl<'a> GuestbookView<'a> {
    pub fn of(community: &'a CommunityV2) -> Self {
        let group = derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
        let plane = group.pk().to_bytes();
        GuestbookView { community, cid_hex: crate::simd::hex::bytes_to_hex_32(&community.id().0), group, plane, added: Mutex::new(Vec::new()) }
    }

    /// The entries new to the Guestbook since this was made.
    pub fn added(self) -> Vec<guestbook::GuestbookEvent> {
        self.added.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    /// Record `wrap` as read: its entry came in live and is stored.
    pub fn hold(&self, wrap: &Event) {
        if wrap.kind.as_u16() == stream::KIND_WRAP && wrap.pubkey == self.group.pk() && wrap.verify().is_ok() {
            let _ = crate::db::community::store_guestbook_wraps(&self.cid_hex, &self.plane, crate::clock::now_secs(), &[(wrap.id.to_bytes(), wrap.created_at.as_secs(), 0)]);
        }
    }
}

impl Held for GuestbookView<'_> {
    fn id(&self) -> [u8; 32] {
        self.plane
    }

    fn pk(&self) -> PublicKey {
        self.group.pk()
    }

    fn name(&self) -> &'static str {
        "Guestbook"
    }

    fn state_key(&self, cid_hex: &str) -> String {
        format!("gbk_reads:{cid_hex}")
    }

    fn held(&self) -> Result<Vec<(EventId, u64)>, String> {
        Ok(crate::db::community::guestbook_wrap_ids(&self.cid_hex, &self.plane)?.into_iter().map(|(id, at)| (EventId::from_byte_array(id), at)).collect())
    }

    fn absorb(&self, wraps: &[Event]) -> Result<Absorbed, String> {
        let held: HashSet<EventId> = self.held()?.into_iter().map(|(id, _)| id).collect();
        let mut out = Absorbed::default();
        let mut taken: HashSet<EventId> = HashSet::new();
        let mut fit: Vec<&Event> = Vec::new();
        for w in wraps {
            if w.kind.as_u16() != stream::KIND_WRAP || w.pubkey != self.group.pk() || held.contains(&w.id) || !taken.insert(w.id) {
                continue;
            }
            if w.verify().is_err() {
                out.unfit.push(w.id);
                continue;
            }
            fit.push(w);
        }
        if fit.is_empty() {
            return Ok(out);
        }
        let mut rows: Vec<([u8; 32], u64, u32)> = Vec::with_capacity(fit.len());
        let mut entries: Vec<guestbook::GuestbookEvent> = Vec::new();
        let batch = stream::check_page(&fit, &self.group);
        for w in &fit {
            let entry = stream::open_wrap(w, &self.group).ok().and_then(|opened| guestbook::parse_guestbook_event(&opened).ok());
            match entry {
                Some(entry) => {
                    out.opened += 1;
                    entries.push(entry);
                    rows.push((w.id.to_bytes(), w.created_at.as_secs(), 0));
                }
                None => {
                    out.refused += 1;
                    rows.push((w.id.to_bytes(), w.created_at.as_secs(), OPEN_RULES));
                }
            }
        }
        drop(batch);
        let newest = fit.iter().map(|w| w.created_at.as_secs()).max().unwrap_or(0);
        let added = super::merge_guestbook(self.community, entries, newest)?;
        // Only now: the entries are stored, so the ids may be told to a relay as held.
        crate::db::community::store_guestbook_wraps(&self.cid_hex, &self.plane, crate::clock::now_secs(), &rows)?;
        self.added.lock().unwrap_or_else(|e| e.into_inner()).extend(added);
        Ok(out)
    }

    fn rejudge(&self, due: bool) {
        let _ = crate::db::community::forget_guestbook_wraps(&self.cid_hex, &self.plane, OPEN_RULES, if due { REJUDGED_DAILY } else { 0 });
    }

    fn purge(&self) {
        let _ = crate::db::community::purge_guestbook_wraps(&self.cid_hex);
    }
}
