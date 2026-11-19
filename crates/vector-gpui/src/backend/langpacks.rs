//! Downloadable languages: the index Vector's key signs, and the packs it pins by hash. Both are
//! fetched on the shared lane, so a download says which language, never whose device, and both
//! are kept per device beside its settings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use nostr_sdk::prelude::{Event, Kind, PublicKey};
use vector_i18n::index::{D_TAG, Entry, Index, KIND};

/// The key this build trusts first. A throwaway test key until the real one is made: see
/// "Publishing" in crates/vector-i18n/TRANSLATING.md.
const SIGNER: &str = "f4ee7a33d3d5335c6c020122901570c269ecf95a6345778a351c4c4473c33c94";
const INDEX_URL: &str = "https://github.com/VectorPrivacy/Vector/releases/download/languages/index.json";
/// Packs by hash, after the Blossom servers.
const MIRROR: &str = "https://github.com/VectorPrivacy/Vector/releases/download/languages";
const MAX_INDEX: usize = 512 << 10;
/// Successive keys followed in one fetch.
const MAX_HOPS: usize = 4;
const DAY_SECS: u64 = 24 * 60 * 60;
/// A successor the pinned key handed over to.
const SIGNER_KEY: &str = "language_signer";
const CHECKED_KEY: &str = "language_checked";
const INDEX_FILE: &str = "index.json";

static DIR: OnceLock<PathBuf> = OnceLock::new();
static INDEX: RwLock<Option<Cached>> = RwLock::new(None);
/// Each installed pack's hash, by tag, to tell when the index has a newer one.
static INSTALLED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static UPDATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One round of background updates at a time.
pub fn begin_updates() -> bool {
    !UPDATING.swap(true, Ordering::AcqRel)
}

pub fn end_updates() {
    UPDATING.store(false, Ordering::Release);
}

#[derive(Clone)]
struct Cached {
    index: Arc<Index>,
    /// The signed event's `created_at`: an older index is never taken over it.
    at: u64,
}

fn now() -> u64 {
    vector_core::clock::now_secs()
}

fn sha256_hex(bytes: &[u8]) -> String {
    vector_core::crypto::sha256_hex(bytes)
}

fn trusted() -> PublicKey {
    super::device::get(SIGNER_KEY)
        .and_then(|k| PublicKey::from_hex(&k).ok())
        .unwrap_or_else(|| PublicKey::from_hex(SIGNER).expect("the pinned key is a key"))
}

/// The packs folder, and the index cached there. Fixtures have no folder: a devctl build reads
/// an unsigned index from `VECTOR_LANG_INDEX` so the Language page can be shown.
pub fn init(dir: Option<PathBuf>) {
    let Some(dir) = dir else {
        #[cfg(feature = "devctl")]
        if let Some(index) = std::env::var_os("VECTOR_LANG_INDEX")
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Index::parse(&s).ok())
        {
            *INDEX.write().unwrap_or_else(|e| e.into_inner()) = Some(Cached { index: Arc::new(index), at: 0 });
        }
        return;
    };
    if let Some((index, at, _)) = std::fs::read_to_string(dir.join(INDEX_FILE)).ok().and_then(|raw| verify(&raw, &trusted()).ok()) {
        *INDEX.write().unwrap_or_else(|e| e.into_inner()) = Some(Cached { index: Arc::new(index), at });
    }
    let _ = DIR.set(dir);
}

/// A pack read from disk or just downloaded.
pub fn note_installed(tag: &str, bytes: &[u8]) {
    INSTALLED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(tag.to_string(), sha256_hex(bytes));
}

fn any_installed() -> bool {
    INSTALLED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|m| !m.is_empty())
}

/// The newest index this device holds.
pub fn index() -> Option<Arc<Index>> {
    INDEX.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|c| c.index.clone())
}

/// Whether a background check is worth its network contact: a pack is installed and a day has
/// passed. A device that never chose a language never asks.
pub fn due() -> bool {
    DIR.get().is_some()
        && any_installed()
        && super::device::get(CHECKED_KEY).and_then(|s| s.parse::<u64>().ok()).is_none_or(|at| now().saturating_sub(at) >= DAY_SECS)
}

/// Whether the index was fetched within `secs`.
pub fn fresh(secs: u64) -> bool {
    index().is_some() && super::device::get(CHECKED_KEY).and_then(|s| s.parse::<u64>().ok()).is_some_and(|at| now().saturating_sub(at) < secs)
}

/// The packs installed here that the index has a newer build of.
pub fn updates() -> Vec<Entry> {
    let Some(index) = index() else { return vec![] };
    let installed = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(installed) = installed.as_ref() else { return vec![] };
    index.usable().filter(|e| installed.get(&e.tag).is_some_and(|sha| *sha != e.sha256)).cloned().collect()
}

