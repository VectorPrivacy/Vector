//! A Locked Channel's sealed history (CORD-03 §1): the wrap ids of what stood at its read address
//! before the lock, which is all a reader fetches from there. Anyone holding the read key can
//! date a wrap into the past; a list fetched by id leaves nothing they add in reach.
//!
//! Pages ride the lock's writer address, newest first, each naming the next by wrap id and the
//! hash of its content, so a reader opens one only when it scrolls into its range and checks each
//! against the one before.

use super::stream::{self, OpenedStream, SealForm};
use super::{chat, kind};
use crate::community::{ChannelId, Epoch};
use nostr_sdk::prelude::{Event, PublicKey, UnsignedEvent};

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
    /// The next, older page: its wrap id and the hash of its content.
    pub next: Option<([u8; 32], [u8; 32])>,
}

fn hex(b: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(b)
}

impl SealPage {
    pub fn content(&self) -> String {
        let ids: String = self.ids.iter().map(hex).collect();
        let next = self.next.map(|(id, h)| serde_json::json!([hex(&id), hex(&h)])).unwrap_or(serde_json::Value::Null);
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
            serde_json::Value::Array(n) if n.len() == 2 => Some((crate::simd::hex::hex_to_bytes_32_checked(n[0].as_str()?)?, crate::simd::hex::hex_to_bytes_32_checked(n[1].as_str()?)?)),
            _ => return None,
        };
        (oldest <= newest).then_some(SealPage { ids, oldest, newest, next })
    }
}

/// What a page's predecessor commits to: the hash of its content.
pub fn content_hash(content: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(content.as_bytes()).into()
}

/// `ids` (each with its date) as pages, newest first, not yet linked.
pub fn paginate(mut ids: Vec<([u8; 32], u64)>) -> Vec<SealPage> {
    ids.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ids.dedup_by(|a, b| a.0 == b.0);
    ids.chunks(MAX_SEAL_IDS)
        .map(|c| SealPage { ids: c.iter().map(|(id, _)| *id).collect(), oldest: c.last().map_or(0, |x| x.1), newest: c.first().map_or(0, |x| x.1), next: None })
        .collect()
}

/// The rumor a page rides in: bound to the Channel and the read epoch it opens under, committing
/// the writer epoch whose address carries it and the sealer's certificate there.
pub fn page_rumor(author: PublicKey, channel: &ChannelId, read: Epoch, window: Epoch, cert: Option<chat::WriterCert>, page: &SealPage, at_ms: u64) -> UnsignedEvent {
    let rumor = stream::build_rumor_ms(kind::SEAL_PAGE, author, &page.content(), stream::channel_binding_tags(channel, read), at_ms);
    chat::with_writer_cert(rumor, window, cert)
}

/// A page fetched by id, when it is the one named: carried at the window's writer address,
/// opened under the read epoch's key, bound to the Channel and both epochs, its content hashing to
/// `hash`. The opened rumor comes back too, for its author.
pub fn open_page(wrap: &Event, writer: &PublicKey, window: Epoch, conv: &nostr_sdk::prelude::nip44::v2::ConversationKey, channel: &ChannelId, read: Epoch, hash: &[u8; 32]) -> Option<(SealPage, OpenedStream)> {
    if wrap.pubkey != *writer {
        return None;
    }
    let opened = stream::open_wrap_at(wrap, writer, conv, true).ok()?;
    if opened.seal_form != SealForm::Encrypted || opened.rumor.kind.as_u16() != kind::SEAL_PAGE {
        return None;
    }
    stream::check_channel_binding(&opened.rumor, channel, read).ok()?;
    if chat::writer_epoch_of(&opened.rumor).ok()? != Some(window) || content_hash(&opened.rumor.content) != *hash {
        return None;
    }
    SealPage::parse(&opened.rumor.content).map(|p| (p, opened))
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
        let ids: Vec<([u8; 32], u64)> = (0..(MAX_SEAL_IDS as u32 * 2 + 5)).map(|n| (id(n), 1_000 + n as u64)).collect();
        let pages = paginate(ids);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].ids.len(), MAX_SEAL_IDS);
        assert!(pages[0].newest > pages[0].oldest && pages[0].oldest > pages[1].newest, "newest first, no overlap");
        let mut p = pages[2].clone();
        p.next = Some((id(7), [9; 32]));
        assert_eq!(SealPage::parse(&p.content()), Some(p));
        assert_eq!(SealPage::parse(&pages[0].content()).unwrap(), pages[0]);
    }

    #[test]
    fn a_malformed_page_is_none() {
        let good = paginate(vec![(id(1), 5)]).remove(0).content();
        for bad in [
            good.replace("\"ids\":\"", "\"ids\":\"0"),
            good.replace("[5,5]", "[6,5]"),
            good.replace("\"next\":null", "\"next\":[\"zz\",\"zz\"]"),
            serde_json::json!({ "ids": "00".repeat(32 * (MAX_SEAL_IDS + 1)), "range": [1, 2], "next": null }).to_string(),
        ] {
            assert_eq!(SealPage::parse(&bad), None, "{bad:.80}");
        }
    }

    #[test]
    fn a_full_page_fits_a_relay_event() {
        let keys = nostr_sdk::prelude::Keys::generate();
        let channel = ChannelId([7; 32]);
        let group = super::super::derive::channel_group_key(&[1; 32], &channel, Epoch(1));
        let page = SealPage { ids: (0..MAX_SEAL_IDS as u32).map(|n| [n as u8 | 0x80; 32]).collect(), oldest: u64::MAX / 4, newest: u64::MAX / 2, next: Some(([0xff; 32], [0xee; 32])) };
        let rumor = page_rumor(keys.public_key(), &channel, Epoch(u32::MAX as u64), Epoch(u32::MAX as u64), Some(chat::WriterCert::Full([0xab; 64])), &page, u64::MAX / 2000);
        let seal = stream::build_seal(&rumor, SealForm::Encrypted, &group, &keys).unwrap();
        let (wrap, _) = stream::wrap_seal(&seal, &group, stream::KIND_WRAP, nostr_sdk::prelude::Timestamp::now()).unwrap();
        let size = wrap.as_json().len();
        assert!(size <= 65_536, "a full page's wrap is {size} bytes");
        let (opened, _) = open_page(&wrap, &group.pk(), Epoch(u32::MAX as u64), group.conv_key(), &channel, Epoch(u32::MAX as u64), &content_hash(&page.content())).unwrap();
        assert_eq!(opened, page);
        assert!(open_page(&wrap, &group.pk(), Epoch(u32::MAX as u64), group.conv_key(), &channel, Epoch(u32::MAX as u64), &[0; 32]).is_none(), "only the page its hash names");
    }
}
