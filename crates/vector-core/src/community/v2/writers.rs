//! A restricted Channel's Writers ledger (CORD-03 §1): which address takes posts when.
//!
//! The ledger is a Control entity, so every member folds the same answer and a Refounding
//! carries it. Writer secrets travel separately, on the access plane, and a member adopts one
//! only if it derives to the address the ledger names.

use super::derive::{channel_writer_group_key, writer_scope};
use crate::community::{ChannelId, Epoch};

/// The most entries a ledger holds; a longer one is malformed.
pub const MAX_LEDGER: usize = 64;

/// The most writer entries the cap retires before the oldest goes for good.
pub const MAX_RETIRED: usize = 64;

/// Writer epochs stay below this, so every reader's arithmetic agrees.
pub const MAX_EPOCH: u64 = 1 << 53;

/// How far an edition's new entry may sit from the edition's own time.
pub const ENTRY_SKEW_MS: u64 = 10 * 60 * 1000;

/// How far past a reader's clock a new entry may be dated before it waits (CORD-02 §5).
pub const HORIZON_MS: u64 = 60 * 60 * 1000;

/// What one ledger entry moves the Channel to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spell {
    /// Posts land at this writer epoch's address, minted under root epoch `root` by `by`, whose
    /// writers carry a certificate from `certifier`.
    Writer { epoch: Epoch, pk: [u8; 32], root: Epoch, by: [u8; 32], certifier: Option<[u8; 32]> },
    /// Posts land at the read address again.
    Open,
    /// From here on the live epoch's certificate for `writer` vouches for nothing: a writer the
    /// Channel no longer lets post. Moves no window.
    Stop { writer: [u8; 32] },
    /// An entry of a kind this build doesn't know, carried verbatim: it moves nothing.
    Other,
}

