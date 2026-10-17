//! A restricted Channel's Writers ledger (CORD-03 §1): which address takes posts when.
//!
//! The ledger is a Control entity, so every member folds the same answer and a Refounding
//! carries it. Writer secrets travel separately, on the access plane, and a member adopts one
//! only if it derives to the address the ledger names.

use super::derive::{channel_writer_group_key, writer_scope};
use crate::community::{ChannelId, Epoch};

/// The most entries a ledger holds; a longer one is malformed.
pub const MAX_LEDGER: usize = 128;

/// Writer epochs stay below this, so every reader's arithmetic agrees.
pub const MAX_EPOCH: u64 = 1 << 53;

/// How far a ledger's newest entry may sit from its edition's own time.
pub const ENTRY_SKEW_MS: u64 = 10 * 60 * 1000;

/// What one ledger entry moves the Channel to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spell {
    /// Posts land at this writer epoch's address, minted under root epoch `root` by `by`.
    Writer { epoch: Epoch, pk: [u8; 32], root: Epoch, by: [u8; 32] },
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
    /// What the cap dropped: the read address takes nothing dated in `[from, to)`.
    pub pruned: Option<(u64, u64)>,
    /// Unknown top-level fields, carried through our own editions (CORD-02 §6).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn hex32(v: &serde_json::Value) -> Option<[u8; 32]> {
    crate::simd::hex::hex_to_bytes_32_checked(v.as_str()?)
}

impl Ledger {
    pub fn new(channel: &ChannelId) -> Self {
        Ledger { channel: channel.0, ..Default::default() }
    }

    /// Parse a ledger, `None` when it is malformed: entries out of `at` order, writer epochs
    /// not strictly rising or out of range, more than [`MAX_LEDGER`], an entry that is neither
    /// one thing, or a pruned range that doesn't end at the first entry.
    pub fn parse(content: &str) -> Option<Self> {
        let serde_json::Value::Object(mut top) = serde_json::from_str(content).ok()? else { return None };
        let channel = hex32(&top.remove("channel")?)?;
        let serde_json::Value::Array(raw) = top.remove("ledger")? else { return None };
        if raw.len() > MAX_LEDGER {
            return None;
        }
        let pruned = match top.remove("pruned") {
            None => None,
            Some(serde_json::Value::Array(r)) if r.len() == 2 => Some((r[0].as_u64()?, r[1].as_u64()?)),
            Some(_) => return None,
        };
        let mut entries = Vec::with_capacity(raw.len());
        for e in raw {
            let serde_json::Value::Object(mut e) = e else { return None };
            let at = e.remove("at")?.as_u64()?;
            let spell = match (e.remove("open"), e.remove("epoch"), e.remove("writer"), e.remove("root"), e.remove("by")) {
                (Some(serde_json::Value::Bool(true)), None, None, None, None) => Spell::Open,
                (None, Some(epoch), Some(writer), Some(root), Some(by)) => {
                    let pk = hex32(&writer)?;
                    nostr_sdk::prelude::PublicKey::from_slice(&pk).ok()?.xonly().ok()?;
                    let epoch = epoch.as_u64().filter(|n| *n > 0 && *n < MAX_EPOCH)?;
                    Spell::Writer { epoch: Epoch(epoch), pk, root: Epoch(root.as_u64()?), by: hex32(&by)? }
                }
                _ => return None,
            };
            entries.push(Entry { at, spell, extra: e });
        }
        let ledger = Ledger { channel, entries, pruned, extra: top };
        ledger.well_formed().then_some(ledger)
    }

    fn well_formed(&self) -> bool {
        let ordered = self.entries.windows(2).all(|w| w[0].at <= w[1].at);
        let epochs: Vec<u64> = self.writers().map(|(e, ..)| e.0).collect();
        let pruned = self.pruned.is_none_or(|(from, to)| from <= to && self.entries.first().is_some_and(|f| f.at == to));
        ordered && pruned && epochs.windows(2).all(|w| w[0] < w[1]) && self.entries.len() <= MAX_LEDGER
    }

