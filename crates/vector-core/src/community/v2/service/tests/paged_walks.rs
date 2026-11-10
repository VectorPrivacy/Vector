//! Walks over relays that hold different amounts of a plane: a relay missing some events fills
//! its page from further down, and what only the fuller relay holds in between must still be read.

use super::*;

const FULL: &str = "wss://full";
const SPARSE: &str = "wss://sparse";
const FILLER: u64 = 700;
const BASE: u64 = 10_000;

fn both() -> Vec<String> {
    vec![FULL.to_string(), SPARSE.to_string()]
}

fn only(relay: &str) -> Vec<String> {
    vec![relay.to_string()]
}

/// An edition no fold admits, taking one slot of the Control Plane at `secs`.
fn filler(community: &CommunityV2, signer: &Keys, i: u64, secs: u64) -> Event {
    let group = control::ControlPlane::of(community).write_group().unwrap();
    let content = format!("{{\"name\":\"filler{i}\",\"private\":false}}");
    let rumor = control::build_edition_rumor(signer.public_key(), vsk::CHANNEL_METADATA, &[0xAB; 32], 1, None, &content, secs, None);
    control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(secs)).unwrap().0
}

struct Bed {
    _tmp: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    owner: Keys,
    relay: MemoryRelay,
    community: CommunityV2,
    rogue: Keys,
}

/// Whether the sparse relay holds entry `i` of 700. The hundred it lacks high up make its first
/// page of 500 end a hundred further down than the full relay's, and the ten it lacks in between
/// are held by the full relay alone, below where its own page ended.
fn on_sparse(i: u64) -> bool {
    !(300..400).contains(&i) && !(150..160).contains(&i)
}

/// A community on two relays under 700 filler editions a second apart.
async fn bed() -> Bed {
    let (tmp, guard, owner) = init_test_db();
    let relay = MemoryRelay::new();
    let community = create_community(&relay, "Walks", both(), None).await.unwrap();
    let rogue = Keys::generate();
    for i in 0..FILLER {
        let wrap = filler(&community, &rogue, i, BASE + i);
        relay.inject(&wrap, &only(FULL));
        if on_sparse(i) {
            relay.inject(&wrap, &only(SPARSE));
        }
    }
    Bed { _tmp: tmp, _guard: guard, owner, relay, community, rogue }
}

fn fillers(editions: &[ParsedEdition], rogue: &Keys) -> usize {
    editions.iter().filter(|e| e.author == rogue.public_key()).count()
}

#[tokio::test]
async fn a_whole_read_holds_every_edition_when_one_relay_holds_fewer() {
    let b = bed().await;
    let editions = fetch_control_plane_whole(&b.relay, &b.community).await.expect("read whole");
    assert_eq!(fillers(&editions, &b.rogue), FILLER as usize);
}

#[tokio::test]
async fn a_follow_reads_what_only_the_fuller_relay_holds() {
    let b = bed().await;
    let read = read_control(&b.relay, &b.community).await.unwrap();
    assert_eq!(fillers(&read.editions, &b.rogue), FILLER as usize);
    assert!(!read.truncated);
}

#[tokio::test]
async fn a_refounding_carries_a_head_only_the_fuller_relay_holds() {
    let b = bed().await;
    // A first Banlist from the owner's other device, below where the full relay's page ended.
    let banned = Keys::generate().public_key().to_hex();
    let group = control::ControlPlane::of(&b.community).write_group().unwrap();
    let eid = crate::community::v2::derive::banlist_locator(b.community.id());
    let content = crate::community::v2::roles::banlist_content_json(std::slice::from_ref(&banned)).unwrap();
    let rumor = control::build_edition_rumor(b.owner.public_key(), vsk::BANLIST, &eid, 1, None, &content, BASE + 150, None);
    let (wrap, _) = control::seal_control_edition(&rumor, &group, &b.owner, Timestamp::from_secs(BASE + 150)).unwrap();
    b.relay.inject(&wrap, &only(FULL));

    let refounded = refound_community(&b.relay, &b.community, &[]).await.unwrap();
    let carried = fetch_control_plane_whole(&b.relay, &refounded).await.expect("the new plane reads whole");
    assert!(carried.iter().any(|e| e.vsk == vsk::BANLIST && e.content.contains(&banned)), "the Banlist crosses to the new epoch");
}

#[tokio::test]
async fn joining_finds_the_genesis_under_another_relays_older_wraps() {
    let (_tmp, _guard, _owner) = init_test_db();
    let relay = MemoryRelay::new();
    // Founded a while back, so wraps can be dated either side of the genesis.
    crate::clock::shift_for_test(-30 * 86_400_000);
    let founded = create_community(&relay, "Deep", only(FULL), None).await.unwrap();
    let genesis = crate::clock::now_secs();
    crate::clock::shift_for_test(0);
    let rogue = Keys::generate();
    // The founding relay holds a page and more above the genesis...
    for i in 0..600u64 {
        relay.inject(&filler(&founded, &rogue, i, genesis + 10 + i), &only(FULL));
    }
    // ...and a relay that never held the genesis holds a page dated before it.
    for i in 0..500u64 {
        relay.inject(&filler(&founded, &rogue, 1_000 + i, genesis - 100_000 + i), &only(SPARSE));
    }
    let asked = CommunityV2 { relays: both(), ..founded.clone() };
    let verified = verify_owner_root_and_reconcile(&relay, asked).await;
    assert!(verified.is_ok(), "the walk reaches the genesis: {:?}", verified.err());
}

