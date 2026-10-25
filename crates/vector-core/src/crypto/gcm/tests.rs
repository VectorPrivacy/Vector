use super::*;
use aes_gcm::aead::consts::U16;
use aes_gcm::{AeadInOut, AesGcm};

fn reference(key: &[u8; 32], nonce: &[u8; 16], pt: &[u8]) -> (Vec<u8>, [u8; 16]) {
    let c = AesGcm::<Aes256, U16>::new_from_slice(key).unwrap();
    let mut buf = pt.to_vec();
    let tag = c
        .encrypt_inout_detached(&aes_gcm::Nonce::<U16>::from(*nonce), &[], buf.as_mut_slice().into())
        .unwrap();
    (buf, tag.into())
}

fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    }
}

fn engines() -> Vec<&'static str> {
    let mut v = vec!["generic"];
    if bench::stitched_present() {
        v.push("stitched");
    }
    v
}

#[test]
fn every_engine_matches_aes_gcm_at_every_length() {
    let mut r = rng(0x9e37_79b9_7f4a_7c15);
    for len in (0..=2100usize).chain([4096, 65_536, 1 << 20, (1 << 20) + 13]) {
        let key: [u8; 32] = core::array::from_fn(|_| r() as u8);
        let nonce: [u8; 16] = core::array::from_fn(|_| r() as u8);
        let pt: Vec<u8> = (0..len).map(|_| r() as u8).collect();
        let (ct_ref, tag_ref) = reference(&key, &nonce, &pt);
        for engine in engines() {
            let mut buf = pt.clone();
            let mut s = bench::stream(engine, &key, &nonce).unwrap();
            s.encrypt(&mut buf);
            assert!(buf == ct_ref, "{engine} ct len={len}");
            assert_eq!(s.finish(), tag_ref, "{engine} tag len={len}");
            let mut s = bench::stream(engine, &key, &nonce).unwrap();
            s.decrypt(&mut buf);
            assert!(buf == pt, "{engine} pt len={len}");
            assert_eq!(s.finish(), tag_ref, "{engine} open tag len={len}");
        }
        // And whichever engine production picks for this length.
        let mut buf = pt.clone();
        assert_eq!(seal(&key, &nonce, &mut buf), tag_ref, "seal len={len}");
        assert!(buf == ct_ref, "seal ct len={len}");
        open(&key, &nonce, &mut buf, &tag_ref).unwrap();
        assert!(buf == pt, "open len={len}");
    }
}