    pub fn to_json(&self) -> String {
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut o = e.extra.clone();
                o.insert("at".into(), e.at.into());
                match e.spell {
                    Spell::Writer { epoch, pk, root, by } => {
                        o.insert("epoch".into(), epoch.0.into());
                        o.insert("writer".into(), crate::simd::hex::bytes_to_hex_32(&pk).into());
                        o.insert("root".into(), root.0.into());
                        o.insert("by".into(), crate::simd::hex::bytes_to_hex_32(&by).into());
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
        if let Some((from, to)) = self.pruned {
            top.insert("pruned".into(), serde_json::json!([from, to]));
        }
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

    /// The live writer entry: its root epoch (a Refounding retires it), its minter, and when it began.
    pub fn live(&self) -> Option<(Epoch, [u8; 32], u64)> {
        let last = self.entries.last()?;
        match last.spell {
            Spell::Writer { root, by, .. } => Some((root, by, last.at)),
            Spell::Open => None,
        }
    }

    /// The writer epoch before the live one, if the ledger still names it.
    pub fn previous_writer(&self) -> Option<Epoch> {
        let mut epochs = self.writers().map(|(e, ..)| e).collect::<Vec<_>>();
        epochs.pop();
        epochs.pop()
    }

    /// The epoch a newly minted writer secret takes, `None` once the range is spent.
    pub fn next_epoch(&self) -> Option<Epoch> {
        let next = self.writers().map(|(e, ..)| e.0).max().unwrap_or(0).checked_add(1)?;
        (next < MAX_EPOCH).then_some(Epoch(next))
    }

    /// Every writer address with the millisecond its entry ends, `None` while it still holds.
    pub fn writers(&self) -> impl Iterator<Item = (Epoch, [u8; 32], Option<u64>)> + '_ {
        self.entries.iter().enumerate().filter_map(|(i, e)| match e.spell {
            Spell::Writer { epoch, pk, .. } => Some((epoch, pk, self.entries.get(i + 1).map(|n| n.at))),
            Spell::Open => None,
        })
    }

    /// When each writer address began, to bound what a read of it can hold.
    pub fn writer_start(&self, epoch: Epoch) -> Option<u64> {
        self.entries.iter().find(|e| matches!(e.spell, Spell::Writer { epoch: w, .. } if w == epoch)).map(|e| e.at)
    }

    /// Whether a post at the read address dated `at_ms` stands: not inside a writer entry, nor
    /// in what the cap pruned, which stays closed.
    pub fn read_address_takes(&self, at_ms: u64) -> bool {
        if self.pruned.is_some_and(|(from, to)| (from..to).contains(&at_ms)) {
            return false;
        }
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

    /// This ledger with `spell` appended at `at`, never before the last entry; at the cap its
    /// oldest entry goes and the read address stays closed over what it held. `None` when a
    /// writer epoch isn't above the last.
    pub fn appended(&self, at: u64, spell: Spell) -> Option<Self> {
        let at = self.entries.last().map_or(at, |last| at.max(last.at));
        let mut next = self.clone();
        next.entries.push(Entry { at, spell, extra: Default::default() });
        if next.entries.len() > MAX_LEDGER {
            let gone = next.entries.remove(0);
            let from = next.pruned.map_or(gone.at, |(from, _)| from);
            next.pruned = Some((from, next.entries[0].at));
        }
        next.well_formed().then_some(next)
    }

    /// Whether this ledger carries `prev` forward: its entries, less any the cap dropped, then at
    /// least one new one.
    pub fn extends(&self, prev: &Ledger) -> bool {
        if self.channel != prev.channel {
            return false;
        }
        (0..=prev.entries.len()).any(|dropped| {
            let kept = &prev.entries[dropped..];
            let grew = self.entries.len() > kept.len() && self.entries[..kept.len()] == *kept;
            let at_cap = dropped == 0 || self.entries.len() == MAX_LEDGER;
            let range = match dropped {
                0 => self.pruned == prev.pruned,
                _ => self.pruned.map(|(from, _)| from) == Some(prev.pruned.map_or(prev.entries[0].at, |(from, _)| from)),
            };
            grew && at_cap && range
        })
    }

    /// Whether this ledger's newest entry sits within [`ENTRY_SKEW_MS`] of `edition_secs`, and,
    /// when it is a writer entry, names `author` as its minter: an edition appends its own.
    pub fn tail_fits(&self, edition_secs: u64, author: &[u8; 32]) -> bool {
        let Some(last) = self.entries.last() else { return false };
        let near = last.at.abs_diff(edition_secs.saturating_mul(1000)) <= ENTRY_SKEW_MS;
        near && !matches!(last.spell, Spell::Writer { by, .. } if by != *author)
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
    const BY: [u8; 32] = [0x0b; 32];

    fn pk(n: u8) -> [u8; 32] {
        writer_address(&[n; 32], &CH, Epoch(n as u64))
    }

    fn w(e: u64, n: u8) -> Spell {
        Spell::Writer { epoch: Epoch(e), pk: pk(n), root: Epoch(0), by: BY }
    }

    fn ledger(entries: &[(u64, Spell)]) -> Ledger {
        let mut l = Ledger::new(&CH);
        l.entries = entries.iter().map(|(at, spell)| Entry { at: *at, spell: *spell, extra: Default::default() }).collect();
        l
    }

    #[test]
    fn a_ledger_round_trips_with_its_unknown_fields() {
        let mut l = ledger(&[(100, w(1, 1)), (200, Spell::Open), (300, Spell::Writer { epoch: Epoch(2), pk: pk(2), root: Epoch(3), by: BY })]);
        l.extra.insert("future".into(), serde_json::json!({"x": 1}));
        l.entries[1].extra.insert("note".into(), "kept".into());
        l.pruned = Some((50, 100));
        let back = Ledger::parse(&l.to_json()).unwrap();
        assert_eq!(back, l);
        assert!(back.restricted() && back.current_writer() == Some((Epoch(2), pk(2))) && back.next_epoch() == Some(Epoch(3)));
        assert_eq!(back.live(), Some((Epoch(3), BY, 300)));
        assert_eq!(back.previous_writer(), Some(Epoch(1)));
    }

    #[test]
    fn a_malformed_ledger_reads_as_none() {
        let ok = ledger(&[(100, w(1, 1)), (100, Spell::Open), (150, w(2, 2))]);
        assert!(Ledger::parse(&ok.to_json()).is_some(), "equal times keep order");
        let mut stray = ok.clone();
        stray.pruned = Some((10, 90));
        for (why, bad) in [
            ("out of order", ledger(&[(200, w(1, 1)), (100, Spell::Open)])),
            ("epoch repeats", ledger(&[(100, w(1, 1)), (200, w(1, 2))])),
            ("epoch falls", ledger(&[(100, w(2, 2)), (200, w(1, 1))])),
            ("epoch out of range", ledger(&[(100, w(MAX_EPOCH, 1))])),
            ("pruned short of the first entry", stray),
        ] {
            assert!(Ledger::parse(&bad.to_json()).is_none(), "{why}");
        }
        let full = ledger(&(0..=MAX_LEDGER as u64).map(|i| (i, Spell::Open)).collect::<Vec<_>>());
        assert!(Ledger::parse(&full.to_json()).is_none(), "too long");
        let ch = crate::simd::hex::bytes_to_hex_32(&CH.0);
        let hex = crate::simd::hex::bytes_to_hex_32(&pk(1));
        let by = crate::simd::hex::bytes_to_hex_32(&BY);
        for (why, raw) in [
            ("epoch zero", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"epoch":0,"writer":"{hex}","root":0,"by":"{by}"}}]}}"#)),
            ("both at once", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"open":true,"epoch":1,"writer":"{hex}","root":0,"by":"{by}"}}]}}"#)),
            ("no root", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"epoch":1,"writer":"{hex}","by":"{by}"}}]}}"#)),
            ("no minter", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"epoch":1,"writer":"{hex}","root":0}}]}}"#)),
            ("open false", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"open":false}}]}}"#)),
            ("no point", format!(r#"{{"channel":"{ch}","ledger":[{{"at":1,"epoch":1,"writer":"{}","root":0,"by":"{by}"}}]}}"#, "00".repeat(32))),
            ("no channel", r#"{"ledger":[]}"#.to_string()),
        ] {
            assert!(Ledger::parse(&raw).is_none(), "{why}");
        }
    }

    #[test]
    fn each_entry_holds_until_the_next() {
        let l = ledger(&[(100_000, w(1, 1)), (200_000, Spell::Open), (300_000, w(2, 2))]);
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
    fn the_cap_drops_the_oldest_and_keeps_its_window_closed() {
        let mut entries: Vec<(u64, Spell)> = vec![(1_000, w(1, 1))];
        entries.extend((1..MAX_LEDGER as u64).map(|i| (1_000 + i * 1_000, Spell::Open)));
        let at_cap = ledger(&entries);
        assert!(at_cap.read_address_takes(500) && !at_cap.read_address_takes(1_500));
        let next = at_cap.appended(500_000, w(2, 2)).unwrap();
        assert_eq!((next.entries.len(), next.entries[0].at, next.restricted()), (MAX_LEDGER, 2_000, true));
        assert_eq!(next.pruned, Some((1_000, 2_000)));
        assert!(!next.read_address_takes(1_500), "what the dropped writer entry held stays closed");
        assert!(next.read_address_takes(500), "before it the Channel was open");
        assert!(next.extends(&at_cap), "a cap drop still carries the ledger forward");
        let again = next.appended(600_000, Spell::Open).unwrap();
        assert_eq!(again.pruned, Some((1_000, 3_000)), "the closed range only grows");
        assert!(Ledger::parse(&again.to_json()).is_some());
    }

    #[test]
    fn a_ledger_extends_only_what_it_carries() {
        let a = ledger(&[(100, w(1, 1))]);
        let b = a.appended(200, Spell::Open).unwrap();
        assert!(b.extends(&a) && !a.extends(&b) && !a.extends(&a), "append-only, at least one new entry");
        let rewritten = ledger(&[(100, w(1, 2)), (200, Spell::Open)]);
        assert!(!rewritten.extends(&a), "a carried entry can't change");
        let shed = ledger(&[(200, Spell::Open)]);
        assert!(!shed.extends(&a), "below the cap nothing is dropped");
    }

    #[test]
    fn an_editions_newest_entry_is_its_own_and_dated_with_it() {
        let l = ledger(&[(100_000, w(1, 1))]);
        assert!(l.tail_fits(100, &BY));
        assert!(!l.tail_fits(100, &[0x0c; 32]), "a writer entry names the edition's signer");
        assert!(!l.tail_fits(100 + ENTRY_SKEW_MS / 1000 + 1, &BY), "nor sits far from the edition's time");
        let opened = l.appended(150_000, Spell::Open).unwrap();
        assert!(opened.tail_fits(150, &[0x0c; 32]), "an open entry names nobody");
    }

    #[test]
    fn appending_never_backdates_and_keeps_epochs_rising() {
        let l = ledger(&[(100, w(1, 1))]);
        let next = l.appended(50, Spell::Open).unwrap();
        assert_eq!(next.entries[1].at, 100, "clamped to the last entry");
        assert!(l.appended(200, w(1, 2)).is_none());
        assert_eq!(next.next_epoch(), Some(Epoch(2)));
        assert_eq!(ledger(&[(1, w(MAX_EPOCH - 1, 1))]).next_epoch(), None, "the range is spent");
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
}
