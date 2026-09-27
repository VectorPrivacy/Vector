//! ChaCha20-Poly1305 (RFC 8439), tuned for many short messages.
//!
//! Byte-for-byte the same construction as the `chacha20poly1305` crate; what
//! differs is the cost shape. The Poly1305 key block is produced in the same SIMD
//! pass as the first data blocks, and Poly1305 is scalar, so a chat message costs
//! one keystream pass and no per-message vector setup.

mod chacha;
mod poly1305;

use chacha::{Backend, State, BUF};
use poly1305::Poly1305;
use zeroize::Zeroize;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

/// RFC 8439's 32-bit block counter starts at 1 for data.
const MAX_LEN: u64 = (u32::MAX as u64 - 1) * 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("aead error")
    }
}

impl std::error::Error for Error {}

/// A keyed ChaCha20-Poly1305 instance. The expanded key is zeroized on drop.
pub struct ChaCha20Poly1305 {
    key: [u32; 8],
    backend: Backend,
}

impl Drop for ChaCha20Poly1305 {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl ChaCha20Poly1305 {
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        let mut k = [0u32; 8];
        for (w, c) in k.iter_mut().zip(key.chunks_exact(4)) {
            *w = u32::from_le_bytes(c.try_into().unwrap());
        }
        Self { key: k, backend: Backend::detect() }
    }

    #[cfg(test)]
    fn with_backend(key: &[u8; KEY_LEN], backend: Backend) -> Self {
        let mut s = Self::new(key);
        s.backend = backend;
        s
    }

    fn state(&self, nonce: &[u8; NONCE_LEN]) -> State {
        let mut n = [0u32; 3];
        for (w, c) in n.iter_mut().zip(nonce.chunks_exact(4)) {
            *w = u32::from_le_bytes(c.try_into().unwrap());
        }
        State { key: self.key, nonce: n }
    }

    /// Encrypt `buf` in place and return the tag.
    pub fn seal_in_place(&self, nonce: &[u8; NONCE_LEN], aad: &[u8], buf: &mut [u8]) -> Result<[u8; TAG_LEN], Error> {
        if buf.len() as u64 > MAX_LEN {
            return Err(Error);
        }
        let st = self.state(nonce);
        let mut ks = [0u8; BUF];
        let (mut mac, first) = self.first_pass(&st, buf.len(), &mut ks);
        mac.padded(aad);
        let used = self.xor_stream(&st, buf, &mut ks, first);
        mac.padded(buf);
        let tag = finish(mac, aad.len(), buf.len());
        wipe(&mut ks[..used]);
        Ok(tag)
    }

    /// Verify and decrypt `buf` in place. On a bad tag `buf` is left as ciphertext.
    pub fn open_in_place(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), Error> {
        if buf.len() as u64 > MAX_LEN {
            return Err(Error);
        }
        let st = self.state(nonce);
        let mut ks = [0u8; BUF];
        let (mut mac, first) = self.first_pass(&st, buf.len(), &mut ks);
        mac.padded(aad);
        mac.padded(buf);
        let expected = finish(mac, aad.len(), buf.len());
        if !ct_eq(&expected, tag) {
            wipe(&mut ks[..64 * first]);
            return Err(Error);
        }
        let used = self.xor_stream(&st, buf, &mut ks, first);
        wipe(&mut ks[..used]);
        Ok(())
    }