#[test]
fn chunked_streams_match_one_shot() {
    let mut r = rng(42);
    for trial in 0..300 {
        let key: [u8; 32] = core::array::from_fn(|_| r() as u8);
        let nonce: [u8; 16] = core::array::from_fn(|_| r() as u8);
        let len = (r() % 5000) as usize;
        let pt: Vec<u8> = (0..len).map(|_| r() as u8).collect();
        let (ct_ref, tag_ref) = reference(&key, &nonce, &pt);
        for engine in engines() {
            for decrypting in [false, true] {
                let mut buf = if decrypting { ct_ref.clone() } else { pt.clone() };
                let mut s = bench::stream(engine, &key, &nonce).unwrap();
                let mut at = 0;
                while at < len {
                    let step = (16 * (r() % 40) as usize).max(16).min(len - at);
                    let chunk = &mut buf[at..at + step];
                    if decrypting {
                        s.decrypt(chunk)
                    } else {
                        s.encrypt(chunk)
                    }
                    at += step;
                }
                assert_eq!(s.finish(), tag_ref, "{engine} trial {trial} decrypting={decrypting}");
                assert!(buf == if decrypting { pt.clone() } else { ct_ref.clone() }, "{engine} trial {trial}");
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn stitched_counter_wraps_like_ctr32be() {
    if !bench::stitched_present() {
        return;
    }
    let key = [3u8; 32];
    for start in [u32::MAX - 20, u32::MAX - 7, u32::MAX, 0] {
        let mut block = [0x5au8; 16];
        block[12..].copy_from_slice(&start.to_be_bytes());
        for len in [16usize, 128, 200, 1024 + 5] {
            let pt: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let mut want = pt.clone();
            ctr::Ctr32BE::<Aes256>::new(&key.into(), &block.into()).apply_keystream(&mut want);
            let mut s = Stream::stitched(&key, &[0u8; 16]);
            let Engine::Stitched(gcm) = &mut s.engine else { unreachable!() };
            // SAFETY: the stitched engine exists, so the instructions are present.
            unsafe { gcm.seek(block) };
            let mut got = pt.clone();
            s.encrypt(&mut got);
            assert!(got == want, "start {start:#x} len {len}");
        }
    }
}

#[test]
fn a_bad_tag_is_refused_and_leaves_no_plaintext() {
    let key = [9u8; 32];
    let nonce = [4u8; 16];
    for len in [0usize, 15, 127, 128, 129, 4096] {
        let pt: Vec<u8> = (0..len).map(|i| (i as u8) | 1).collect();
        let (ct, tag) = reference(&key, &nonce, &pt);
        for flip in [0usize, 15] {
            let mut bad = tag;
            bad[flip] ^= 0x40;
            let mut buf = ct.clone();
            assert!(open(&key, &nonce, &mut buf, &bad).is_err(), "len {len}");
            assert!(buf.iter().all(|&b| b == 0), "len {len}: plaintext left behind");
        }
        if len > 0 {
            let mut buf = ct.clone();
            buf[len / 2] ^= 1;
            assert!(open(&key, &nonce, &mut buf, &tag).is_err(), "len {len}: tampered ciphertext");
        }
        let mut other = nonce;
        other[15] ^= 1;
        let mut buf = ct.clone();
        assert!(open(&key, &other, &mut buf, &tag).is_err(), "len {len}: wrong nonce");
    }
}

#[test]
fn the_keystream_opens_any_range_of_a_sealed_message() {
    let key = [0x2cu8; 32];
    let nonce = [0x91u8; 16];
    let mut next = rng(0x5eed);
    for len in [0usize, 1, 15, 16, 17, 255, 4096, 70_001] {
        let pt: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
        let (ct, _) = reference(&key, &nonce, &pt);
        let mut ranges: Vec<(usize, usize)> = vec![(0, len), (0, 0), (len, len)];
        for edge in [15usize, 16, 17, 32] {
            if edge <= len {
                ranges.push((edge, len));
                ranges.push((0, edge));
            }
        }
        for _ in 0..40 {
            if len == 0 {
                break;
            }
            let a = (next() as usize) % len;
            let b = a + (next() as usize) % (len - a + 1);
            ranges.push((a, b));
        }
        let mut keystream = Keystream::new(&key, &nonce);
        for (a, b) in ranges {
            let mut part = ct[a..b].to_vec();
            keystream.apply_at(a as u64, &mut part).unwrap();
            assert!(part == pt[a..b], "len {len} range {a}..{b}");
        }
    }
}

#[test]
fn the_keystream_wraps_its_counter_like_a_whole_pass() {
    let key = [7u8; 32];
    for start in [u32::MAX - 20, u32::MAX - 1, u32::MAX, 0] {
        let mut j0 = ghash::Block::default();
        j0[..12].copy_from_slice(&[0xa5; 12]);
        j0[12..].copy_from_slice(&start.to_be_bytes());
        let mut whole = vec![0u8; 16 + 1024];
        ctr::Ctr32BE::<Aes256>::new(&key.into(), &j0).apply_keystream(&mut whole);
        for (offset, len) in [(0usize, 1024usize), (1, 31), (15, 2), (16, 16), (300, 700), (1023, 1)] {
            let mut got = vec![0u8; len];
            Keystream::from_j0(&key, &j0).apply_at(offset as u64, &mut got).unwrap();
            assert!(got == whole[16 + offset..16 + offset + len], "start {start:#x} at {offset}+{len}");
        }
    }
}

#[test]
fn the_keystream_refuses_to_run_past_the_longest_message() {
    let mut keystream = Keystream::new(&[1u8; 32], &[2u8; 16]);
    assert_eq!(keystream.apply_at(u64::MAX, &mut [0u8; 1]), Err(PastTheEnd));
    assert_eq!(keystream.apply_at(1 << 40, &mut [0u8; 1]), Err(PastTheEnd));
    assert!(keystream.apply_at(5, &mut [0u8; 4]).is_ok(), "and still works afterwards");
}