impl Spell {
    /// Whether this entry moves where posts land.
    fn moves(&self) -> bool {
        matches!(self, Spell::Writer { .. } | Spell::Open)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Unix milliseconds this entry starts holding.
    pub at: u64,
    pub spell: Spell,
    /// Unknown fields, carried through our own editions (CORD-02 §6).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The field a stop carries when it records a departure that came before it was written.
const STOP_FROM: &str = "from";

impl Entry {
    /// When this entry takes effect: its own date, or for a stop, the departure it records if
    /// that came first. A reader that predates `from` applies the stop from its own date.
    pub fn effective(&self) -> u64 {
        match self.spell {
            Spell::Stop { .. } => self.extra.get(STOP_FROM).and_then(|v| v.as_u64()).map_or(self.at, |from| from.min(self.at)),
            _ => self.at,
        }
    }
}

/// A writer entry the cap dropped: its address still reads, its window stays closed, and its
/// writers' certificates still check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retired {
    pub epoch: Epoch,
    pub pk: [u8; 32],
    pub from: u64,
    pub to: u64,
    pub certifier: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ledger {
    pub channel: [u8; 32],
    pub entries: Vec<Entry>,
    pub retired: Vec<Retired>,
    /// Unknown top-level fields, carried through our own editions (CORD-02 §6).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn hex32(v: &serde_json::Value) -> Option<[u8; 32]> {
    crate::simd::hex::hex_to_bytes_32_checked(v.as_str()?)
}

fn writer_pk(v: &serde_json::Value) -> Option<[u8; 32]> {
    let pk = hex32(v)?;
    nostr_sdk::prelude::PublicKey::from_slice(&pk).ok()?.xonly().ok()?;
    Some(pk)
}

fn epoch_of(v: &serde_json::Value) -> Option<Epoch> {
    v.as_u64().filter(|n| *n > 0 && *n < MAX_EPOCH).map(Epoch)
}

impl Ledger {
    pub fn new(channel: &ChannelId) -> Self {
        Ledger { channel: channel.0, ..Default::default() }
    }

    /// Parse a ledger, `None` when it is malformed: entries out of `at` order, writer epochs
    /// (retired, then live) not strictly rising or out of range, more than [`MAX_LEDGER`]
    /// entries or [`MAX_RETIRED`] retired, or an entry that is neither one thing.
    pub fn parse(content: &str) -> Option<Self> {
        let serde_json::Value::Object(mut top) = serde_json::from_str(content).ok()? else { return None };
        let channel = hex32(&top.remove("channel")?)?;
        let serde_json::Value::Array(raw) = top.remove("ledger")? else { return None };
        let retired = match top.remove("retired") {
            None => Vec::new(),
            Some(serde_json::Value::Array(r)) => r
                .iter()
                .map(|x| {
                    let x = x.as_array().filter(|x| x.len() == 4 || x.len() == 5)?;
                    let certifier = match x.get(4) {
                        None => None,
                        Some(c) => Some(writer_pk(c)?),
                    };
                    Some(Retired { epoch: epoch_of(&x[0])?, pk: writer_pk(&x[1])?, from: x[2].as_u64()?, to: x[3].as_u64()?, certifier })
                })
                .collect::<Option<Vec<_>>>()?,
            Some(_) => return None,
        };
        if raw.len() > MAX_LEDGER || retired.len() > MAX_RETIRED {
            return None;
        }
        let mut entries = Vec::with_capacity(raw.len());
        for e in raw {
            let serde_json::Value::Object(mut e) = e else { return None };
            let at = e.remove("at")?.as_u64()?;
            let stop = e.remove("stop");
            let spell = match (e.remove("open"), e.remove("epoch"), e.remove("writer"), e.remove("root"), e.remove("by")) {
                (None, None, None, None, None) if stop.is_some() => Spell::Stop { writer: writer_pk(stop.as_ref()?)? },
                (None, None, None, None, None) => Spell::Other,
                (Some(serde_json::Value::Bool(true)), None, None, None, None) if stop.is_none() => Spell::Open,
                (None, Some(epoch), Some(writer), Some(root), Some(by)) if stop.is_none() => {
                    let certifier = match e.remove("certifier") {
                        None => None,
                        Some(c) => Some(writer_pk(&c)?),
                    };
                    Spell::Writer { epoch: epoch_of(&epoch)?, pk: writer_pk(&writer)?, root: Epoch(root.as_u64()?), by: hex32(&by)?, certifier }
                }
                _ => return None,
            };
            entries.push(Entry { at, spell, extra: e });
        }
        let ledger = Ledger { channel, entries, retired, extra: top };
        ledger.well_formed().then_some(ledger)
    }

    fn well_formed(&self) -> bool {
        let ordered = self.entries.windows(2).all(|w| w[0].at <= w[1].at);
        // A stop is a writer window's; an open one has nobody to stop.
        let stops = self.entries.iter().enumerate().all(|(i, e)| {
            !matches!(e.spell, Spell::Stop { .. }) || !matches!(self.entries[..i].iter().rev().find(|e| e.spell.moves()).map(|e| e.spell), Some(Spell::Open))
        });
        let windows = self.retired.iter().all(|r| r.from <= r.to);
        let epochs: Vec<u64> = self.writers().map(|(e, ..)| e.0).collect();
        ordered && stops && windows && epochs.windows(2).all(|w| w[0] < w[1]) && self.entries.len() <= MAX_LEDGER && self.retired.len() <= MAX_RETIRED
    }

    pub fn to_json(&self) -> String {
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut o = e.extra.clone();
                o.insert("at".into(), e.at.into());
                match e.spell {
                    Spell::Writer { epoch, pk, root, by, certifier } => {
                        o.insert("epoch".into(), epoch.0.into());
                        o.insert("writer".into(), crate::simd::hex::bytes_to_hex_32(&pk).into());
                        o.insert("root".into(), root.0.into());
                        o.insert("by".into(), crate::simd::hex::bytes_to_hex_32(&by).into());
                        if let Some(c) = certifier {
                            o.insert("certifier".into(), crate::simd::hex::bytes_to_hex_32(&c).into());
                        }
                    }
                    Spell::Open => {
                        o.insert("open".into(), true.into());
                    }
                    Spell::Stop { writer } => {
                        o.insert("stop".into(), crate::simd::hex::bytes_to_hex_32(&writer).into());
                    }
                    Spell::Other => {}
                }
                serde_json::Value::Object(o)
            })
            .collect();
        let mut top = self.extra.clone();
        top.insert("channel".into(), crate::simd::hex::bytes_to_hex_32(&self.channel).into());
        top.insert("ledger".into(), entries.into());
        if !self.retired.is_empty() {
            let retired: Vec<serde_json::Value> = self
                .retired
                .iter()
                .map(|r| match r.certifier {
                    Some(c) => serde_json::json!([r.epoch.0, crate::simd::hex::bytes_to_hex_32(&r.pk), r.from, r.to, crate::simd::hex::bytes_to_hex_32(&c)]),
                    None => serde_json::json!([r.epoch.0, crate::simd::hex::bytes_to_hex_32(&r.pk), r.from, r.to]),
                })
                .collect();
            top.insert("retired".into(), retired.into());
        }
        serde_json::Value::Object(top).to_string()
    }

