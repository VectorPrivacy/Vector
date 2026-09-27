//! AES-256-GCM with a 16-byte nonce and no associated data: the attachment cipher
//! (0xChat-compatible). A stitched AES-NI/PCLMULQDQ engine where it has been measured
//! to win, RustCrypto's `ctr` + `ghash` everywhere else; both produce the same bytes.

#[cfg(target_arch = "x86_64")]
mod x86;

use aes::cipher::{BlockCipherEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::Aes256;
use ghash::universal_hash::UniversalHash;
use ghash::GHash;

pub const TAG_LEN: usize = 16;

// One per message, built and dropped on the stack: boxing the larger variant would cost
// an allocation per seal.
#[allow(clippy::large_enum_variant)]
enum Engine {
    #[cfg(target_arch = "x86_64")]
    Stitched(x86::Gcm),
    Generic { ctr: ctr::Ctr32BE<Aes256>, ghash: GHash, tag_mask: [u8; TAG_LEN] },
}

/// One GCM message fed in chunks. Every chunk but the last must be a multiple of 16
/// bytes: GHASH may only pad the final block.
pub struct Stream {
    engine: Engine,
    len: u64,
    closed: bool,
}

impl Stream {
    /// The stitched engine wherever it runs, key setup included: in `crypto_bench sweep`
    /// it won every size from 0 bytes to 1 MiB against the generic one.
    pub fn new(key: &[u8; 32], nonce: &[u8; 16]) -> Self {
        #[cfg(target_arch = "x86_64")]
        if x86::available() {
            return Self::stitched(key, nonce);
        }
        Self::generic(key, nonce)
    }

    #[cfg(target_arch = "x86_64")]
    fn stitched(key: &[u8; 32], nonce: &[u8; 16]) -> Self {
        // SAFETY: callers establish `x86::instructions_present()`.
        let gcm = unsafe { x86::Gcm::new(key, nonce) };
        Stream { engine: Engine::Stitched(gcm), len: 0, closed: false }
    }

    fn generic(key: &[u8; 32], nonce: &[u8; 16]) -> Self {
        // H = E_K(0^128); a 16-byte nonce derives J0 through GHASH (SP 800-38D 7.1).
        let cipher = Aes256::new(&(*key).into());
        let mut h = ghash::Block::default();
        cipher.encrypt_block(&mut h);
        let mut j0_hash = GHash::new(&h);
        j0_hash.update_padded(nonce);
        let mut len_block = ghash::Block::default();
        len_block[8..].copy_from_slice(&128u64.to_be_bytes());
        j0_hash.update(&[len_block]);
        let j0 = j0_hash.finalize();

        // Keystream block 0 masks the tag; the payload starts at block 1.
        let mut ctr = ctr::Ctr32BE::<Aes256>::new(&(*key).into(), &j0);
        let mut tag_mask = [0u8; TAG_LEN];
        ctr.apply_keystream(&mut tag_mask);
        Stream { engine: Engine::Generic { ctr, ghash: GHash::new(&h), tag_mask }, len: 0, closed: false }
    }

    fn account(&mut self, chunk: &[u8]) {
        debug_assert!(!self.closed, "a chunk followed a partial one");
        self.closed = !chunk.len().is_multiple_of(16);
        self.len += chunk.len() as u64;
    }

    pub fn encrypt(&mut self, chunk: &mut [u8]) {
        self.account(chunk);
        match &mut self.engine {
            // SAFETY: construction checked the instructions.
            #[cfg(target_arch = "x86_64")]
            Engine::Stitched(gcm) => unsafe { gcm.encrypt(chunk) },
            Engine::Generic { ctr, ghash, .. } => {
                ctr.apply_keystream(chunk);
                ghash.update_padded(chunk);
            }
        }
    }

    pub fn decrypt(&mut self, chunk: &mut [u8]) {
        self.account(chunk);
        match &mut self.engine {
            // SAFETY: construction checked the instructions.
            #[cfg(target_arch = "x86_64")]
            Engine::Stitched(gcm) => unsafe { gcm.decrypt(chunk) },
            Engine::Generic { ctr, ghash, .. } => {
                ghash.update_padded(chunk);
                ctr.apply_keystream(chunk);
            }
        }
    }

    /// The tag over everything fed so far.
    pub fn finish(self) -> [u8; TAG_LEN] {
        match self.engine {
            // SAFETY: construction checked the instructions.
            #[cfg(target_arch = "x86_64")]
            Engine::Stitched(gcm) => unsafe { gcm.finish(self.len) },
            Engine::Generic { mut ghash, tag_mask, .. } => {
                let mut lengths = ghash::Block::default();
                lengths[8..].copy_from_slice(&(self.len * 8).to_be_bytes());
                ghash.update(&[lengths]);
                let mut tag = [0u8; TAG_LEN];
                for (t, (s, m)) in tag.iter_mut().zip(ghash.finalize().iter().zip(tag_mask)) {
                    *t = s ^ m;
                }
                tag
            }
        }
    }
}

/// Seal `data` in place and return the tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 16], data: &mut [u8]) -> [u8; TAG_LEN] {
    let mut s = Stream::new(key, nonce);
    s.encrypt(data);
    s.finish()
}

/// The tag did not authenticate the ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagMismatch;

/// Open `data` in place. On a bad tag the buffer is wiped, so no unauthenticated
/// plaintext survives the call.
pub fn open(key: &[u8; 32], nonce: &[u8; 16], data: &mut [u8], tag: &[u8; TAG_LEN]) -> Result<(), TagMismatch> {
    let mut s = Stream::new(key, nonce);
    s.decrypt(data);
    if tags_equal(&s.finish(), tag) {
        Ok(())
    } else {
        super::wipe(data);
        Err(TagMismatch)
    }
}

/// Every byte compared, whatever the first difference.
pub fn tags_equal(a: &[u8; TAG_LEN], b: &[u8; TAG_LEN]) -> bool {
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    core::hint::black_box(diff) == 0
}

/// The engines with their choice forced, for measuring where the stitched one pays.
#[doc(hidden)]
pub mod bench {
    use super::*;

    /// Whether this CPU can run the stitched engine at all (VAES or not).
    pub fn stitched_present() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            x86::instructions_present()
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// Whether production picks the stitched engine on this CPU.
    pub fn stitched_in_production() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            x86::available()
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// A stream on the named engine ("stitched" or "generic"), or None if this CPU lacks it.
    pub fn stream(engine: &str, key: &[u8; 32], nonce: &[u8; 16]) -> Option<Stream> {
        match engine {
            #[cfg(target_arch = "x86_64")]
            "stitched" if x86::instructions_present() => Some(Stream::stitched(key, nonce)),
            "generic" => Some(Stream::generic(key, nonce)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
