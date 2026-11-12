//! A Locked Channel's sealed history (CORD-03 §1): the wrap ids of what stood at its read address
//! before the lock, which is all a reader fetches from there. Anyone holding the read key can
//! date a wrap into the past; a list fetched by id leaves nothing they add in reach.
//!
//! Each page lists the ids one read plane carried and rides that plane, signed and encrypted by
//! its key and dated inside what the seal covers so nobody reads it by date. A member opens only
//! the pages for history they can read. Pages run newest first, each naming the next by event id,
//! which commits to its content; a reader fetches one only when it scrolls into its range, and a
//! later seal's oldest page names an earlier seal's first, so one chain holds every lock.

use super::derive::GroupKey;
use super::stream::{self, SealForm};
use super::kind;
use crate::community::{ChannelId, Epoch};
use nostr_sdk::prelude::{Event, Keys, Timestamp};

/// The most ids one page names: its wrap, double-encrypted and padded, stays well under the
/// 64 KB a relay takes.
pub const MAX_SEAL_IDS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealPage {
    /// Wrap ids, newest first.
    pub ids: Vec<[u8; 32]>,
    /// The dates (unix ms) of the oldest and newest event it names.
    pub oldest: u64,
    pub newest: u64,
    /// The next, older page: its event id and the read epoch it opens under.
    pub next: Option<([u8; 32], Epoch)>,
}

fn hex(b: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(b)
}

impl SealPage {
    pub fn content(&self) -> String {
        let ids: String = self.ids.iter().map(hex).collect();
        let next = self.next.map(|(id, read)| serde_json::json!([hex(&id), read.0])).unwrap_or(serde_json::Value::Null);
        serde_json::json!({ "ids": ids, "range": [self.oldest, self.newest], "next": next }).to_string()
    }

    /// `None` for anything but a whole page: ids in 64-hex runs, at most [`MAX_SEAL_IDS`], a
    /// range whose ends are in order.
    pub fn parse(content: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(content).ok()?;
        let raw = v.get("ids")?.as_str()?;
        if raw.len() % 64 != 0 || raw.len() / 64 > MAX_SEAL_IDS {
            return None;
        }
        let ids = (0..raw.len() / 64).map(|i| crate::simd::hex::hex_to_bytes_32_checked(&raw[i * 64..(i + 1) * 64])).collect::<Option<Vec<_>>>()?;
        let range = v.get("range")?.as_array().filter(|r| r.len() == 2)?;
        let (oldest, newest) = (range[0].as_u64()?, range[1].as_u64()?);
        let next = match v.get("next")? {
            serde_json::Value::Null => None,
            serde_json::Value::Array(n) if n.len() == 2 => Some((crate::simd::hex::hex_to_bytes_32_checked(n[0].as_str()?)?, Epoch(n[1].as_u64()?))),
            _ => return None,
        };
        (oldest <= newest).then_some(SealPage { ids, oldest, newest, next })
    }
}

/// `ids` (each with its date and the read plane that carried it) as pages, newest first, not yet
/// linked: a page holds one plane's ids, so it breaks wherever the plane changes.
pub fn paginate<P: PartialEq + Copy>(mut ids: Vec<([u8; 32], u64, P)>) -> Vec<(SealPage, P)> {
    ids.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ids.dedup_by(|a, b| a.0 == b.0);
    let mut out: Vec<(SealPage, P)> = Vec::new();
    for (id, at, plane) in ids {
        match out.last_mut() {
            Some((page, e)) if *e == plane && page.ids.len() < MAX_SEAL_IDS => {
                page.ids.push(id);
                page.oldest = at;
            }
            _ => out.push((SealPage { ids: vec![id], oldest: at, newest: at, next: None }, plane)),
        }
    }
    out
}

/// `page` as a wrap on the read plane `group` of epoch `read`, bound to the Channel, dated `at_ms`.
/// Its owner is a key nobody keeps, so a relay that judges deletion by owner deletes it for
/// nobody. Its event id is what names it.
pub fn page_wrap(page: &SealPage, channel: &ChannelId, read: Epoch, group: &GroupKey, at_ms: u64) -> Result<Event, String> {
    let rumor = stream::build_rumor_ms(kind::SEAL_PAGE, group.pk(), &page.content(), stream::channel_binding_tags(channel, read), at_ms);
    let seal = stream::build_seal(&rumor, SealForm::Encrypted, group, group.keys()).map_err(|e| e.to_string())?;
    stream::wrap_seal_for(&seal, group, stream::KIND_WRAP, Timestamp::from_secs(at_ms / 1000), &[], &Keys::generate().public_key()).map_err(|e| e.to_string())
}