    /// The newest entry that moved where posts land.
    fn last_move(&self) -> Option<&Entry> {
        self.entries.iter().rev().find(|e| e.spell.moves())
    }

    /// Whether posts land at a writer address now.
    pub fn restricted(&self) -> bool {
        matches!(self.last_move(), Some(Entry { spell: Spell::Writer { .. }, .. }))
    }

    /// The writer epoch taking posts now.
    pub fn current_writer(&self) -> Option<(Epoch, [u8; 32])> {
        match self.last_move()?.spell {
            Spell::Writer { epoch, pk, .. } => Some((epoch, pk)),
            _ => None,
        }
    }

    /// The live writer entry: its root epoch (a Refounding retires it), its minter, and when it began.
    pub fn live(&self) -> Option<(Epoch, [u8; 32], u64)> {
        let last = self.last_move()?;
        match last.spell {
            Spell::Writer { root, by, .. } => Some((root, by, last.at)),
            _ => None,
        }
    }

    /// Whether `writer` was stopped at writer `epoch` by `at_ms`: a stop entry for them inside
    /// that epoch's window, dated no later. A stop outlives its window's entry retiring; only the
    /// cap dropping the stop itself undoes it.
    pub fn stopped(&self, epoch: Epoch, writer: &[u8; 32], at_ms: u64) -> bool {
        let names = |e: &Entry| e.effective() <= at_ms && matches!(e.spell, Spell::Stop { writer: w } if w == *writer);
        if let Some(start) = self.entries.iter().position(|e| matches!(e.spell, Spell::Writer { epoch: w, .. } if w == epoch)) {
            return self.entries[start + 1..].iter().take_while(|e| !e.spell.moves()).any(names);
        }
        // Retired: the stops left at the front, before the first entry that moves, inside its window.
        // A stop dated with the entry that retired its window was still written inside it.
        self.retired.iter().find(|r| r.epoch == epoch).is_some_and(|r| self.entries.iter().take_while(|e| !e.spell.moves()).any(|e| (r.from..=r.to).contains(&e.at) && names(e)))
    }

    /// Who minted writer `epoch`, while the ledger still holds its entry.
    pub fn minter(&self, epoch: Epoch) -> Option<[u8; 32]> {
        self.entries.iter().find_map(|e| match e.spell {
            Spell::Writer { epoch: w, by, .. } if w == epoch => Some(by),
            _ => None,
        })
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

    /// Every writer address, retired then live, with the millisecond its entry ends, `None` while
    /// it still holds.
    pub fn writers(&self) -> impl Iterator<Item = (Epoch, [u8; 32], Option<u64>)> + '_ {
        let retired = self.retired.iter().map(|r| (r.epoch, r.pk, Some(r.to)));
        let live = self.entries.iter().enumerate().filter_map(|(i, e)| match e.spell {
            Spell::Writer { epoch, pk, .. } => Some((epoch, pk, self.entries[i + 1..].iter().find(|n| n.spell.moves()).map(|n| n.at))),
            _ => None,
        });
        retired.chain(live)
    }

    /// When each writer address began, to bound what a read of it can hold.
    pub fn writer_start(&self, epoch: Epoch) -> Option<u64> {
        self.retired
            .iter()
            .find(|r| r.epoch == epoch)
            .map(|r| r.from)
            .or_else(|| self.entries.iter().find(|e| matches!(e.spell, Spell::Writer { epoch: w, .. } if w == epoch)).map(|e| e.at))
    }

    /// Whether a post at the read address dated `at_ms` stands: not inside a writer entry, live
    /// or retired.
    pub fn read_address_takes(&self, at_ms: u64) -> bool {
        if self.retired.iter().any(|r| (r.from..r.to).contains(&at_ms)) {
            return false;
        }
        !matches!(self.entries.iter().rev().find(|e| e.at <= at_ms && e.spell.moves()), Some(Entry { spell: Spell::Writer { .. }, .. }))
    }

    /// The key that certifies writer `epoch`'s writers, live or retired.
    pub fn certifier(&self, epoch: Epoch) -> Option<[u8; 32]> {
        self.retired.iter().find(|r| r.epoch == epoch).and_then(|r| r.certifier).or_else(|| {
            self.entries.iter().find_map(|e| match e.spell {
                Spell::Writer { epoch: w, certifier, .. } if w == epoch => certifier,
                _ => None,
            })
        })
    }

