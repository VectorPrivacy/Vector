//! SHA-256 for bulk data: attachments, files and blobs.
//!
//! aarch64 hashes through `bitcoin_hashes`, which runtime-detects the ARMv8 SHA-256
//! instructions; `sha2` 0.10 only uses them behind its `asm` feature. Elsewhere `sha2`
//! stays: it runtime-detects SHA-NI too, and without SHA-NI its software path measured
//! 9% faster than `bitcoin_hashes`' on a Cascade Lake Xeon.

#[cfg(target_arch = "aarch64")]
use bitcoin_hashes::HashEngine as _;
#[cfg(not(target_arch = "aarch64"))]
use sha2::Digest as _;

/// An incremental SHA-256.
#[derive(Clone, Default)]
pub struct Sha256 {
    #[cfg(target_arch = "aarch64")]
    engine: bitcoin_hashes::sha256::HashEngine,
    #[cfg(not(target_arch = "aarch64"))]
    engine: sha2::Sha256,
}

impl Sha256 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, data: impl AsRef<[u8]>) {
        #[cfg(target_arch = "aarch64")]
        self.engine.input(data.as_ref());
        #[cfg(not(target_arch = "aarch64"))]
        self.engine.update(data.as_ref());
    }

    pub fn finalize(self) -> [u8; 32] {
        #[cfg(target_arch = "aarch64")]
        {
            self.engine.finalize().to_byte_array()
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            self.engine.finalize().into()
        }
    }
}

/// SHA-256 of `data` in one call.
pub fn digest(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// HMAC-SHA256 (RFC 2104) of `parts`, concatenated, under `key`.
pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    use zeroize::Zeroize as _;
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut pad = block.map(|b| b ^ 0x36);
    let mut inner = Sha256::new();
    inner.update(pad);
    for part in parts {
        inner.update(part);
    }
    let mut inner = inner.finalize();
    pad = block.map(|b| b ^ 0x5c);
    let mut outer = Sha256::new();
    outer.update(pad);
    outer.update(inner);
    block.zeroize();
    pad.zeroize();
    inner.zeroize();
    outer.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn matches_sha2_across_block_and_update_boundaries() {
        assert_eq!(
            super::super::hex::encode(&digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 31 + 7) as u8).collect();
        for len in [0usize, 1, 55, 56, 63, 64, 65, 127, 128, 1000, 5000] {
            let want: [u8; 32] = sha2::Sha256::digest(&data[..len]).into();
            assert_eq!(digest(&data[..len]), want, "len {len}");
            for split in [0, 1, 63, 64, len / 2, len] {
                let split = split.min(len);
                let mut h = Sha256::new();
                h.update(&data[..split]);
                h.update(&data[split..len]);
                assert_eq!(h.finalize(), want, "len {len} split {split}");
            }
        }
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        let hex = |b: &[u8; 32]| super::super::hex::encode(b);
        // Test cases 1, 2 and 6: a short key, a short ASCII key, a key longer than a block.
        assert_eq!(hex(&hmac(&[0x0b; 20], &[b"Hi There"])), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        assert_eq!(hex(&hmac(b"Jefe", &[b"what do ya want ", b"for nothing?"])), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert_eq!(
            hex(&hmac(&[0xaa; 131], &[b"Test Using Larger Than Block-Size Key - Hash Key First"])),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
