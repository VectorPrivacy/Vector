//! Community reads with Local Encryption on: loading a community and the moderation window.

use std::hint::black_box;
use std::time::Instant;

use nostr_sdk::prelude::*;
use vector_core::community::v2::community::CommunityV2;
use vector_core::community::ChannelId;
use vector_core::db::community::{community_policy_messages, community_recent_texts, load_community_v2, save_community_v2};
use vector_core::state::{set_encryption_enabled, ENCRYPTION_KEY};

/// An unsaved v2 community fresh from genesis, holding one Public `#general` channel.
fn fixture(name: &str) -> CommunityV2 {
    let owner = Keys::generate();
    let metadata = vector_core::community::v2::control::CommunityMetadata { name: name.into(), ..Default::default() };
    let g = vector_core::community::v2::control::genesis(&owner, metadata, 1_000).unwrap();
    CommunityV2::from_genesis(&g, name, None, vec!["wss://r".into()], 1_000)
}

fn encrypt_at_rest() {
    ENCRYPTION_KEY.set([0x55u8; 32], &[]);
    set_encryption_enabled(true);
}

fn decrypt_at_rest() {
    set_encryption_enabled(false);
    ENCRYPTION_KEY.clear(&[]);
}

pub fn load_encrypted() {
    crate::fresh_account();
    encrypt_at_rest();

    let mut v2 = fixture("V2");
    v2.description = Some("about".into());
    for i in 0..7 {
        let mut ch = v2.channels[0].clone();
        ch.id = ChannelId([i as u8 + 0x40; 32]);
        ch.name = format!("channel {i}");
        v2.channels.push(ch);
    }
    save_community_v2(&v2).unwrap();

    let time = |name: &str, f: &mut dyn FnMut()| {
        for _ in 0..20 { f(); }
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            for _ in 0..200 { f(); }
            best = best.min(t.elapsed().as_nanos() as f64 / 200.0);
        }
        println!("{name:<44} {:>9.1} µs", best / 1000.0);
    };
    time("vault read", &mut || { ENCRYPTION_KEY.with_key(|k| black_box(k[0])); });
    time("load_community_v2, 8 channels, encrypted", &mut || {
        black_box(load_community_v2(v2.id()).unwrap().unwrap());
    });

    decrypt_at_rest();
}

pub fn policy_window() {
    crate::fresh_account();
    encrypt_at_rest();
    let c = fixture("HQ");
    save_community_v2(&c).unwrap();
    let (cid, ch) = (c.id().to_hex(), c.channels[0].id.to_hex());
    let authors: Vec<String> = (0..7).map(|_| Keys::generate().public_key().to_bech32().unwrap()).collect();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    for i in 0..2000usize {
        let msg = vector_core::Message {
            id: format!("{i:064x}"),
            content: format!("message number {i}, a few words long"),
            at: 1_700_000_000_000 + i as u64 * 1000,
            npub: Some(authors[i % 7].clone()),
            ..Default::default()
        };
        rt.block_on(vector_core::db::events::save_message(&ch, &msg)).unwrap();
    }

    for name in ["community_policy_messages, 2000 rows", "community_recent_texts, 2000 rows"] {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            if name.starts_with("community_policy") {
                black_box(community_policy_messages(&cid, 4000).unwrap());
            } else {
                black_box(community_recent_texts(&cid, 4000).unwrap());
            }
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("{name:<44} {best:>9.2} ms");
    }
    decrypt_at_rest();
}
