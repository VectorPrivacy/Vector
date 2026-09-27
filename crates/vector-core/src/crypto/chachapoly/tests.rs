use super::*;
use rand::{rngs::StdRng, Rng, RngCore, SeedableRng};

fn backends() -> Vec<Backend> {
    Backend::available().into_iter().map(|(_, b)| b).collect()
}

fn reference_seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], pt: &[u8]) -> (Vec<u8>, [u8; 16]) {
    use chacha20poly1305::{AeadInPlace, KeyInit};
    let c = chacha20poly1305::ChaCha20Poly1305::new(key.into());
    let mut buf = pt.to_vec();
    let tag = c.encrypt_in_place_detached(nonce.into(), aad, &mut buf).unwrap();
    (buf, tag.into())
}

#[test]
fn rfc8439_2_8_2() {
    let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let aad = [0x50, 0x51, 0x52, 0x53, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7];
    let key: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
    let nonce = [0x07, 0, 0, 0, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47];
    let tag_expect = [
        0x1a, 0xe1, 0x0b, 0x59, 0x4f, 0x09, 0xe2, 0x6a, 0x7e, 0x90, 0x2e, 0xcb, 0xd0, 0x60, 0x06, 0x91,
    ];
    let ct_head = [0xd3, 0x1a, 0x8d, 0x34, 0x64, 0x8e, 0x60, 0xdb, 0x7b, 0x86, 0xaf, 0xbc, 0x53, 0xef, 0x7e, 0xc2];
    let ct_tail = [0x61, 0x16]; // last two bytes of the RFC ciphertext
    for b in backends() {
        let c = ChaCha20Poly1305::with_backend(&key, b);
        let mut buf = pt.to_vec();
        let tag = c.seal_in_place(&nonce, &aad, &mut buf).unwrap();
        assert_eq!(tag, tag_expect, "{b:?}");
        assert_eq!(&buf[..16], &ct_head, "{b:?}");
        assert_eq!(&buf[buf.len() - 2..], &ct_tail, "{b:?}");
        c.open_in_place(&nonce, &aad, &mut buf, &tag).unwrap();
        assert_eq!(&buf[..], &pt[..], "{b:?}");
    }
}

#[test]
fn matches_reference_every_length_every_backend() {
    let mut rng = StdRng::seed_from_u64(0x5eed);
    for len in 0..=1100usize {
        let key: [u8; 32] = rng.gen();
        let nonce: [u8; 12] = rng.gen();
        let aad_len = if len % 3 == 0 { 0 } else { rng.gen_range(0..40) };
        let mut aad = vec![0u8; aad_len];
        rng.fill_bytes(&mut aad);
        let mut pt = vec![0u8; len];
        rng.fill_bytes(&mut pt);
        let (ct_ref, tag_ref) = reference_seal(&key, &nonce, &aad, &pt);
        for b in backends() {
            let c = ChaCha20Poly1305::with_backend(&key, b);
            let mut buf = pt.clone();
            let tag = c.seal_in_place(&nonce, &aad, &mut buf).unwrap();
            assert_eq!(buf, ct_ref, "ct len={len} {b:?}");
            assert_eq!(tag, tag_ref, "tag len={len} {b:?}");
            c.open_in_place(&nonce, &aad, &mut buf, &tag).unwrap();
            assert_eq!(buf, pt, "roundtrip len={len} {b:?}");
        }
    }
}

#[test]
fn large_messages_match_reference() {
    let mut rng = StdRng::seed_from_u64(7);
    for &len in &[4096usize, 65_535, 65_536, 1 << 20] {
        let key: [u8; 32] = rng.gen();
        let nonce: [u8; 12] = rng.gen();
        let mut pt = vec![0u8; len];
        rng.fill_bytes(&mut pt);
        let (ct_ref, tag_ref) = reference_seal(&key, &nonce, b"", &pt);
        for b in backends() {
            let c = ChaCha20Poly1305::with_backend(&key, b);
            let mut buf = pt.clone();
            assert_eq!(c.seal_in_place(&nonce, b"", &mut buf).unwrap(), tag_ref, "{len} {b:?}");
            assert!(buf == ct_ref, "{len} {b:?}");
        }
    }
}

#[test]
fn poly1305_matches_reference_on_adversarial_inputs() {
    use ::poly1305::universal_hash::{KeyInit, UniversalHash};
    let mut rng = StdRng::seed_from_u64(99);
    let mut keys: Vec<[u8; 32]> = vec![[0xff; 32], [0; 32]];
    for _ in 0..200 {
        keys.push(rng.gen());
    }
    let mut msgs: Vec<Vec<u8>> = vec![vec![0xff; 16 * 64], vec![0; 160], vec![0xff; 15], vec![]];
    for _ in 0..200 {
        let mut m = vec![0u8; rng.gen_range(0..300)];
        rng.fill_bytes(&mut m);
        msgs.push(m);
    }
    for k in &keys {
        for m in &msgs {
            let mut theirs = ::poly1305::Poly1305::new(k.into());
            theirs.update_padded(m);
            let want = <[u8; 16]>::from(theirs.finalize());
            let mut r64 = super::poly1305::r64::Poly1305::new(k);
            r64.padded(m, usize::MAX);
            assert_eq!(r64.finish(), want, "radix 2^64");
            let mut r26 = super::poly1305::r26::Poly1305::new(k);
            r26.padded(m, usize::MAX);
            assert_eq!(r26.finish(), want, "radix 2^26");
        }
    }
}

