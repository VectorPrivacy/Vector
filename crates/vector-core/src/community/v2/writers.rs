//! A restricted Channel's Writers ledger (CORD-03 §1): which address takes posts when.
//!
//! The ledger is a Control entity, so every member folds the same answer and a Refounding
//! carries it. Writer secrets travel separately, on the access plane, and a member adopts one
//! only if it derives to the address the ledger names.

use super::derive::{channel_writer_group_key, writer_scope};
use crate::community::{ChannelId, Epoch};

/// The most entries a ledger holds; a longer one is malformed.
pub const MAX_LEDGER: usize = 128;

/// What one ledger entry moves the Channel to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spell {
    /// Posts land at this writer epoch's address, minted under root epoch `root`.
    Writer { epoch: Epoch, pk: [u8; 32], root: Epoch },
    /// Posts land at the read address again.
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Unix milliseconds this entry starts holding.
    pub at: u64,
    pub spell: Spell,
    /// Unknown fields, carried through our own editions (CORD-02 §6).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ledger {
    pub channel: [u8; 32],
    pub entries: Vec<Entry>,
    /// Unknown top-level fields, carried through our own editions (CORD-02 §6).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Ledger {
    pub fn new(channel: &ChannelId) -> Self {
        Ledger { channel: channel.0, entries: Vec::new(), extra: Default::default() }
    }

    /// Parse a ledger, `None` when it is malformed: entries out of `at` order, writer epochs
    /// not strictly rising, more than [`MAX_LEDGER`], or an entry that is neither one thing.
    pub fn parse(content: &str) -> Option<Self> {
        let serde_json::Value::Object(mut top) = serde_json::from_str(content).ok()? else { return None };
        let channel = crate::simd::hex::hex_to_bytes_32_checked(top.remove("channel")?.as_str()?)?;
        let serde_json::Value::Array(raw) = top.remove("ledger")? else { return None };
        if raw.len() > MAX_LEDGER {
            return None;
        }
        let mut entries = Vec::with_capacity(raw.len());
        for e in raw {
            let serde_json::Value::Object(mut e) = e else { return None };
            let at = e.remove("at")?.as_u64()?;
            let spell = match (e.remove("open"), e.remove("epoch"), e.remove("writer"), e.remove("root")) {
                (Some(serde_json::Value::Bool(true)), None, None, None) => Spell::Open,
                (None, Some(epoch), Some(writer), Some(root)) => {
                    let pk = crate::simd::hex::hex_to_bytes_32_checked(writer.as_str()?)?;
                    nostr_sdk::prelude::PublicKey::from_slice(&pk).ok()?.xonly().ok()?;
                    Spell::Writer { epoch: Epoch(epoch.as_u64().filter(|n| *n > 0)?), pk, root: Epoch(root.as_u64()?) }
                }
                _ => return None,
            };
            entries.push(Entry { at, spell, extra: e });
        }
        let ledger = Ledger { channel, entries, extra: top };
        ledger.well_formed().then_some(ledger)
    }

    fn well_formed(&self) -> bool {
        let ordered = self.entries.windows(2).all(|w| w[0].at <= w[1].at);
        let epochs: Vec<u64> = self.writers().map(|(e, ..)| e.0).collect();
        ordered && epochs.windows(2).all(|w| w[0] < w[1]) && self.entries.len() <= MAX_LEDGER
    }

    pub fn to_json(&self) -> String {
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut o = e.extra.clone();
                o.insert("at".into(), e.at.into());
                match e.spell {
                    Spell::Writer { epoch, pk, root } => {
                        o.insert("epoch".into(), epoch.0.into());
                        o.insert("writer".into(), crate::simd::hex::bytes_to_hex_32(&pk).into());
                        o.insert("root".into(), root.0.into());
                    }
                    Spell::Open => {
                        o.insert("open".into(), true.into());
                    }
                }
                serde_json::Value::Object(o)
            })
            .collect();
        let mut top = self.extra.clone();
        top.insert("channel".into(), crate::simd::hex::bytes_to_hex_32(&self.channel).into());
        top.insert("ledger".into(), entries.into());
        serde_json::Value::Object(top).to_string()
    }

    /// Whether posts land at a writer address now.
    pub fn restricted(&self) -> bool {
        matches!(self.entries.last(), Some(Entry { spell: Spell::Writer { .. }, .. }))
    }

    /// The writer epoch taking posts now.
    pub fn current_writer(&self) -> Option<(Epoch, [u8; 32])> {
        match self.entries.last()?.spell {
            Spell::Writer { epoch, pk, .. } => Some((epoch, pk)),
            Spell::Open => None,
        }
    }

    /// The root epoch the live writer epoch was minted under: a Refounding retires it.
    pub fn live_root(&self) -> Option<Epoch> {
        match self.entries.last()?.spell {
            Spell::Writer { root, .. } => Some(root),
            Spell::Open => None,
        }
    }

    /// The epoch a newly minted writer secret takes.
    pub fn next_epoch(&self) -> Epoch {
        Epoch(self.writers().map(|(e, ..)| e.0).max().unwrap_or(0) + 1)
    }

    /// Every writer address with the millisecond its entry ends, `None` while it still holds.
    pub fn writers(&self) -> impl Iterator<Item = (Epoch, [u8; 32], Option<u64>)> + '_ {
        self.entries.iter().enumerate().filter_map(|(i, e)| match e.spell {
            Spell::Writer { epoch, pk, .. } => Some((epoch, pk, self.entries.get(i + 1).map(|n| n.at))),
            Spell::Open => None,
        })
    }

    /// Whether a post at the read address dated `at_ms` stands: not inside a writer entry.
    pub fn read_address_takes(&self, at_ms: u64) -> bool {
        !matches!(self.entries.iter().rev().find(|e| e.at <= at_ms), Some(Entry { spell: Spell::Writer { .. }, .. }))
    }

    /// Whether a post at writer `epoch`'s address dated `at_ms` stands: before its entry ends.
    pub fn writer_address_takes(&self, epoch: Epoch, at_ms: u64) -> bool {
        self.writers().any(|(e, _, end)| e == epoch && end.is_none_or(|end| at_ms < end))
    }

    /// Whether a post of `kind` dated `at_ms` stands where it was read: at writer `wepoch`'s
    /// address, or at the read address when `None`.
    pub fn admits(&self, wepoch: Option<Epoch>, kind: u16, at_ms: u64) -> bool {
        match wepoch {
            Some(w) => self.writer_address_takes(w, at_ms),
            None => !super::chat::rides_writer_address(kind) || self.read_address_takes(at_ms),
        }
    }

    /// This ledger with `spell` appended at `at`, never before the last entry, its oldest entry
    /// dropped at the cap. `None` when a writer epoch isn't above the last.
    pub fn appended(&self, at: u64, spell: Spell) -> Option<Self> {
        let at = self.entries.last().map_or(at, |last| at.max(last.at));
        let mut next = self.clone();
        next.entries.push(Entry { at, spell, extra: Default::default() });
        if next.entries.len() > MAX_LEDGER {
            next.entries.remove(0);
        }
        next.well_formed().then_some(next)
    }
}