/// The index in `raw` (a JSON array of events) that `trusted` vouches for, following each
/// handover to the newest key: `(index, created_at, the key that signed it)`.
fn verify(raw: &str, trusted: &PublicKey) -> Result<(Index, u64, PublicKey), String> {
    let invalid = || crate::t::language_index_invalid().to_string();
    let values: Vec<serde_json::Value> = serde_json::from_str(raw).map_err(|_| invalid())?;
    let events: Vec<Event> = values.iter().filter_map(|v| vector_core::event_json::event(&v.to_string()).ok()).collect();
    let mut key = *trusted;
    let mut best = None;
    for _ in 0..MAX_HOPS {
        let Some(ev) = events
            .iter()
            .filter(|e| e.pubkey == key && e.kind == Kind::Custom(KIND) && e.tags.identifier().as_deref() == Some(D_TAG))
            .filter(|e| e.verify().is_ok())
            .max_by_key(|e| e.created_at)
        else {
            break;
        };
        let index = Index::parse(&ev.content).map_err(|_| invalid())?;
        let next = index.next.as_deref().and_then(|n| PublicKey::from_hex(n).ok()).filter(|n| *n != key);
        best = Some((index, ev.created_at.as_secs(), key));
        match next {
            Some(n) => key = n,
            None => break,
        }
    }
    best.ok_or_else(|| crate::t::language_index_unsigned().to_string())
}

/// Fetches the index, keeping the one held when the fetched one is older. The release is asked
/// first; Vector's relays carry the same signed events for networks that can't reach it.
pub async fn refresh() -> Result<Arc<Index>, String> {
    let dir = DIR.get().ok_or_else(|| crate::t::language_pack_unsaved().to_string())?.clone();
    let trusted = trusted();
    let fetched = match from_release().await.and_then(|raw| verify(&raw, &trusted).map(|v| (v, raw))) {
        Ok(v) => v,
        Err(e) => {
            vector_core::log_debug!("[Languages] release: {e}; asking relays");
            from_relays(trusted).await.and_then(|raw| verify(&raw, &trusted).map(|v| (v, raw))).map_err(|_| e)?
        }
    };
    let ((index, at, signer), raw) = fetched;
    super::device::set(CHECKED_KEY, now().to_string());
    let mut held = INDEX.write().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = held.as_ref().filter(|c| c.at > at) {
        return Ok(c.index.clone());
    }
    if signer != trusted {
        super::device::set(SIGNER_KEY, signer.to_hex());
    }
    write_atomic(&dir.join(INDEX_FILE), raw.as_bytes());
    let index = Arc::new(index);
    *held = Some(Cached { index: index.clone(), at });
    Ok(index)
}

async fn from_release() -> Result<String, String> {
    let client = vector_core::net::shared_http_client();
    let mut resp = client.get(INDEX_URL).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(crate::t::language_index_status(resp.status().as_u16()).to_string());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > MAX_INDEX {
            return Err(crate::t::language_index_invalid().to_string());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| crate::t::language_index_invalid().to_string())
}

