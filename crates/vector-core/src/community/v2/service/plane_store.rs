//! The wraps of a community's rekey and access planes as a pass reads into them ([`Held`]): kept
//! whole and sealed at rest beside the Control Plane's, so a sync asks each relay only for what
//! is not held. A wrap is kept whole when it opens under the plane's key; what it carries is
//! parsed where it is read.

use nostr_sdk::prelude::*;

use super::super::community::CommunityV2;
use super::super::derive::GroupKey;
use super::super::stream;
use super::control_store::{self, OPEN_RULES, REJUDGED_DAILY};
use super::control_sync::{Absorbed, Held};

pub(super) struct PlaneView<'a> {
    cid_hex: String,
    group: &'a GroupKey,
    id: [u8; 32],
}

impl<'a> PlaneView<'a> {
    pub fn of(community: &CommunityV2, group: &'a GroupKey) -> Self {
        let mut input = b"concord/plane-store".to_vec();
        input.extend_from_slice(&group.pk().to_bytes());
        let id: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&input).into();
        PlaneView { cid_hex: crate::simd::hex::bytes_to_hex_32(&community.id().0), group, id }
    }

    /// Every wrap held, newest first.
    pub fn all(&self) -> Result<Vec<Event>, String> {
        let mut out = Vec::new();
        let mut below: Option<(u64, EventId)> = None;
        loop {
            let page = control_store::held_page(&self.cid_hex, &self.id, below, 500)?;
            let Some(last) = page.last() else { return Ok(out) };
            below = Some((last.created_at.as_secs(), last.id));
            out.extend(page);
        }
    }
}

impl Held for PlaneView<'_> {
    fn id(&self) -> [u8; 32] {
        self.id
    }

    fn pk(&self) -> PublicKey {
        self.group.pk()
    }

    fn name(&self) -> &'static str {
        "plane"
    }

    fn state_key(&self, cid_hex: &str) -> String {
        format!("pln_reads:{cid_hex}:{}", crate::simd::hex::bytes_to_hex_32(&self.id))
    }

    fn plane_keys(&self) -> Option<&Keys> {
        Some(self.group.keys())
    }

    fn held(&self) -> Result<Vec<(EventId, u64)>, String> {
        Ok(crate::db::community::control_wrap_ids(&self.cid_hex, &self.id)?.into_iter().map(|(id, at)| (EventId::from_byte_array(id), at)).collect())
    }

    fn absorb(&self, wraps: &[Event]) -> Result<Absorbed, String> {
        let group = self.group;
        control_store::absorb_wraps(&self.cid_hex, &self.id, group.pk(), wraps, &|fit| {
            let _batch = stream::check_page(fit, group);
            fit.iter().map(|w| stream::open_wrap(w, group).is_ok()).collect()
        })
    }

    fn rejudge(&self, due: bool) {
        let _ = crate::db::community::forget_control_refusals(&self.cid_hex, &self.id, OPEN_RULES, if due { REJUDGED_DAILY } else { 0 });
    }

    fn at_cap(&self) -> bool {
        let (max_bodies, max_bytes, max_refusals) = control_store::caps();
        crate::db::community::control_wrap_counts(&self.cid_hex, &self.id).is_ok_and(|(bodies, refusals, bytes)| bodies >= max_bodies || bytes >= max_bytes || refusals >= max_refusals)
    }

    fn purge(&self) {
        let _ = crate::db::community::purge_control_wraps(&self.cid_hex);
    }
}