    /// Whether `cert` is the certifier of writer `epoch` vouching for `writer` in this Channel.
    pub fn certifies(&self, community: &[u8; 32], epoch: Epoch, writer: &[u8; 32], cert: &[u8; 64]) -> bool {
        self.certifier(epoch).is_some_and(|c| verify_writer_cert(&c, &cert_digest(community, &self.channel, epoch, writer), cert))
    }

    /// Whether a post at writer `epoch`'s address dated `at_ms` stands: before its entry ends.
    pub fn writer_address_takes(&self, epoch: Epoch, at_ms: u64) -> bool {
        self.writers().any(|(e, _, end)| e == epoch && end.is_none_or(|end| at_ms < end))
    }

    /// This ledger with `entry` appended verbatim; at the cap the oldest entry goes, a writer one
    /// into `retired`. `None` when that leaves it malformed.
    fn with(&self, entry: Entry) -> Option<Self> {
        let mut next = self.clone();
        next.entries.push(entry);
        if next.entries.len() > MAX_LEDGER {
            let gone = next.entries.remove(0);
            if let Spell::Writer { epoch, pk, certifier, .. } = gone.spell {
                // Its window runs to the next entry that moves posts; the live one never retires.
                let to = next.entries.iter().find(|e| e.spell.moves())?.at;
                next.retired.push(Retired { epoch, pk, from: gone.at, to, certifier });
                if next.retired.len() > MAX_RETIRED {
                    next.retired.remove(0);
                }
            }
        }
        next.well_formed().then_some(next)
    }

    /// This ledger with `spell` appended at `at`, never before the last entry. `None` when a
    /// writer epoch isn't above the last.
    pub fn appended(&self, at: u64, spell: Spell) -> Option<Self> {
        self.appended_from(at, spell, None)
    }

    /// [`Self::appended`]; a stop dated `at` that records a departure from `from`.
    pub fn appended_from(&self, at: u64, spell: Spell, from: Option<u64>) -> Option<Self> {
        let at = self.entries.last().map_or(at, |last| at.max(last.at));
        let mut extra = serde_json::Map::new();
        if let (Spell::Stop { .. }, Some(from)) = (spell, from.filter(|f| *f < at)) {
            extra.insert(STOP_FROM.into(), from.into());
        }
        self.with(Entry { at, spell, extra })
    }

    /// Whether this ledger is `prev` with exactly one entry appended.
    pub fn extends(&self, prev: &Ledger) -> bool {
        let Some(last) = self.entries.last() else { return false };
        prev.with(last.clone()).is_some_and(|grown| grown.channel == self.channel && grown.entries == self.entries && grown.retired == self.retired)
    }

    /// Whether this ledger could be a Channel's first: one entry and nothing retired.
    pub fn is_first(&self) -> bool {
        self.entries.len() == 1 && self.retired.is_empty()
    }

    /// Whether this ledger's newest entry sits within [`ENTRY_SKEW_MS`] of `edition_secs`, and,
    /// when it is a writer entry, names `author` as its minter: an edition appends its own.
    pub fn tail_fits(&self, edition_secs: u64, author: &[u8; 32]) -> bool {
        let Some(last) = self.entries.last() else { return false };
        let near = last.at.abs_diff(edition_secs.saturating_mul(1000)) <= ENTRY_SKEW_MS;
        near && !matches!(last.spell, Spell::Writer { by, .. } if by != *author)
    }

    /// Whether the newest entry is dated within [`HORIZON_MS`] of `now_ms`: a later one waits.
    pub fn not_ahead(&self, now_ms: u64) -> bool {
        self.entries.last().is_some_and(|e| e.at <= now_ms.saturating_add(HORIZON_MS))
    }
}

/// What a writer certificate signs: one member's place among a Channel's writers at one epoch.
pub fn cert_digest(community: &[u8; 32], channel: &[u8; 32], epoch: Epoch, writer: &[u8; 32]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(b"concord/writer-cert");
    h.update(community);
    h.update(channel);
    h.update(epoch.0.to_be_bytes());
    h.update(writer);
    h.finalize().into()
}

/// The never-published event a writer certificate signs: [`cert_digest`] under the certifier.
fn cert_event(certifier: nostr_sdk::prelude::PublicKey, digest: &[u8; 32]) -> nostr_sdk::prelude::UnsignedEvent {
    use nostr_sdk::prelude::{Kind, Tag, Timestamp, UnsignedEvent};
    UnsignedEvent::new(certifier, Timestamp::from_secs(0), Kind::Custom(CERT_KIND), [Tag::custom("digest", [crate::simd::hex::bytes_to_hex_32(digest)])], "")
}