/// The newest index event of `trusted` and of each successor it names, from Vector's relays,
/// in the release's shape. A scratch client: no account, no authenticator.
async fn from_relays(trusted: PublicKey) -> Result<String, String> {
    use nostr_sdk::prelude::{ClientBuilder, Filter};
    use vector_core::ClientRelayExt as _;
    let client = vector_core::apply_transport(ClientBuilder::new(), vector_core::transport::Lane::Shared).build();
    for url in vector_core::state::TRUSTED_RELAYS {
        let _ = client.add_managed_relay(*url).await;
    }
    client.connect().await;
    let timeout = vector_core::relay_request_timeout(std::time::Duration::from_secs(8));
    let mut events = vec![];
    let mut key = trusted;
    for _ in 0..MAX_HOPS {
        let filter = Filter::new().kind(Kind::Custom(KIND)).author(key).identifier(D_TAG);
        let Ok(got) = client.fetch_events(filter).timeout(timeout).await else { break };
        let Some(ev) = got.into_iter().filter(|e| e.verify().is_ok()).max_by_key(|e| e.created_at) else { break };
        let next = Index::parse(&ev.content).ok().and_then(|i| i.next).and_then(|n| PublicKey::from_hex(&n).ok()).filter(|n| *n != key);
        events.push(serde_json::from_str::<serde_json::Value>(&ev.as_json()).map_err(|e| e.to_string())?);
        match next {
            Some(n) => key = n,
            None => break,
        }
    }
    client.shutdown().await;
    if events.is_empty() {
        return Err(crate::t::language_index_unsigned().to_string());
    }
    Ok(serde_json::Value::Array(events).to_string())
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> bool {
    let tmp = path.with_extension("part");
    let ok = std::fs::create_dir_all(path.parent().unwrap_or(path)).is_ok() && std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// Fetches `entry` from the first source that has it, checks it against the index's hash and
/// size, and keeps it. `progress` runs 0 to 1000 as it arrives.
pub async fn download(entry: Entry, progress: Arc<AtomicU32>) -> Result<vector_i18n::Pack, String> {
    let dir = DIR.get().ok_or_else(|| crate::t::language_pack_unsaved().to_string())?.clone();
    let sources = vector_core::blossom_servers::DEFAULT_BLOSSOM_SERVERS
        .iter()
        .map(|s| format!("{}/{}", s.trim_end_matches('/'), entry.sha256))
        .chain([format!("{MIRROR}/{}.vpack", entry.sha256)]);
    let mut last = String::new();
    for url in sources {
        progress.store(0, Ordering::Relaxed);
        match fetch(&url, &entry, &progress).await {
            Ok(bytes) => {
                let pack = vector_i18n::Pack::from_bytes(&bytes)?;
                if pack.tag != entry.tag {
                    return Err(crate::t::language_pack_mismatch().to_string());
                }
                if !write_atomic(&crate::lang::pack_path(&dir, &entry.tag), &bytes) {
                    return Err(crate::t::language_pack_unsaved().to_string());
                }
                note_installed(&entry.tag, &bytes);
                return Ok(pack);
            }
            Err(e) => {
                vector_core::log_debug!("[Languages] {url}: {e}");
                last = e;
            }
        }
    }
    Err(last)
}

async fn fetch(url: &str, entry: &Entry, progress: &AtomicU32) -> Result<Vec<u8>, String> {
    let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true)?;
    let mut resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(resp.status().to_string());
    }
    let size = entry.size.max(1);
    let mut body = Vec::with_capacity(entry.size.min(4 << 20) as usize);
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if (body.len() + chunk.len()) as u64 > entry.size {
            return Err(crate::t::language_pack_mismatch().to_string());
        }
        body.extend_from_slice(&chunk);
        progress.store((body.len() as u64 * 1000 / size) as u32, Ordering::Relaxed);
    }
    if body.len() as u64 != entry.size || sha256_hex(&body) != entry.sha256 {
        return Err(crate::t::language_pack_mismatch().to_string());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::{EventBuilder, FinalizeEvent as _, Keys, Tag};

    fn signed(keys: &Keys, index: &Index, at: u64) -> serde_json::Value {
        let ev = EventBuilder::new(Kind::Custom(KIND), index.to_json())
            .tag(Tag::identifier(D_TAG))
            .custom_created_at(nostr_sdk::prelude::Timestamp::from_secs(at))
            .finalize(keys)
            .unwrap();
        serde_json::from_str(&ev.as_json()).unwrap()
    }

    fn index(tag: &str) -> Index {
        Index {
            format: vector_i18n::index::INDEX_FORMAT,
            packs: vec![Entry {
                tag: tag.into(),
                name: tag.into(),
                english_name: tag.into(),
                rtl: false,
                coverage: 100,
                format: 1,
                size: 1,
                sha256: "0".repeat(64),
                english: String::new(),
            }],
            next: None,
        }
    }

    #[test]
    fn only_the_trusted_key_and_its_successors_are_believed() {
        let (pinned, next, stranger) = (Keys::generate(), Keys::generate(), Keys::generate());
        let raw = |v: Vec<serde_json::Value>| serde_json::to_string(&v).unwrap();

        let (i, at, by) = verify(&raw(vec![signed(&pinned, &index("es"), 10)]), &pinned.public_key()).unwrap();
        assert_eq!((i.packs[0].tag.as_str(), at, by), ("es", 10, pinned.public_key()));

        assert!(verify(&raw(vec![signed(&stranger, &index("es"), 10)]), &pinned.public_key()).is_err(), "anyone else's index is ignored");

        let mut forged = signed(&pinned, &index("es"), 10);
        forged["content"] = index("de").to_json().into();
        assert!(verify(&raw(vec![forged]), &pinned.public_key()).is_err(), "a changed index fails its signature");

        let handover = Index { next: Some(next.public_key().to_hex()), ..index("es") };
        let both = raw(vec![signed(&pinned, &handover, 10), signed(&next, &index("fr"), 11)]);
        let (i, _, by) = verify(&both, &pinned.public_key()).unwrap();
        assert_eq!((i.packs[0].tag.as_str(), by), ("fr", next.public_key()), "a handover moves to the successor's index");

        let alone = raw(vec![signed(&pinned, &handover, 10)]);
        assert_eq!(verify(&alone, &pinned.public_key()).unwrap().2, pinned.public_key(), "until the successor signs one, the handover itself stands");

        let newer = raw(vec![signed(&pinned, &index("es"), 10), signed(&pinned, &index("de"), 20)]);
        assert_eq!(verify(&newer, &pinned.public_key()).unwrap().0.packs[0].tag, "de", "the newest of a key's indexes");
    }

    #[test]
    fn the_pinned_key_parses() {
        assert!(PublicKey::from_hex(SIGNER).is_ok());
    }
}
