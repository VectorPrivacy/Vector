//! Secrets a message should never carry: a Nostr private key, a wallet's seed phrase or a
//! wallet private key. Clients ask before sending one; only a checksum that verifies
//! counts, so ordinary words and ids never trip it.

use bip39::{Language, Mnemonic};
use nostr_sdk::prelude::{FromBech32, SecretKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    /// An `nsec`, or the password-wrapped `ncryptsec`: the keys to a Nostr account.
    NostrKey,
    /// A BIP-39 recovery phrase whose checksum holds.
    SeedPhrase,
    /// A WIF private key or an extended private key (`xprv` and kin), checksum intact.
    WalletKey,
}

/// Seed phrases are 12 to 24 words, in steps of three.
const PHRASE_LENGTHS: [usize; 5] = [24, 21, 18, 15, 12];
/// Plenty for any phrase; bounds the work on a long run of wordlist words.
const MAX_RUN: usize = 64;

/// The most sensitive secret in `text`, if any.
pub fn detect(text: &str) -> Option<SecretKind> {
    if has_nostr_key(text) {
        return Some(SecretKind::NostrKey);
    }
    if has_wallet_key(text) {
        return Some(SecretKind::WalletKey);
    }
    has_seed_phrase(text).then_some(SecretKind::SeedPhrase)
}

fn has_nostr_key(text: &str) -> bool {
    tokens(text, |c| c.is_ascii_alphanumeric()).any(|t| {
        let lower = t.to_ascii_lowercase();
        (lower.starts_with("nsec1") && SecretKey::from_bech32(&lower).is_ok())
            // Its checksum is bech32's own: length and alphabet are enough for a warning.
            || (lower.starts_with("ncryptsec1") && lower.len() > 100 && lower[10..].bytes().all(is_bech32))
    })
}

fn is_bech32(b: u8) -> bool {
    matches!(b, b'0' | b'2'..=b'9' | b'a' | b'c'..=b'h' | b'j'..=b'n' | b'p'..=b'z')
}

fn has_wallet_key(text: &str) -> bool {
    tokens(text, |c| c.is_ascii_alphanumeric()).any(|t| {
        let n = t.len();
        // WIF: a version byte, 32 key bytes and an optional compression flag. Extended
        // private keys: 78 bytes, named by their prefix (xprv, yprv, zprv, tprv…).
        let wif = (51..=52).contains(&n);
        let extended = (110..=112).contains(&n) && t.get(1..4) == Some("prv");
        if !wif && !extended {
            return false;
        }
        match base58check(t) {
            Some(payload) if wif => payload.len() == 33 || (payload.len() == 34 && payload[33] == 0x01),
            Some(payload) => payload.len() == 78,
            None => false,
        }
    })
}

fn has_seed_phrase(text: &str) -> bool {
    let lang = Language::English;
    let words: Vec<String> = tokens(text, |c| c.is_ascii_alphabetic()).map(|w| w.to_ascii_lowercase()).collect();
    let mut run: Vec<&str> = Vec::new();
    // A run of wordlist words long enough to hold a phrase is tried at every length and offset.
    let check = |run: &[&str]| {
        PHRASE_LENGTHS.iter().filter(|&&len| run.len() >= len).any(|&len| {
            run.windows(len).any(|w| Mnemonic::parse_in_normalized(lang, &w.join(" ")).is_ok())
        })
    };
    for w in &words {
        if lang.find_word(w).is_some() && run.len() < MAX_RUN {
            run.push(w);
            continue;
        }
        if check(&run) {
            return true;
        }
        run.clear();
        if lang.find_word(w).is_some() {
            run.push(w);
        }
    }
    check(&run)
}

/// The maximal runs of characters `keep` accepts.
fn tokens(text: &str, keep: impl Fn(char) -> bool + Copy) -> impl Iterator<Item = &str> {
    text.split(move |c: char| !keep(c)).filter(|t| !t.is_empty())
}

const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Base58 with a four-byte double-SHA256 checksum: the payload when it verifies.
fn base58check(s: &str) -> Option<Vec<u8>> {
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    for c in s.bytes() {
        let mut carry = BASE58.iter().position(|&b| b == c)? as u32;
        for b in bytes.iter_mut().rev() {
            carry += (*b as u32) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let zeros = s.bytes().take_while(|&b| b == b'1').count();
    let mut full = vec![0u8; zeros];
    full.extend(bytes);
    if full.len() < 5 {
        return None;
    }
    let (payload, sum) = full.split_at(full.len() - 4);
    let digest = Sha256::digest(Sha256::digest(payload));
    (digest[..4] == *sum).then(|| payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nostr_keys_are_found_and_lookalikes_are_not() {
        let keys = nostr_sdk::prelude::Keys::generate();
        let nsec = nostr_sdk::prelude::ToBech32::to_bech32(keys.secret_key()).unwrap();
        assert_eq!(detect(&format!("here you go: {nsec} thanks")), Some(SecretKind::NostrKey));
        assert_eq!(detect(&nsec.to_uppercase()), Some(SecretKind::NostrKey));
        let npub = nostr_sdk::prelude::ToBech32::to_bech32(&keys.public_key()).unwrap();
        assert_eq!(detect(&format!("my npub is {npub}")), None);
        let mut broken = nsec.clone();
        broken.replace_range(10..11, if &nsec[10..11] == "q" { "p" } else { "q" });
        assert_eq!(detect(&broken), None, "a checksum that fails is not a key");
    }

    #[test]
    fn seed_phrases_need_a_valid_checksum() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        assert_eq!(detect(phrase), Some(SecretKind::SeedPhrase));
        assert_eq!(detect(&format!("backup:\n1. {}", phrase.replace(' ', "\n2. "))), Some(SecretKind::SeedPhrase));
        assert_eq!(detect(&format!("Pls check my wallet {} ok?", phrase.to_uppercase())), Some(SecretKind::SeedPhrase));
        let mnemonic = Mnemonic::generate_in(Language::English, 24).unwrap().to_string();
        assert_eq!(detect(&mnemonic), Some(SecretKind::SeedPhrase));
        // Twelve wordlist words whose checksum fails: everyday English, not a wallet.
        assert_eq!(detect("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon"), None);
        assert_eq!(detect("we should all go to the beach this summer and have a great time with family"), None);
    }

    #[test]
    fn wallet_keys_need_a_valid_checksum() {
        // Well-known test vectors (mastering bitcoin): uncompressed and compressed WIF.
        assert_eq!(detect("5HueCGU8rMjxEXxiPuD5BDku4MkFqeZyd4dZ1jvhTVqvbTLvyTJ"), Some(SecretKind::WalletKey));
        assert_eq!(detect("key KwdMAjGmerYanjeui5SHS7JkmpZvVipYvB2LJGU1ZxJwYvP98617 there"), Some(SecretKind::WalletKey));
        assert_eq!(detect("xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi"), Some(SecretKind::WalletKey));
        // A bitcoin address is base58check too, but not a key.
        assert_eq!(detect("send to 1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2"), None);
        assert_eq!(detect("5HueCGU8rMjxEXxiPuD5BDku4MkFqeZyd4dZ1jvhTVqvbTLvyTX"), None, "checksum fails");
    }
}