#[test]
fn tampering_is_rejected_and_plaintext_withheld() {
    let key = [3u8; 32];
    let nonce = [9u8; 12];
    for b in backends() {
        let c = ChaCha20Poly1305::with_backend(&key, b);
        let pt = b"attack at dawn, bring snacks".to_vec();
        let mut ct = pt.clone();
        let tag = c.seal_in_place(&nonce, b"", &mut ct).unwrap();

        for i in 0..ct.len() {
            let mut t = ct.clone();
            t[i] ^= 1;
            let before = t.clone();
            assert_eq!(c.open_in_place(&nonce, b"", &mut t, &tag), Err(Error));
            assert_eq!(t, before, "failed open must not touch the buffer");
        }
        for i in 0..16 {
            let mut bad = tag;
            bad[i] ^= 0x80;
            let mut t = ct.clone();
            assert!(c.open_in_place(&nonce, b"", &mut t, &bad).is_err());
        }
        let mut t = ct.clone();
        assert!(c.open_in_place(&nonce, b"x", &mut t, &tag).is_err(), "aad is bound");
        let mut n2 = nonce;
        n2[0] ^= 1;
        let mut t = ct.clone();
        assert!(c.open_in_place(&n2, b"", &mut t, &tag).is_err(), "nonce is bound");
    }
}

#[test]
fn framed_open_matches_reference_layout() {
    let key = [5u8; 32];
    let nonce = [1u8; 12];
    let pt = "héllo wörld — framed".as_bytes();
    let (ct, tag) = reference_seal(&key, &nonce, b"", pt);
    let mut framed = nonce.to_vec();
    framed.extend_from_slice(&ct);
    framed.extend_from_slice(&tag);
    let c = ChaCha20Poly1305::new(&key);
    assert_eq!(c.open_framed_in_place(&mut framed).unwrap(), pt);
    assert!(c.open_framed_in_place(&mut [0u8; 27]).is_err());
}

#[test]
fn wipe_clears_every_byte_at_every_alignment() {
    for off in 0..9 {
        for len in 0..40 {
            let mut buf = [0xAAu8; 64];
            wipe(&mut buf[off..off + len]);
            assert!(buf[off..off + len].iter().all(|&b| b == 0));
            assert!(buf[..off].iter().chain(&buf[off + len..]).all(|&b| b == 0xAA), "wrote outside the slice");
        }
    }
    let mut v = Vec::with_capacity(100);
    v.extend_from_slice(&[7u8; 30]);
    wipe_vec(&mut v);
    assert!(v.is_empty());
    // SAFETY: wipe_vec zeroed the full capacity, so every byte is initialised.
    let spare = unsafe { std::slice::from_raw_parts(v.as_ptr(), v.capacity()) };
    assert!(spare.iter().all(|&b| b == 0));
}

#[cfg(target_arch = "x86_64")]
#[test]
fn simd_poly1305_matches_scalar_from_any_state() {
    if !super::poly1305::r64::simd_available() {
        return;
    }
    let mut rng = StdRng::seed_from_u64(0xa5a5);
    for trial in 0..400 {
        let key: [u8; 32] = if trial < 4 { [0xff; 32] } else { rng.gen() };
        let prefix = 16 * rng.gen_range(0..6);
        let groups = rng.gen_range(1..20);
        let mut data = vec![0xffu8; prefix + 64 * groups];
        if trial % 3 != 0 {
            rng.fill_bytes(&mut data);
        }
        let mut serial = super::poly1305::r64::Poly1305::new(&key);
        serial.blocks(&data);
        let mut lanes = super::poly1305::r64::Poly1305::new(&key);
        lanes.blocks(&data[..prefix]);
        // SAFETY: lanes available (checked above); the remainder is a nonzero multiple of 64.
        unsafe { lanes.blocks_simd(&data[prefix..]) };
        assert_eq!(serial.finish(), lanes.finish(), "trial {trial} prefix {prefix} groups {groups}");
    }
}

#[test]
fn matches_reference_with_poly_lanes_forced_on() {
    let mut rng = StdRng::seed_from_u64(0x1a2e);
    for len in (0..=1100usize).step_by(7).chain([4096, 65_536]) {
        let key: [u8; 32] = rng.gen();
        let nonce: [u8; 12] = rng.gen();
        let mut pt = vec![0u8; len];
        rng.fill_bytes(&mut pt);
        let (ct_ref, tag_ref) = reference_seal(&key, &nonce, b"", &pt);
        for (name, _) in Backend::available() {
            let c = super::bench::cipher(&key, name, 64).unwrap();
            let mut buf = pt.clone();
            let tag = c.seal_in_place(&nonce, &[], &mut buf).unwrap();
            assert!(buf == ct_ref && tag == tag_ref, "len={len} {name}");
            c.open_in_place(&nonce, &[], &mut buf, &tag).unwrap();
            assert!(buf == pt, "roundtrip len={len} {name}");
        }
    }
}

#[test]
fn two_block_scalar_matches_single_blocks() {
    let st = State { key: [0x0123_4567, 0x89ab_cdef, 7, 8, 9, 10, 11, 12], nonce: [1, 2, 3] };
    for counter in [0u32, 1, 41, u32::MAX - 1] {
        let mut pair = [0u8; 128];
        super::chacha::pair_blocks(&st, counter, &mut pair);
        let mut single = [0u8; super::chacha::BUF];
        Backend::Portable.keystream(&st, counter, 2, &mut single);
        assert_eq!(&pair[..], &single[..128], "counter {counter}");
    }
}
