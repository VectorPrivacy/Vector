//! Building and signing what the app downloads. The key comes from `VECTOR_LANGUAGES_KEY` (hex
//! or nsec), never an argument, so it stays out of process lists and shell history. During a
//! handover `VECTOR_LANGUAGES_OLD_KEY` signs one more index naming the new key.

use std::io::Write as _;
use std::path::Path;

use nostr::prelude::*;
use vector_i18n::index::{D_TAG, Entry, INDEX_FORMAT, Index, KIND};

/// Below this a language isn't listed: a selector entry that is nearly all English helps no one.
const MIN_LISTED: u32 = 10;
/// How long an upload authorisation lasts.
const AUTH_SECS: u64 = 300;

fn die(why: impl std::fmt::Display) -> ! {
    eprintln!("{why}");
    std::process::exit(1);
}

fn key_from_env(var: &str) -> Option<Keys> {
    let v = std::env::var(var).ok().filter(|v| !v.trim().is_empty())?;
    Some(Keys::parse(v.trim()).unwrap_or_else(|_| die(format!("{var} is not a secret key"))))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn sign(keys: &Keys, index: &Index) -> Event {
    EventBuilder::new(Kind::Custom(KIND), index.to_json())
        .tag(Tag::identifier(D_TAG))
        .finalize(keys)
        .unwrap_or_else(|e| die(format!("signing: {e}")))
}

/// The entries of the index published last, from the newest event one of our keys signed.
fn previous(path: &Path, ours: &[PublicKey]) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    let events: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    events
        .iter()
        .filter_map(|v| Event::from_json(v.to_string()).ok())
        .filter(|e| ours.contains(&e.pubkey) && e.verify().is_ok())
        .max_by_key(|e| e.created_at)
        .and_then(|e| Index::parse(&e.content).ok())
        .map(|i| i.packs)
        .unwrap_or_default()
}

pub fn publish(root: &Path, out: &Path, prev: Option<&Path>) {
    let key = key_from_env("VECTOR_LANGUAGES_KEY").unwrap_or_else(|| die("VECTOR_LANGUAGES_KEY is not set"));
    let old = key_from_env("VECTOR_LANGUAGES_OLD_KEY");
    let ours: Vec<PublicKey> = std::iter::once(key.public_key()).chain(old.as_ref().map(|k| k.public_key())).collect();
    let before = prev.map(|p| previous(p, &ours)).unwrap_or_default();
    std::fs::create_dir_all(out).unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
    let mut packs = vec![];
    let mut failed = false;
    println!("{:<12}{:>9}{:>9}  {}", "language", "coverage", "bytes", "sha256");
    for tag in crate::tags(root) {
        match crate::build(&tag, root) {
            Ok((p, bytes)) => {
                if p.coverage < MIN_LISTED {
                    println!("{tag:<12}{:>8}%{:>9}  not listed below {MIN_LISTED}%", p.coverage, bytes.len());
                    continue;
                }
                let sha = sha256_hex(&bytes);
                let file = out.join(format!("{sha}.vpack"));
                std::fs::write(&file, &bytes).unwrap_or_else(|e| die(format!("{}: {e}", file.display())));
                println!("{tag:<12}{:>8}%{:>9}  {sha}", p.coverage, bytes.len());
                packs.push(Entry {
                    tag: p.tag,
                    name: p.name,
                    english_name: p.english_name,
                    rtl: p.rtl,
                    coverage: p.coverage,
                    format: vector_i18n::PACK_FORMAT,
                    size: bytes.len() as u64,
                    sha256: sha,
                    english: p.english,
                });
            }
            Err(broken) => {
                failed = true;
                for b in &broken {
                    eprintln!("{tag}: broken {b}");
                }
                // The last good pack stays listed until this one is fixed.
                if let Some(e) = before.iter().find(|e| e.tag == tag) {
                    println!("{tag:<12}{:>8}%{:>9}  {} (kept: this one is broken)", e.coverage, e.size, e.sha256);
                    packs.push(e.clone());
                } else {
                    println!("{tag:<12}  not listed: broken");
                }
            }
        }
    }
    let index = Index { format: INDEX_FORMAT, packs, next: None };
    let mut events = vec![];
    if let Some(old) = &old {
        let handover = Index { next: Some(key.public_key().to_hex()), ..index.clone() };
        events.push(sign(old, &handover));
    }
    events.push(sign(&key, &index));
    let json: Vec<serde_json::Value> = events.iter().map(|e| serde_json::from_str(&e.as_json()).expect("an event is JSON")).collect();
    let file = out.join("index.json");
    std::fs::write(&file, serde_json::to_string(&json).unwrap()).unwrap_or_else(|e| die(format!("{}: {e}", file.display())));
    println!("signed by {} → {}", key.public_key().to_hex(), file.display());
    if failed {
        std::process::exit(1);
    }
}

/// A new signing key, written only to `file`: the public half is printed to pin in the app.
pub fn keygen(file: &Path) {
    let keys = Keys::generate();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(file).unwrap_or_else(|e| die(format!("{}: {e}", file.display())));
    f.write_all(keys.secret_key().to_secret_hex().as_bytes()).unwrap_or_else(|e| die(format!("{}: {e}", file.display())));
    println!("public key: {}", keys.public_key().to_hex());
}

/// A BUD-02 `Authorization` header for uploading `file`, signed by a key made for it alone, so
/// the index key signs nothing but indexes.
pub fn blossom_auth(file: &Path) {
    let bytes = std::fs::read(file).unwrap_or_else(|e| die(format!("{}: {e}", file.display())));
    let expires = Timestamp::from_secs(Timestamp::now().as_secs() + AUTH_SECS);
    let tags = [
        Tag::parse(["t", "upload"]).unwrap(),
        Tag::parse(["x".to_string(), sha256_hex(&bytes)]).unwrap(),
        Tag::expiration(expires),
    ];
    let ev = EventBuilder::new(Kind::Custom(24242), "Upload a Vector language pack")
        .tags(tags)
        .finalize(&Keys::generate())
        .unwrap_or_else(|e| die(format!("signing: {e}")));
    println!("Nostr {}", base64(ev.as_json().as_bytes()));
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}