#[tokio::test]
async fn the_history_walk_reads_what_only_the_fuller_relay_holds() {
    let (_tmp, _guard, owner) = init_test_db();
    let relay = MemoryRelay::new();
    let community = create_community(&relay, "History", both(), None).await.unwrap();
    let general = community.channels[0].id;
    let group = channel_group_key(&community.community_root, &general, community.root_epoch);
    for i in 0..250u64 {
        let at = BASE + i;
        let rumor = chat::build_message_rumor(owner.public_key(), &general, community.root_epoch, &format!("msg {i}"), None, &[], vec![], at * 1000);
        let (wrap, _) = chat::seal_chat_rumor(&rumor, &group, &owner, Timestamp::from_secs(at), false).unwrap();
        relay.inject(&wrap, &only(FULL));
        if !(100..150).contains(&i) {
            relay.inject(&wrap, &only(SPARSE));
        }
    }
    let all = fetch_channel_history(&relay, &community, &general, 50, 16, None, None, crate::community::transport::Evidence::Quorum, |_, _| true, |_| Default::default()).await.unwrap();
    assert_eq!(all.len(), 250);
}

#[tokio::test]
async fn a_rotation_only_the_fuller_relay_holds_is_found_under_a_flood() {
    let (_tmp, _guard, owner) = init_test_db();
    let relay = MemoryRelay::new();
    let community = create_community(&relay, "Rotated", both(), None).await.unwrap();
    let (new_root, new_epoch) = ([0xD9; 32], Epoch(1));
    let group = base_rekey_group_key(&community.community_root, community.id(), new_epoch);
    let prev_commit = super::super::super::derive::epoch_key_commitment(Epoch(0), &community.community_root);
    let rogue = Keys::generate();
    let junk = |i: u64, secs: u64| {
        let blob = rekey::build_blob_local(rogue.secret_key(), &rogue.public_key().to_bytes(), &rogue.public_key(), RekeyScope::Root, new_epoch, &[0xEE; 32]).unwrap();
        let rumor = rekey::build_rekey_rumor(rogue.public_key(), RekeyScope::Root, new_epoch, Epoch(0), &prev_commit, &[blob], 1, 1, secs + i, None, false).unwrap();
        rekey::seal_rekey_chunk(&rumor, &group, &rogue, Timestamp::from_secs(secs + i)).unwrap().0
    };

    // The owner's rotation, on the full relay alone.
    let blob = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &owner.public_key(), RekeyScope::Root, new_epoch, &new_root).unwrap();
    let genuine = rekey::build_rekey_chunks_local(&owner, &group, RekeyScope::Root, new_epoch, Epoch(0), &prev_commit, &[blob], 50_000_000, None, false).unwrap();
    let dated = genuine[0].created_at.as_secs();
    for e in &genuine {
        relay.inject(e, &only(FULL));
    }
    // A page of junk above it there, and a page dated below it on the other relay.
    for i in 0..260u64 {
        relay.inject(&junk(i, dated + 1_000), &only(FULL));
    }
    for i in 0..200u64 {
        relay.inject(&junk(i, dated - 2_000), &only(SPARSE));
    }

    let session = crate::db::current_session();
    let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("the rotation is found");
    assert_eq!((updated.root_epoch, updated.community_root), (Epoch(1), new_root));
}

#[tokio::test]
async fn the_memberlist_holds_a_join_only_the_fuller_relay_holds() {
    let (_tmp, _guard, _owner) = init_test_db();
    let relay = MemoryRelay::new();
    let community = create_community(&relay, "Members", both(), None).await.unwrap();
    let gb = crate::community::v2::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let mut joined = Vec::new();
    for i in 0..FILLER {
        let k = Keys::generate();
        let join = guestbook::build_join_rumor(k.public_key(), None, (BASE + i) * 1000);
        let (wrap, _) = guestbook::seal_guestbook_rumor(&join, &gb, &k, Timestamp::from_secs(BASE + i)).unwrap();
        relay.inject(&wrap, &only(FULL));
        if on_sparse(i) {
            relay.inject(&wrap, &only(SPARSE));
        }
        joined.push(k.public_key());
    }
    let members: std::collections::HashSet<PublicKey> = memberlist(&relay, &community).await.unwrap().into_iter().collect();
    let missing = joined.iter().filter(|m| !members.contains(m)).count();
    assert_eq!(missing, 0, "every Join is read, whichever relay holds it");
}