/// The kind of the event a writer certificate signs; never published.
const CERT_KIND: u16 = 3320;

/// A certifier's signature over [`cert_digest`].
pub fn sign_writer_cert(certifier: &[u8; 32], digest: &[u8; 32]) -> Option<[u8; 64]> {
    let keys = nostr_sdk::prelude::Keys::new(nostr_sdk::prelude::SecretKey::from_slice(certifier).ok()?);
    let e = cert_event(keys.public_key(), digest);
    let id = nostr_sdk::prelude::EventId::compute(&e.pubkey, &e.created_at, &e.kind, &e.tags, &e.content);
    Some(keys.sign_schnorr(id.as_bytes()).to_bytes())
}

pub fn verify_writer_cert(certifier: &[u8; 32], digest: &[u8; 32], cert: &[u8; 64]) -> bool {
    use nostr_sdk::prelude::{Event, EventId, PublicKey, Signature};
    let (Ok(pk), Ok(sig)) = (PublicKey::from_slice(certifier), Signature::from_slice(cert)) else { return false };
    let unsigned = cert_event(pk, digest);
    let id = EventId::compute(&pk, &unsigned.created_at, &unsigned.kind, &unsigned.tags, &unsigned.content);
    Event::new(id, pk, unsigned.created_at, unsigned.kind, unsigned.tags, unsigned.content, sig).verify_signature()
}

/// The certifier key a certifier secret yields.
pub fn certifier_of(secret: &[u8; 32]) -> Option<[u8; 32]> {
    nostr_sdk::prelude::SecretKey::from_slice(secret).ok().map(|s| nostr_sdk::prelude::Keys::new(s).public_key().to_bytes())
}

/// The writer address a secret yields for `epoch`.
pub fn writer_address(secret: &[u8; 32], channel: &ChannelId, epoch: Epoch) -> [u8; 32] {
    channel_writer_group_key(secret, channel, epoch).pk().to_bytes()
}

/// The rekey scope writer blobs bind to.
pub fn writer_rekey_scope(channel: &ChannelId) -> super::rekey::RekeyScope {
    super::rekey::RekeyScope::Channel(ChannelId(writer_scope(channel)))
}

/// The access-plane scope a Channel's writer certifier secrets ride to its managers.
pub fn certifier_rekey_scope(channel: &ChannelId) -> super::rekey::RekeyScope {
    super::rekey::RekeyScope::Channel(ChannelId(super::derive::certifier_scope(channel)))
}

/// Where a Channel's writer secrets are archived, by writer epoch.
pub fn secret_scope(channel: &ChannelId) -> String {
    format!("wr:{}", crate::simd::hex::bytes_to_hex_32(&channel.0))
}

/// Where a minter keeps the secrets it minted, so a retry reuses its own and never one it
/// was delivered.
pub fn minted_scope(channel: &ChannelId) -> String {
    format!("wrmint:{}", crate::simd::hex::bytes_to_hex_32(&channel.0))
}