/// The page `id` names, when `wrap` is it: that id, on the read plane at `address` of epoch `read`,
/// opened under its key and bound to the Channel and that epoch.
pub fn open_page(wrap: &Event, id: &[u8; 32], channel: &ChannelId, read: Epoch, address: &nostr_sdk::prelude::PublicKey, conv: &nostr_sdk::prelude::nip44::v2::ConversationKey) -> Option<SealPage> {
    if wrap.id.to_bytes() != *id || wrap.pubkey != *address {
        return None;
    }
    let opened = stream::open_wrap_at(wrap, address, conv, true).ok()?;
    if opened.seal_form != SealForm::Encrypted || opened.rumor.kind.as_u16() != kind::SEAL_PAGE {
        return None;
    }
    stream::check_channel_binding(&opened.rumor, channel, read).ok()?;
    SealPage::parse(&opened.rumor.content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&n.to_be_bytes());
        b
    }

    #[test]
    fn pages_run_newest_first_and_round_trip() {
        let ids: Vec<([u8; 32], u64, Epoch)> = (0..(MAX_SEAL_IDS as u32 * 2 + 5)).map(|n| (id(n), 1_000 + n as u64, Epoch(1))).collect();
        let pages: Vec<SealPage> = paginate(ids).into_iter().map(|(p, _)| p).collect();
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].ids.len(), MAX_SEAL_IDS);
        assert!(pages[0].newest > pages[0].oldest && pages[0].oldest > pages[1].newest, "newest first, no overlap");
        let mut p = pages[2].clone();
        p.next = Some((id(7), Epoch(4)));
        assert_eq!(SealPage::parse(&p.content()), Some(p));
        assert_eq!(SealPage::parse(&pages[0].content()).unwrap(), pages[0]);
    }

    #[test]
    fn a_page_holds_one_read_epoch() {
        let ids = vec![(id(1), 50, Epoch(2)), (id(2), 40, Epoch(2)), (id(3), 30, Epoch(1)), (id(4), 20, Epoch(1))];
        let pages = paginate(ids);
        assert_eq!(pages.iter().map(|(p, e)| (p.ids.len(), *e, p.newest, p.oldest)).collect::<Vec<_>>(), vec![(2, Epoch(2), 50, 40), (2, Epoch(1), 30, 20)]);
    }

    #[test]
    fn a_malformed_page_is_none() {
        let good = paginate(vec![(id(1), 5, Epoch(1))]).remove(0).0.content();
        for bad in [
            good.replace("\"ids\":\"", "\"ids\":\"0"),
            good.replace("[5,5]", "[6,5]"),
            good.replace("\"next\":null", "\"next\":[\"zz\",1]"),
            serde_json::json!({ "ids": "00".repeat(32 * (MAX_SEAL_IDS + 1)), "range": [1, 2], "next": null }).to_string(),
        ] {
            assert_eq!(SealPage::parse(&bad), None, "{bad:.80}");
        }
    }

    #[test]
    fn a_full_page_fits_a_relay_event_and_opens_only_as_the_page_named() {
        use super::super::derive;
        let channel = ChannelId([7; 32]);
        let group = derive::channel_group_key(&[1; 32], &channel, Epoch(1));
        let page = SealPage { ids: (0..MAX_SEAL_IDS as u32).map(|n| [n as u8 | 0x80; 32]).collect(), oldest: u64::MAX / 4, newest: u64::MAX / 2, next: Some(([0xff; 32], Epoch(u32::MAX as u64))) };
        let read = Epoch(u32::MAX as u64);
        let wrap = page_wrap(&page, &channel, read, &group, u64::MAX / 2000).unwrap();
        assert_eq!(wrap.pubkey, group.pk(), "on the plane it lists");
        let size = wrap.as_json().len();
        assert!(size <= 65_536, "a full page's wrap is {size} bytes");
        let opens = |id: &[u8; 32], ch: &ChannelId, at: &GroupKey, conv: &GroupKey| open_page(&wrap, id, ch, read, &at.pk(), conv.conv_key());
        assert_eq!(opens(&wrap.id.to_bytes(), &channel, &group, &group), Some(page.clone()));
        assert!(opens(&[0; 32], &channel, &group, &group).is_none(), "only as the page its id names");
        assert!(opens(&wrap.id.to_bytes(), &ChannelId([8; 32]), &group, &group).is_none(), "in its own Channel");
        let other = derive::channel_group_key(&[2; 32], &channel, Epoch(1));
        assert!(opens(&wrap.id.to_bytes(), &channel, &group, &other).is_none(), "under its own read key");
        assert!(opens(&wrap.id.to_bytes(), &channel, &other, &group).is_none(), "on its own plane");

    }
}
