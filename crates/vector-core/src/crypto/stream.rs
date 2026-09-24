//! Attachment AES-256-GCM over a file, a chunk at a time: the cipher of
//! [`super::decrypt_data`] (16-byte nonce, 16-byte tag appended) without the file
//! ever sitting whole in memory.
//!
//! GCM is CTR encryption plus a GHASH over the ciphertext, and both run
//! incrementally. The catch is that the tag only speaks at the end, so plaintext
//! is written to a file the caller keeps out of sight until this returns Ok.

use std::io::{Read, Write};
use std::path::Path;

use aes::cipher::{BlockEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::Aes256;
use ghash::universal_hash::UniversalHash;
use ghash::GHash;
use sha2::{Digest, Sha256};

use super::hex;

/// Read size for every streamed pass. A multiple of the 16-byte GHASH block, so
/// only the final read can leave a partial block.
const CHUNK: usize = 1 << 20;

/// What one pass over a downloaded file learned.
#[derive(Debug)]
pub struct StreamedFile {
    /// SHA-256 of the file as downloaded: a Blossom URL's content address.
    pub source_sha256: String,
    /// SHA-256 of what was written out: the attachment's identity.
    pub output_sha256: String,
    pub output_len: u64,
}

/// What a pass stopped by its caller returns.
pub const CANCELLED: &str = "Upload cancelled";

/// Tell `progress` when `done` of `total` reaches a new whole percentage; false when
/// it asks the pass to stop.
fn report(done: u64, total: u64, reported: &mut u8, progress: &mut dyn FnMut(u8) -> bool) -> bool {
    let pct = done.saturating_mul(100).checked_div(total).map_or(100, |p| p.min(100) as u8);
    if pct > *reported {
        *reported = pct;
        return progress(pct);
    }
    true
}

fn hex32(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Decrypt `src` (ciphertext || tag) into `dst`, hashing both sides on the way.
/// `dst` holds unauthenticated plaintext until this returns: on any error it is
/// removed, and a tag mismatch is reported as a decryption failure.
pub fn decrypt_file(src: &Path, dst: &Path, key_hex: &str, nonce_hex: &str) -> Result<StreamedFile, String> {
    decrypt_file_with_progress(src, dst, key_hex, nonce_hex, &mut |_| true)
}

/// [`decrypt_file`], telling `progress` each new whole percentage of the file opened.
/// `progress` answering false stops the pass, as for [`encrypt_file_with_progress`].
pub fn decrypt_file_with_progress(src: &Path, dst: &Path, key_hex: &str, nonce_hex: &str, progress: &mut dyn FnMut(u8) -> bool) -> Result<StreamedFile, String> {
    let result = decrypt_file_inner(src, dst, key_hex, nonce_hex, progress);
    if result.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    result
}

/// The GCM state for one message: the keystream (positioned past the tag's block),
/// the tag's mask, and a GHASH ready for the ciphertext.
fn gcm_start(key_hex: &str, nonce_hex: &str) -> Result<(ctr::Ctr32BE<Aes256>, [u8; 16], GHash), String> {
    let key: [u8; 32] = hex::decode(key_hex)
        .map_err(|e| format!("Invalid key: {}", e))?
        .try_into()
        .map_err(|_| "Invalid decryption key".to_string())?;
    let nonce = hex::decode(nonce_hex).map_err(|e| format!("Invalid nonce: {}", e))?;
    if nonce.len() != 16 {
        return Err("Invalid nonce length".to_string());
    }
    // H = E_K(0^128); a 16-byte nonce derives J0 through GHASH (SP 800-38D 7.1).
    let cipher = Aes256::new(&key.into());
    let mut h = ghash::Block::default();
    cipher.encrypt_block(&mut h);
    let mut j0_hash = GHash::new(&h);
    j0_hash.update_padded(&nonce);
    let mut len_block = ghash::Block::default();
    len_block[8..].copy_from_slice(&((nonce.len() as u64) * 8).to_be_bytes());
    j0_hash.update(&[len_block]);
    let j0 = j0_hash.finalize();

    // Keystream block 0 masks the tag; the payload starts at block 1.
    let mut ctr = ctr::Ctr32BE::<Aes256>::new(&key.into(), &j0);
    let mut tag_mask = [0u8; 16];
    ctr.apply_keystream(&mut tag_mask);
    Ok((ctr, tag_mask, GHash::new(&h)))
}

/// The tag over `ciphertext_len` bytes already fed to `ghash`.
fn gcm_tag(mut ghash: GHash, tag_mask: [u8; 16], ciphertext_len: u64) -> [u8; 16] {
    let mut lengths = ghash::Block::default();
    lengths[8..].copy_from_slice(&(ciphertext_len * 8).to_be_bytes());
    ghash.update(&[lengths]);
    let mut tag = [0u8; 16];
    for (t, (s, m)) in tag.iter_mut().zip(ghash.finalize().iter().zip(tag_mask)) {
        *t = s ^ m;
    }
    tag
}

/// Encrypt `src` into `dst` (ciphertext || tag), the counterpart of [`decrypt_file`].
/// Returns the SHA-256 and length of what was written: the blob a server will hold.
pub fn encrypt_file(src: &Path, dst: &Path, key_hex: &str, nonce_hex: &str) -> Result<(String, u64), String> {
    encrypt_file_with_progress(src, dst, key_hex, nonce_hex, &mut |_| true)
}

/// [`encrypt_file`], telling `progress` each new whole percentage of the file sealed.
/// `progress` answering false stops the pass: nothing is written and the error says
/// it was cancelled.
pub fn encrypt_file_with_progress(src: &Path, dst: &Path, key_hex: &str, nonce_hex: &str, progress: &mut dyn FnMut(u8) -> bool) -> Result<(String, u64), String> {
    let result = encrypt_file_inner(src, dst, key_hex, nonce_hex, progress);
    if result.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    result
}

/// GCM's limit for one message (2^36 - 32 bytes): past it the 32-bit counter wraps and
/// the keystream repeats.
const GCM_MAX_PLAINTEXT: u64 = (1 << 36) - 32;

fn encrypt_file_inner(src: &Path, dst: &Path, key_hex: &str, nonce_hex: &str, progress: &mut dyn FnMut(u8) -> bool) -> Result<(String, u64), String> {
    let (mut ctr, tag_mask, mut ghash) = gcm_start(key_hex, nonce_hex)?;
    let mut input = std::fs::File::open(src).map_err(|e| format!("open file: {e}"))?;
    let total = input.metadata().map(|m| m.len()).unwrap_or(0);
    if total > GCM_MAX_PLAINTEXT {
        return Err("File too large to encrypt as one attachment (over 64 GiB)".to_string());
    }
    let mut reported = 0u8;
    let mut output = std::io::BufWriter::with_capacity(
        CHUNK,
        std::fs::File::create(dst).map_err(|e| format!("create ciphertext: {e}"))?,
    );
    let mut blob_hash = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut len = 0u64;
    loop {
        // Fill the buffer before encrypting: GHASH may only pad the final block.
        let mut n = 0;
        while n < CHUNK {
            let got = input.read(&mut buf[n..]).map_err(|e| format!("read file: {e}"))?;
            if got == 0 {
                break;
            }
            n += got;
        }
        if n == 0 {
            break;
        }
        let chunk = &mut buf[..n];
        ctr.apply_keystream(chunk);
        ghash.update_padded(chunk);
        blob_hash.update(&*chunk);
        output.write_all(chunk).map_err(|e| format!("write ciphertext: {e}"))?;
        len += n as u64;
        if !report(len, total, &mut reported, progress) {
            return Err(CANCELLED.to_string());
        }
        if n < CHUNK {
            break;
        }
    }
    let tag = gcm_tag(ghash, tag_mask, len);
    blob_hash.update(tag);
    output.write_all(&tag).map_err(|e| format!("write ciphertext: {e}"))?;
    let file = output.into_inner().map_err(|e| format!("write ciphertext: {e}"))?;
    file.sync_all().map_err(|e| format!("sync ciphertext: {e}"))?;
    Ok((hex32(&blob_hash.finalize()), len + 16))
}

fn decrypt_file_inner(src: &Path, dst: &Path, key_hex: &str, nonce_hex: &str, progress: &mut dyn FnMut(u8) -> bool) -> Result<StreamedFile, String> {
    let (mut ctr, tag_mask, mut tag_hash) = gcm_start(key_hex, nonce_hex)?;
    let mut input = std::fs::File::open(src).map_err(|e| format!("open download: {e}"))?;
    let total = input.metadata().map_err(|e| format!("stat download: {e}"))?.len();
    if total < 16 {
        return Err(format!("Invalid Input: encrypted data too small ({} bytes, minimum 16 bytes required for authentication tag)", total));
    }
    let ciphertext_len = total - 16;
    if ciphertext_len > GCM_MAX_PLAINTEXT {
        return Err("Invalid Input: encrypted data exceeds the AES-GCM limit".to_string());
    }
    let mut reported = 0u8;

    let mut source_hash = Sha256::new();
    let mut output_hash = Sha256::new();
    let mut output = std::io::BufWriter::with_capacity(
        CHUNK,
        std::fs::File::create(dst).map_err(|e| format!("create output: {e}"))?,
    );

    let mut buf = vec![0u8; CHUNK];
    let mut remaining = ciphertext_len;
    while remaining > 0 {
        let n = remaining.min(CHUNK as u64) as usize;
        let chunk = &mut buf[..n];
        input.read_exact(chunk).map_err(|e| format!("read download: {e}"))?;
        source_hash.update(&*chunk);
        tag_hash.update_padded(chunk);
        ctr.apply_keystream(chunk);
        output_hash.update(&*chunk);
        output.write_all(chunk).map_err(|e| format!("write output: {e}"))?;
        remaining -= n as u64;
        if !report(ciphertext_len - remaining, ciphertext_len, &mut reported, progress) {
            return Err(CANCELLED.to_string());
        }
    }

    let mut tag = [0u8; 16];
    input.read_exact(&mut tag).map_err(|e| format!("read download: {e}"))?;
    source_hash.update(tag);

    let expected = gcm_tag(tag_hash, tag_mask, ciphertext_len);
    // Every byte compared, whatever the first difference.
    let mismatch = expected.iter().zip(tag).fold(0u8, |acc, (e, t)| acc | (e ^ t));
    if mismatch != 0 {
        return Err("Decryption failed: aead::Error (authentication tag mismatch)".to_string());
    }

    let file = output.into_inner().map_err(|e| format!("write output: {e}"))?;
    file.sync_all().map_err(|e| format!("sync output: {e}"))?;
    Ok(StreamedFile {
        source_sha256: hex32(&source_hash.finalize()),
        output_sha256: hex32(&output_hash.finalize()),
        output_len: ciphertext_len,
    })
}

/// SHA-256 of a file, read a chunk at a time.
pub fn hash_file(path: &Path) -> Result<(String, u64), String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((hex32(&hasher.finalize()), len))
}

/// Put a verified download into the download directory under `name`: an identical
/// file already there (same name, size and hash) is reused and `src` dropped;
/// otherwise `src` moves to a free spelling of the name. Returns where it landed.
pub fn place_download(src: &Path, file_hash: &str, len: u64, name: &str, extension: &str) -> Result<std::path::PathBuf, String> {
    let dir = crate::db::get_download_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;
    let target_name = if name.is_empty() { format!("{}.{}", file_hash, extension) } else { name.to_string() };
    // One plain component, or nothing: the name and extension come from the sender.
    let mut parts = Path::new(&target_name).components();
    if !matches!((parts.next(), parts.next()), (Some(std::path::Component::Normal(_)), None))
        || target_name.contains(['/', '\\'])
    {
        return Err("Refusing a download name outside the download folder".to_string());
    }

    let candidate = dir.join(&target_name);
    let identical = std::fs::metadata(&candidate).map(|m| m.len() == len).unwrap_or(false)
        && hash_file(&candidate).map(|(h, _)| h == file_hash).unwrap_or(false);
    if identical {
        let _ = std::fs::remove_file(src);
        return Ok(candidate);
    }

    let dest = crate::crypto::resolve_unique_filename(&dir, &target_name);
    // A rename can't cross filesystems (app storage to shared storage on Android):
    // copy beside the destination, then rename that into place.
    if std::fs::rename(src, &dest).is_err() {
        let staging = dir.join(format!(".{}.{}.tmp", file_hash, extension));
        let copied = std::fs::copy(src, &staging)
            .map_err(|e| format!("Failed to write file: {}", e))
            .and_then(|_| std::fs::File::open(&staging).and_then(|f| f.sync_all()).map_err(|e| format!("Failed to write file: {}", e)))
            .and_then(|_| std::fs::rename(&staging, &dest).map_err(|e| format!("Failed to rename file: {}", e)));
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&staging);
            return Err(e);
        }
        let _ = std::fs::remove_file(src);
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vector-stream-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_download_never_lands_outside_its_folder() {
        let dir = scratch("escape");
        let src = dir.join("plain");
        std::fs::write(&src, b"x").unwrap();
        for bad in ["..\\..\\evil.bat", "../evil", "sub/evil", ".."] {
            assert!(place_download(&src, "abc", 1, bad, "bin").is_err(), "{bad}");
        }
        assert!(place_download(&src, "abc", 1, "", "..\\evil").is_err());
    }

    fn roundtrip(len: usize) {
        let dir = scratch(&format!("rt{len}"));
        let plain: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let params = crate::crypto::generate_encryption_params();
        let sealed = crate::crypto::encrypt_data(&plain, &params).unwrap();
        let (src, dst) = (dir.join("in"), dir.join("out"));
        std::fs::write(&src, &sealed).unwrap();

        let got = decrypt_file(&src, &dst, &params.key, &params.nonce).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), plain, "same plaintext as the one-shot cipher at {len} bytes");
        assert_eq!(got.output_sha256, crate::crypto::sha256_hex(&plain));
        assert_eq!(got.source_sha256, crate::crypto::sha256_hex(&sealed));
        assert_eq!(got.output_len, len as u64);
    }

    #[test]
    fn it_matches_the_one_shot_cipher_across_chunk_and_block_edges() {
        for len in [0, 1, 15, 16, 17, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK + 7] {
            roundtrip(len);
        }
    }

    #[test]
    fn a_streamed_seal_opens_with_the_one_shot_cipher() {
        for len in [0usize, 1, 16, 17, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK + 7] {
            let dir = scratch(&format!("enc{len}"));
            let plain: Vec<u8> = (0..len).map(|i| (i * 7 % 253) as u8).collect();
            let (src, sealed) = (dir.join("plain"), dir.join("sealed"));
            std::fs::write(&src, &plain).unwrap();
            let params = crate::crypto::generate_encryption_params();
            let (hash, sealed_len) = encrypt_file(&src, &sealed, &params.key, &params.nonce).unwrap();
            let blob = std::fs::read(&sealed).unwrap();
            assert_eq!(blob, crate::crypto::encrypt_data(&plain, &params).unwrap(), "byte for byte at {len} bytes");
            assert_eq!((hash, sealed_len), (crate::crypto::sha256_hex(&blob), blob.len() as u64));
        }
    }

    #[test]
    fn a_stopped_pass_ends_early_and_leaves_nothing() {
        let dir = scratch("stop");
        let (src, sealed, opened) = (dir.join("plain"), dir.join("sealed"), dir.join("opened"));
        std::fs::write(&src, vec![3u8; 4 * CHUNK]).unwrap();
        let params = crate::crypto::generate_encryption_params();
        let mut seen = 0;
        let err = encrypt_file_with_progress(&src, &sealed, &params.key, &params.nonce, &mut |pct| { seen = pct; pct < 25 }).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(seen < 100 && !sealed.exists(), "stopped at {seen}%, nothing left behind");

        encrypt_file(&src, &sealed, &params.key, &params.nonce).unwrap();
        let err = decrypt_file_with_progress(&sealed, &opened, &params.key, &params.nonce, &mut |pct| pct < 50).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!opened.exists(), "no half-opened plaintext");
    }

    #[test]
    fn a_flipped_byte_fails_and_leaves_no_plaintext() {
        let dir = scratch("tamper");
        let params = crate::crypto::generate_encryption_params();
        let mut sealed = crate::crypto::encrypt_data(&vec![7u8; 5000], &params).unwrap();
        sealed[2500] ^= 1;
        let (src, dst) = (dir.join("in"), dir.join("out"));
        std::fs::write(&src, &sealed).unwrap();
        let err = decrypt_file(&src, &dst, &params.key, &params.nonce).unwrap_err();
        assert!(err.contains("aead"), "reads as a decryption failure: {err}");
        assert!(!dst.exists(), "unauthenticated plaintext must not survive");
    }

    #[test]
    fn a_wrong_key_fails() {
        let dir = scratch("key");
        let params = crate::crypto::generate_encryption_params();
        let sealed = crate::crypto::encrypt_data(b"hello", &params).unwrap();
        let (src, dst) = (dir.join("in"), dir.join("out"));
        std::fs::write(&src, &sealed).unwrap();
        let other = crate::crypto::generate_encryption_params();
        assert!(decrypt_file(&src, &dst, &other.key, &params.nonce).is_err());
    }

    #[test]
    fn hash_file_matches_the_in_memory_hash() {
        let dir = scratch("hash");
        let data: Vec<u8> = (0..(CHUNK + 5)).map(|i| i as u8).collect();
        std::fs::write(dir.join("f"), &data).unwrap();
        assert_eq!(hash_file(&dir.join("f")).unwrap(), (crate::crypto::sha256_hex(&data), data.len() as u64));
    }
}