/// Where a minter keeps the secret that certifies its epoch's writers.
pub fn certifier_scope(channel: &ChannelId) -> String {
    format!("wrcert:{}", crate::simd::hex::bytes_to_hex_32(&channel.0))
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
        Spell::Writer { epoch: Epoch(e), pk: pk(n), root: Epoch(0), by: BY, certifier: None }
    }

    /// The certificate test vector CORD-03 §1 publishes: what every client must compute alike.
    #[test]
    fn a_writer_certificate_matches_the_published_vector() {
        let (community, channel, writer, secret) = ([0x01; 32], [0x02; 32], [0x04; 32], [0x05; 32]);
        let d = cert_digest(&community, &channel, Epoch(3), &writer);
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&d), "f49b5c7359b09366b5b96e074d2b7c5a84bd714a04bf0e6270d13c02d453e619");
        let certifier = certifier_of(&secret).unwrap();
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&certifier), "62c0a046dacce86ddd0343c6d3c7c79c2208ba0d9c9cf24a6d046d21d21f90f7");
        let e = cert_event(nostr_sdk::prelude::PublicKey::from_slice(&certifier).unwrap(), &d);
        let id = nostr_sdk::prelude::EventId::compute(&e.pubkey, &e.created_at, &e.kind, &e.tags, &e.content);
        assert_eq!(id.to_hex(), "1b6fcc32de4a485d7b0109b0e5dd91b7c676fcb26f7dc41c8c2bdc7e3f2cbe90");
        let published: [u8; 64] = crate::simd::hex::hex_string_to_bytes_checked("cd1c3030e1220c539f1efb50d32e5b83432baa20eaa8709aa935050dc11a6ac6e25c3f9555b729cee65829fdbf1ba5e7656865a4fca860e42b05c8341922895a").unwrap().try_into().unwrap();
        assert!(verify_writer_cert(&certifier, &d, &published));
        assert!(verify_writer_cert(&certifier, &d, &sign_writer_cert(&secret, &d).unwrap()));
    }

    #[test]
    fn a_writer_certificate_vouches_for_one_member_at_one_epoch_in_one_channel() {
        let secret = [0x31; 32];
        let certifier = certifier_of(&secret).unwrap();
        let (community, channel, writer) = ([1; 32], [2; 32], pk(5));
        let cert = sign_writer_cert(&secret, &cert_digest(&community, &channel, Epoch(3), &writer)).unwrap();
        assert!(verify_writer_cert(&certifier, &cert_digest(&community, &channel, Epoch(3), &writer), &cert));
        for (why, digest) in [
            ("another member", cert_digest(&community, &channel, Epoch(3), &pk(6))),
            ("another epoch", cert_digest(&community, &channel, Epoch(4), &writer)),
            ("another channel", cert_digest(&community, &[9; 32], Epoch(3), &writer)),
            ("another community", cert_digest(&[8; 32], &channel, Epoch(3), &writer)),
        ] {
            assert!(!verify_writer_cert(&certifier, &digest, &cert), "{why}");
        }
        assert!(!verify_writer_cert(&certifier_of(&[0x32; 32]).unwrap(), &cert_digest(&community, &channel, Epoch(3), &writer), &cert), "another certifier");
    }

    #[test]
    fn a_full_ledger_fits_one_relay_event() {
        let c = certifier_of(&[0x41; 32]).unwrap();
        let mut l = Ledger { channel: [7; 32], ..Default::default() };
        let mut epoch = 0u64;
        while l.retired.len() < MAX_RETIRED || l.entries.len() < MAX_LEDGER {
            epoch += 1;
            let spell = Spell::Writer { epoch: Epoch(epoch), pk: pk((epoch % 200) as u8 + 1), root: Epoch(epoch), by: BY, certifier: Some(c) };
            l = l.appended(1_792_256_141_843 + epoch * 1_000, spell).unwrap();
        }
        let keys = nostr_sdk::prelude::Keys::generate();
        let group = super::super::derive::channel_group_key(&[1; 32], &ChannelId([7; 32]), Epoch(1));
        let rumor = super::super::control::build_edition_rumor_bound(keys.public_key(), super::super::vsk::WRITERS, &[8; 32], 999, Some(&[9; 32]), &l.to_json(), 1_792_256_141, None, Some([10; 32]));
        let (wrap, _) = super::super::control::seal_control_edition(&rumor, &group, &keys, nostr_sdk::prelude::Timestamp::from_secs(1_792_256_141)).unwrap();
        assert!(wrap.as_json().len() <= 65_536, "{} bytes: a full ledger must fit a 64KB relay event", wrap.as_json().len());
    }

    #[test]
    fn a_ledger_carries_its_certifiers_live_and_retired() {
        let c = certifier_of(&[0x41; 32]).unwrap();
        let mut l = Ledger { channel: [7; 32], ..Default::default() };
        for i in 0..MAX_LEDGER as u64 + 1 {
            let spell = Spell::Writer { epoch: Epoch(i + 1), pk: pk((i % 200) as u8 + 1), root: Epoch(0), by: BY, certifier: Some(c) };
            l = l.appended(1_000 + i, spell).unwrap();
        }
        assert_eq!(l.retired.len(), 1);
        assert_eq!((l.certifier(Epoch(1)), l.certifier(Epoch(2))), (Some(c), Some(c)), "retired and live");
        let back = Ledger::parse(&l.to_json()).unwrap();
        assert_eq!(back, l, "a round trip keeps every certifier");
        let old = ledger(&[(100, w(1, 1))]);
        assert_eq!(Ledger::parse(&old.to_json()).unwrap().certifier(Epoch(1)), None, "an entry from before certifiers reads");
    }

    #[test]
    fn a_stop_voids_one_writer_inside_its_epoch_and_moves_no_window() {
        let (a, b) = (pk(30), pk(31));
        let l = ledger(&[(100, w(1, 1)), (200, Spell::Stop { writer: a }), (300, w(2, 2)), (400, Spell::Open)]);
        assert!(l.well_formed());
        assert_eq!(Ledger::parse(&l.to_json()).unwrap(), l, "round trips");
        let ends: Vec<Option<u64>> = l.writers().map(|(.., end)| end).collect();
        assert_eq!(ends, vec![Some(300), Some(400)], "a window runs to the next entry that moves posts");
        assert!(!l.read_address_takes(250), "a stop opens nothing");
        assert!(l.stopped(Epoch(1), &a, 200) && l.stopped(Epoch(1), &a, 299));
        assert!(!l.stopped(Epoch(1), &a, 199), "posts before it stand");
        assert!(!l.stopped(Epoch(1), &b, 250), "it names one writer");
        assert!(!l.stopped(Epoch(2), &a, 350), "and one epoch");
        let later = ledger(&[(100, w(1, 1)), (200, w(2, 2)), (300, Spell::Stop { writer: a })]);
        assert!(!later.stopped(Epoch(1), &a, 400) && later.stopped(Epoch(2), &a, 400), "a stop is the window's it sits in");
        let live = ledger(&[(100, w(1, 1)), (200, Spell::Stop { writer: a })]);
        assert!(live.restricted() && live.current_writer() == Some((Epoch(1), pk(1))) && live.live().map(|l| l.2) == Some(100));
        assert!(!ledger(&[(100, w(1, 1)), (150, Spell::Open), (200, Spell::Stop { writer: a })]).well_formed(), "an open window has nobody to stop");
        assert!(ledger(&[(200, Spell::Stop { writer: a }), (300, w(2, 2))]).well_formed(), "one whose epoch retired still parses");
        let mut both: serde_json::Value = serde_json::from_str(&live.to_json()).unwrap();
        both["ledger"][1]["open"] = true.into();
        assert!(Ledger::parse(&both.to_string()).is_none(), "an entry is one thing");
        let mut unknown: serde_json::Value = serde_json::from_str(&live.to_json()).unwrap();
        unknown["ledger"].as_array_mut().unwrap().push(serde_json::json!({ "at": 300, "future": "kind" }));
        let carried = Ledger::parse(&unknown.to_string()).expect("a kind this build doesn't know is carried");
        assert_eq!(carried.current_writer(), live.current_writer(), "and moves nothing");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&carried.to_json()).unwrap()["ledger"][2], serde_json::json!({ "at": 300, "future": "kind" }), "verbatim");
    }

    #[test]
    fn a_stop_never_pushes_the_live_writer_out_and_a_retired_window_skips_stops() {
        let mut l = ledger(&[(1_000, w(1, 1))]);
        for i in 1..MAX_LEDGER as u64 {
            l = l.appended(1_000 + i, Spell::Stop { writer: pk((i % 200) as u8 + 20) }).unwrap();
        }
        assert_eq!(l.entries.len(), MAX_LEDGER);
        assert!(l.appended(5_000, Spell::Stop { writer: pk(9) }).is_none(), "at the cap a stop would retire the live entry");
        let next = l.appended(5_000, w(2, 2)).unwrap();
        assert_eq!(next.retired, vec![Retired { epoch: Epoch(1), pk: pk(1), from: 1_000, to: 5_000, certifier: None }], "its window ends where the next epoch begins");
        assert!(next.stopped(Epoch(1), &pk(21), 4_000), "a stop outlives its window's entry retiring");
        assert!(!next.stopped(Epoch(1), &pk(21), 1_000), "and still only from its date");
        let mut later = next.clone();
        for i in 0..MAX_LEDGER as u64 {
            later = later.appended(6_000 + i, Spell::Open).and_then(|l| l.appended(6_000 + i, w(3 + i, 3))).unwrap_or(later);
        }
        assert!(!later.stopped(Epoch(1), &pk(21), 4_000), "a stop the cap dropped stops nothing");
    }

    fn ledger(entries: &[(u64, Spell)]) -> Ledger {
        let mut l = Ledger::new(&CH);
        l.entries = entries.iter().map(|(at, spell)| Entry { at: *at, spell: *spell, extra: Default::default() }).collect();
        l
    }

    #[test]
    fn a_ledger_round_trips_with_its_unknown_fields() {
        let mut l = ledger(&[(100, w(2, 1)), (200, Spell::Open), (300, Spell::Writer { epoch: Epoch(3), pk: pk(2), root: Epoch(3), by: BY, certifier: None })]);
        l.extra.insert("future".into(), serde_json::json!({"x": 1}));
        l.entries[1].extra.insert("note".into(), "kept".into());
        l.retired = vec![Retired { epoch: Epoch(1), pk: pk(3), from: 10, to: 50, certifier: None }];
        let back = Ledger::parse(&l.to_json()).unwrap();
        assert_eq!(back, l);
        assert!(back.restricted() && back.current_writer() == Some((Epoch(3), pk(2))) && back.next_epoch() == Some(Epoch(4)));
        assert_eq!(back.live(), Some((Epoch(3), BY, 300)));
        assert_eq!((back.previous_writer(), back.minter(Epoch(2)), back.minter(Epoch(1))), (Some(Epoch(2)), Some(BY), None));
    }

    #[test]
    fn a_malformed_ledger_reads_as_none() {
        let ok = ledger(&[(100, w(1, 1)), (100, Spell::Open), (150, w(2, 2))]);
        assert!(Ledger::parse(&ok.to_json()).is_some(), "equal times keep order");
        let mut stale = ok.clone();
        stale.retired = vec![Retired { epoch: Epoch(5), pk: pk(5), from: 1, to: 2, certifier: None }];
        for (why, bad) in [
            ("out of order", ledger(&[(200, w(1, 1)), (100, Spell::Open)])),
            ("epoch repeats", ledger(&[(100, w(1, 1)), (200, w(1, 2))])),
            ("epoch falls", ledger(&[(100, w(2, 2)), (200, w(1, 1))])),
            ("epoch out of range", ledger(&[(100, w(MAX_EPOCH, 1))])),
            ("a retired epoch above a live one", stale),
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
            ("a retired window backwards", format!(r#"{{"channel":"{ch}","ledger":[],"retired":[[1,"{hex}",9,3]]}}"#)),
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
    fn the_cap_retires_writer_entries_and_forgets_open_ones() {
        // A writer entry, then an open stretch, then entries to the cap.
        let mut entries: Vec<(u64, Spell)> = vec![(1_000, w(1, 1)), (2_000, Spell::Open)];
        entries.extend((2..MAX_LEDGER as u64).map(|i| (1_000 + i * 1_000, Spell::Open)));
        let at_cap = ledger(&entries);
        let once = at_cap.appended(500_000, w(2, 2)).unwrap();
        assert_eq!(once.retired, vec![Retired { epoch: Epoch(1), pk: pk(1), from: 1_000, to: 2_000, certifier: None }]);
        assert!(!once.read_address_takes(1_500), "the dropped writer entry's window stays closed");
        assert!(once.writer_address_takes(Epoch(1), 1_500) && !once.writer_address_takes(Epoch(1), 2_000), "and its address still reads, up to its end");
        assert!(once.writers().any(|(e, ..)| e == Epoch(1)), "history still fetches it");
        assert_eq!(once.writer_start(Epoch(1)), Some(1_000));
        let twice = once.appended(600_000, Spell::Open).unwrap();
        assert_eq!(twice.retired.len(), 1, "a dropped open entry leaves nothing");
        assert!(twice.read_address_takes(2_500), "and its open stretch stays open");
        assert!(once.extends(&at_cap) && twice.extends(&once), "a cap drop still carries the ledger forward");
        assert!(Ledger::parse(&twice.to_json()).is_some());
    }

    #[test]
    fn a_ledger_extends_only_by_one_entry() {
        let a = ledger(&[(100, w(1, 1))]);
        let b = a.appended(200, Spell::Open).unwrap();
        assert!(b.extends(&a) && !a.extends(&b) && !a.extends(&a), "exactly one new entry");
        let c = b.appended(300, w(2, 2)).unwrap();
        assert!(!c.extends(&a), "two at once is no extension");
        let rewritten = ledger(&[(100, w(1, 2)), (200, Spell::Open)]);
        assert!(!rewritten.extends(&a), "a carried entry can't change");
        let shed = ledger(&[(200, Spell::Open)]);
        assert!(!shed.extends(&a), "below the cap nothing is dropped");
        assert!(a.is_first() && !b.is_first());
    }

    #[test]
    fn an_editions_newest_entry_is_its_own_and_dated_with_it() {
        let l = ledger(&[(100_000, w(1, 1))]);
        let far = ledger(&[(HORIZON_MS + 10_000_000, w(1, 1))]);
        assert!(far.not_ahead(10_000_000) && !far.not_ahead(10_000_000 - 1), "a far-future entry waits");
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
        use crate::community::v2::service::{sort_arrival, Arrival};
        assert!(matches!(sort_arrival(None, ev.clone(), None), Arrival::Stands(_)));
        assert!(matches!(sort_arrival(None, ev, Some(Epoch(1))), Arrival::Held(None)), "no ledger names a writer address");
    }
}