/// [`Ledger::admits`] for a Channel that may have no ledger, where only the read address serves.
pub fn admits(ledger: Option<&Ledger>, wepoch: Option<Epoch>, event: &super::chat::ChatEvent) -> bool {
    let o = event.opened();
    match ledger {
        Some(l) => l.admits(wepoch, o.rumor.kind.as_u16(), o.at_ms),
        None => wepoch.is_none(),
    }
}

/// The writer address a secret yields for `epoch`.
pub fn writer_address(secret: &[u8; 32], channel: &ChannelId, epoch: Epoch) -> [u8; 32] {
    channel_writer_group_key(secret, channel, epoch).pk().to_bytes()
}

/// The rekey scope writer blobs bind to.
pub fn writer_rekey_scope(channel: &ChannelId) -> super::rekey::RekeyScope {
    super::rekey::RekeyScope::Channel(ChannelId(writer_scope(channel)))
}

/// Where a Channel's writer secrets are archived, by writer epoch.
pub fn secret_scope(channel: &ChannelId) -> String {
    format!("wr:{}", crate::simd::hex::bytes_to_hex_32(&channel.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CH: ChannelId = ChannelId([0x42; 32]);

    fn pk(n: u8) -> [u8; 32] {
        writer_address(&[n; 32], &CH, Epoch(n as u64))
    }

    fn ledger(entries: &[(u64, Spell)]) -> Ledger {
        let mut l = Ledger::new(&CH);
        l.entries = entries.iter().map(|(at, spell)| Entry { at: *at, spell: *spell, extra: Default::default() }).collect();
        l
    }

    #[test]
    fn a_ledger_round_trips_with_its_unknown_fields() {
        let mut l = ledger(&[(100, Spell::Writer { epoch: Epoch(1), pk: pk(1), root: Epoch(0) }), (200, Spell::Open), (300, Spell::Writer { epoch: Epoch(2), pk: pk(2), root: Epoch(3) })]);
        l.extra.insert("future".into(), serde_json::json!({"x": 1}));
        l.entries[1].extra.insert("note".into(), "kept".into());
        let back = Ledger::parse(&l.to_json()).unwrap();
        assert_eq!(back, l);
        assert!(back.restricted() && back.current_writer() == Some((Epoch(2), pk(2))) && back.next_epoch() == Epoch(3));
        assert_eq!(back.live_root(), Some(Epoch(3)));
    }

    #[test]
    fn a_malformed_ledger_reads_as_none() {
        let w = |e: u64, n: u8| Spell::Writer { epoch: Epoch(e), pk: pk(n), root: Epoch(0) };
        let ok = ledger(&[(100, w(1, 1)), (100, Spell::Open), (150, w(2, 2))]);
        assert!(Ledger::parse(&ok.to_json()).is_some(), "equal seconds keep order");
        for (why, bad) in [
            ("out of order", ledger(&[(200, w(1, 1)), (100, Spell::Open)])),
            ("epoch repeats", ledger(&[(100, w(1, 1)), (200, w(1, 2))])),
            ("epoch falls", ledger(&[(100, w(2, 2)), (200, w(1, 1))])),
        ] {
            assert!(Ledger::parse(&bad.to_json()).is_none(), "{why}");
        }
        let full = ledger(&(0..=MAX_LEDGER as u64).map(|i| (i, Spell::Open)).collect::<Vec<_>>());
        assert!(Ledger::parse(&full.to_json()).is_none(), "too long");
        let at_cap = ledger(&(0..MAX_LEDGER as u64).map(|i| (i, Spell::Open)).collect::<Vec<_>>());
        let next = at_cap.appended(500, Spell::Writer { epoch: Epoch(1), pk: pk(1), root: Epoch(0) }).unwrap();
        assert_eq!((next.entries.len(), next.entries[0].at, next.restricted()), (MAX_LEDGER, 1, true), "a full ledger drops its oldest to take a new one");
        let ch = crate::simd::hex::bytes_to_hex_32(&CH.0);
        let hex = crate::simd::hex::bytes_to_hex_32(&pk(1));
        for (why, raw) in [
            ("epoch zero", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"epoch":0,"writer":"{hex}","root":0}}]}}"#)),
            ("both at once", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"open":true,"epoch":1,"writer":"{hex}","root":0}}]}}"#)),
            ("no root", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"epoch":1,"writer":"{hex}"}}]}}"#)),
            ("open false", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"open":false}}]}}"#)),
            ("no point", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"epoch":1,"writer":"{}","root":0}}]}}"#, "00".repeat(32))),
            ("no channel", r#"{"ledger":[]}"#.to_string()),
        ] {
            assert!(Ledger::parse(&raw).is_none(), "{why}");
        }
    }

    #[test]
    fn each_entry_holds_until_the_next() {
        let l = ledger(&[(100_000, Spell::Writer { epoch: Epoch(1), pk: pk(1), root: Epoch(0) }), (200_000, Spell::Open), (300_000, Spell::Writer { epoch: Epoch(2), pk: pk(2), root: Epoch(0) })]);
        assert!(l.read_address_takes(99_999), "before the first entry the Channel is open");
        assert!(!l.read_address_takes(100_000) && !l.read_address_takes(199_999), "inside a writer entry");
        assert!(l.read_address_takes(200_000) && l.read_address_takes(299_999), "the open stretch");
        assert!(!l.read_address_takes(5_000_000), "the live writer entry");
        assert!(l.writer_address_takes(Epoch(1), 150_000) && l.writer_address_takes(Epoch(1), 1_000), "a writer's past posts stay");
        assert!(!l.writer_address_takes(Epoch(1), 200_000), "nothing after its entry ends");
        assert!(l.writer_address_takes(Epoch(2), u64::MAX), "the live address takes everything");
        assert!(!l.writer_address_takes(Epoch(3), 0), "an epoch the ledger never named");
    }

    #[test]
    fn a_channel_without_a_ledger_reads_only_its_read_address() {
        use nostr_sdk::prelude::{Keys, Timestamp};
        let k = Keys::generate();
        let g = crate::community::v2::derive::channel_group_key(&[1; 32], &CH, Epoch(0));
        let rumor = super::super::chat::build_message_rumor(k.public_key(), &CH, Epoch(0), "x", None, &[], vec![], 1_000);
        let (wrap, _) = super::super::chat::seal_chat_rumor(&rumor, &g, &k, Timestamp::from_secs(1), false).unwrap();
        let ev = super::super::chat::open_chat_event(&wrap, &g, &CH, Epoch(0)).unwrap();
        assert!(admits(None, None, &ev));
        assert!(!admits(None, Some(Epoch(1)), &ev), "no ledger names a writer address");
    }

    #[test]
    fn appending_never_backdates_and_keeps_epochs_rising() {
        let l = ledger(&[(100, Spell::Writer { epoch: Epoch(1), pk: pk(1), root: Epoch(0) })]);
        let next = l.appended(50, Spell::Open).unwrap();
        assert_eq!(next.entries[1].at, 100, "clamped to the last entry");
        assert!(l.appended(200, Spell::Writer { epoch: Epoch(1), pk: pk(2), root: Epoch(0) }).is_none());
        assert_eq!(next.next_epoch(), Epoch(2));
    }
}