    /// Opens `nonce(12) || ciphertext || tag(16)` in place and returns the plaintext
    /// range within `data`. The storage format every at-rest blob uses.
    pub fn open_framed_in_place<'a>(&self, data: &'a mut [u8]) -> Result<&'a mut [u8], Error> {
        if data.len() < NONCE_LEN + TAG_LEN {
            return Err(Error);
        }
        let (nonce, rest) = data.split_at_mut(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = (&*nonce).try_into().unwrap();
        let (body, tag) = rest.split_at_mut(rest.len() - TAG_LEN);
        let tag: [u8; TAG_LEN] = (&*tag).try_into().unwrap();
        self.open_in_place(&nonce, &[], body, &tag)?;
        Ok(body)
    }

    /// Generates the first keystream batch (counter 0 onward) into `ks` and keys
    /// Poly1305 from block 0. Returns the MAC and the batch's block count; blocks
    /// 1.. of that batch are data keystream that `xor_stream` consumes first.
    #[inline]
    fn first_pass(&self, st: &State, len: usize, ks: &mut [u8; BUF]) -> (Poly1305, usize) {
        let want = 1 + len.div_ceil(64);
        let n = self.backend.keystream(st, 0, want, ks);
        let pk: &[u8; 32] = ks[..32].try_into().unwrap();
        (Poly1305::new(pk), n)
    }

    /// Returns how many leading bytes of `ks` hold keystream and need wiping.
    #[inline]
    fn xor_stream(&self, st: &State, buf: &mut [u8], ks: &mut [u8; BUF], first: usize) -> usize {
        let mut used = 64 * first;
        let avail = 64 * (first - 1);
        let head = avail.min(buf.len());
        xor(&mut buf[..head], &ks[64..64 + head]);
        let mut counter = first as u32;
        let mut rest = &mut buf[head..];
        while !rest.is_empty() {
            let want = rest.len().div_ceil(64);
            let n = self.backend.keystream(st, counter, want, ks);
            used = used.max(64 * n);
            let take = (64 * n).min(rest.len());
            let (now, later) = rest.split_at_mut(take);
            xor(now, &ks[..take]);
            rest = later;
            counter = counter.wrapping_add(n as u32);
        }
        used
    }
}

#[inline]
fn finish(mut mac: Poly1305, aad_len: usize, ct_len: usize) -> [u8; TAG_LEN] {
    let mut lens = [0u8; 16];
    lens[..8].copy_from_slice(&(aad_len as u64).to_le_bytes());
    lens[8..].copy_from_slice(&(ct_len as u64).to_le_bytes());
    mac.blocks(&lens);
    mac.finish()
}

#[inline(always)]
fn xor(dst: &mut [u8], ks: &[u8]) {
    for (d, k) in dst.iter_mut().zip(ks) {
        *d ^= *k;
    }
}

/// Zeroize with word-wide volatile stores. `zeroize` on a byte slice issues one
/// volatile store per byte, which costs more than the cipher on short messages.
#[inline]
pub fn wipe(buf: &mut [u8]) {
    // SAFETY: the slice is `len` writable bytes.
    unsafe { wipe_raw(buf.as_mut_ptr(), buf.len()) }
}

/// [`wipe`] over a vector's whole allocation, spare capacity included, then empty it.
#[inline]
pub fn wipe_vec(v: &mut Vec<u8>) {
    // SAFETY: the allocation is `capacity` writable bytes; zeros are a valid u8 anywhere.
    unsafe {
        wipe_raw(v.as_mut_ptr(), v.capacity());
        v.set_len(0);
    }
}

/// SAFETY: `p` must be valid for `len` byte writes.
#[inline]
unsafe fn wipe_raw(p: *mut u8, len: usize) {
    use core::ptr::write_volatile;
    let head = p.align_offset(8).min(len);
    let words = (len - head) / 8;
    for i in 0..head {
        write_volatile(p.add(i), 0);
    }
    let w = p.add(head) as *mut u64;
    for i in 0..words {
        write_volatile(w.add(i), 0);
    }
    for i in head + 8 * words..len {
        write_volatile(p.add(i), 0);
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[inline]
fn ct_eq(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let d = u128::from_ne_bytes(*a) ^ u128::from_ne_bytes(*b);
    let folded = (d as u64) | ((d >> 64) as u64);
    core::hint::black_box(folded) == 0
}

#[cfg(test)]
mod tests;

