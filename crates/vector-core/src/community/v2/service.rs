//! Concord v2 service — the stateful orchestration binding the pure v2 modules
//! to storage + transport. Free functions, `SessionGuard`-gated at every write
//! (a `swap_session` can land at any await — see CLAUDE.md), mirroring the v1
//! service's discipline.
//!
//! First-cut scope (bots): create a community, send + fetch channel messages,
//! accept invites, publish a Guestbook Join. Rotation/refounding/moderation and
//! full roster folding layer on next. Signing is local-keys for now (bots hold
//! their nsec); a NIP-46 bunker create/send path is a documented follow-up (v2's
//! genesis + chat seals are sign-only ops, so it composes — just needs the async
//! signer threaded through `build_seal`).

use nostr_sdk::prelude::{Keys, Timestamp};

use super::super::transport::{Query, Transport};
use super::super::{ChannelId, Epoch};
use super::chat::{self, ChatEvent};
use super::community::CommunityV2;
use super::control;
use super::derive::channel_group_key;
use super::guestbook;
use super::stream;
use crate::state::SessionGuard;

/// The local identity keys, or an error if none is installed. First-cut signing
/// path (a bunker account routes through the async signer — a follow-up).
fn local_keys() -> Result<Keys, String> {
    crate::state::MY_SECRET_KEY
        .to_keys()
        .ok_or_else(|| "Concord v2 needs a local identity key (bunker create/send is not wired yet)".to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Create a fresh v2 community owned by the local identity: mint the genesis
/// (self-certifying id + the two owner editions), persist, publish the genesis
/// control editions, and announce the owner's Guestbook Join. Returns the saved
/// community.
pub async fn create_community<T: Transport + ?Sized>(
    transport: &T,
    name: &str,
    relays: Vec<String>,
    description: Option<String>,
) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let owner = local_keys()?;
    let at_ms = now_ms();

    let meta = control::CommunityMetadata {
        name: name.to_string(),
        description: description.clone(),
        relays: relays.clone(),
        ..Default::default()
    };
    let genesis = control::genesis(&owner, meta, at_ms / 1000).map_err(|e| e.to_string())?;
    let community = CommunityV2::from_genesis(&genesis, name, description, relays.clone(), at_ms);

    // Save-before-publish (like v1 create): no peers exist yet so there's no
    // shared view to diverge from, and the fresh-random keys are irrecoverable
    // if a publish hiccup rolled them back. Re-check the session first — genesis
    // signing straddled no await here, but the DB write is the side effect.
    if !session.is_valid() {
        return Err("account changed during community creation".to_string());
    }
    crate::db::community::save_community_v2(&community)?;

    // Publish the two genesis control editions at the epoch-0 control plane.
    for wrap in &genesis.wraps {
        transport.publish(wrap, &community.relays).await?;
    }

    // Announce the owner's Guestbook Join so they appear in the memberlist.
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let join_rumor = guestbook::build_join_rumor(owner.public_key(), None, at_ms);
    if let Ok((join_wrap, _)) = guestbook::seal_guestbook_rumor(&join_rumor, &gb_group, &owner, Timestamp::from_secs(at_ms / 1000)) {
        let _ = transport.publish(&join_wrap, &community.relays).await;
    }

    Ok(community)
}

/// Send a text message to a channel. Derives the channel's Chat-Plane group key
/// (community_root for a Public channel, the channel key for a Private one),
/// seals it encrypted, and publishes. Returns the message's rumor id (hex).
pub async fn send_message<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    content: &str,
) -> Result<String, String> {
    let session = SessionGuard::capture();
    let author = local_keys()?;
    let ch = community.channel(channel_id).ok_or("no such channel in this community")?;
    let (secret, epoch) = community.channel_secret(ch);
    let group = channel_group_key(&secret, channel_id, epoch);

    let at_ms = now_ms();
    let rumor = chat::build_message_rumor(author.public_key(), channel_id, epoch, content, None, &[], vec![], at_ms);
    let rumor_id = rumor.id.ok_or("rumor has no id")?.to_hex();
    let (wrap, _ephemeral) = chat::seal_chat_rumor(&rumor, &group, &author, Timestamp::from_secs(at_ms / 1000), false)
        .map_err(|e| e.to_string())?;

    if !session.is_valid() {
        return Err("account changed before send".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    Ok(rumor_id)
}

/// A chat event opened from a channel fetch, tagged with the epoch its key
/// decrypted under.
pub struct FetchedEvent {
    pub event: ChatEvent,
    pub epoch: Epoch,
}

/// Fetch a channel's recent messages: query every held epoch's Chat-Plane
/// address, open + bind each, drop foreign/malformed, dedup by rumor id, and
/// order oldest→newest by the millisecond timestamp. `limit` bounds the newest
/// slice fetched from each address.
pub async fn fetch_channel<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    limit: usize,
) -> Result<Vec<FetchedEvent>, String> {
    let ch = community.channel(channel_id).ok_or("no such channel in this community")?;
    let coords = community.channel_read_coords(ch);

    // Address every held epoch by its Chat-Plane pubkey.
    let authors: Vec<String> = coords
        .iter()
        .map(|(secret, epoch)| channel_group_key(secret, channel_id, *epoch).pk_hex())
        .collect();
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: authors.clone(),
        limit: Some(limit),
        ..Default::default()
    };
    let wraps = transport.fetch(&query, &community.relays).await?;

    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(u64, FetchedEvent)> = Vec::new();
    for wrap in &wraps {
        // Select the epoch whose group key authored this wrap (no trial decrypt).
        for (secret, epoch) in &coords {
            let group = channel_group_key(secret, channel_id, *epoch);
            if wrap.pubkey != group.pk() {
                continue;
            }
            if let Ok(event) = chat::open_chat_event(wrap, &group, channel_id, *epoch) {
                let id = event.opened().rumor_id;
                if seen.insert(id) {
                    out.push((event.opened().at_ms, FetchedEvent { event, epoch: *epoch }));
                }
            }
            break;
        }
    }
    out.sort_by_key(|(ms, _)| *ms);
    Ok(out.into_iter().map(|(_, e)| e).collect())
}

#[cfg(test)]
mod tests {
    use super::super::super::transport::memory::MemoryRelay;
    use super::*;

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, Keys) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(50_000);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // A valid bech32-charset npub-shaped account dir name.
        const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut acct = String::from("npub1");
        let mut v = n as usize;
        for _ in 0..58 {
            acct.push(B[v % 32] as char);
            v = v / 32 + 7;
        }
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();
        let _ = crate::state::take_nostr_client();
        let owner = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&owner, &[]);
        crate::state::set_my_public_key(owner.public_key());
        (tmp, guard, owner)
    }

    #[tokio::test]
    async fn create_persists_and_reloads_a_v2_community() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let relays = vec!["wss://r".to_string()];

        let created = create_community(&relay, "Vectorville", relays.clone(), Some("hi".into())).await.unwrap();
        assert!(created.identity.verify());
        assert_eq!(created.owner().unwrap(), owner.public_key());
        assert_eq!(created.channels.len(), 1);

        // Protocol dispatch sees it as v2, and it reloads byte-faithfully.
        assert_eq!(
            crate::db::community::community_protocol(created.id()).unwrap(),
            Some(crate::community::ConcordProtocol::V2)
        );
        let loaded = crate::db::community::load_community_v2(created.id()).unwrap().expect("reloads");
        assert_eq!(loaded.name, "Vectorville");
        assert_eq!(loaded.community_root, created.community_root);
        assert_eq!(loaded.identity, created.identity);
        assert_eq!(loaded.channels[0].id.0, created.channels[0].id.0);
        assert!(!loaded.channels[0].private);

        // The genesis control editions + the owner Join landed on the relay.
        assert!(relay.count_on("wss://r") >= 3, "2 genesis editions + 1 guestbook join");
    }

    #[tokio::test]
    async fn owner_sends_and_reads_back_a_message() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Chat", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;

        let id1 = send_message(&relay, &community, &general, "hello world").await.unwrap();
        let id2 = send_message(&relay, &community, &general, "second message").await.unwrap();
        assert_ne!(id1, id2);

        let page = fetch_channel(&relay, &community, &general, 100).await.unwrap();
        let texts: Vec<String> = page
            .iter()
            .filter_map(|f| match &f.event {
                ChatEvent::Message { .. } => Some(f.event.opened().rumor.content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hello world", "second message"], "messages round-trip in ms order");
    }

    #[tokio::test]
    async fn a_second_member_reads_the_public_channel_from_the_root() {
        // A member who holds the community_root (via an invite bundle, modeled
        // here by cloning the community) reads the owner's public-channel message
        // — public channels need no key delivery, they derive from the root.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Public", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&relay, &community, &general, "everyone can read this").await.unwrap();

        // The "member" reconstructs the same read coordinates from the root.
        let member_view = community.clone();
        let page = fetch_channel(&relay, &member_view, &general, 100).await.unwrap();
        assert_eq!(page.len(), 1);
        assert!(matches!(&page[0].event, ChatEvent::Message { .. }));
        assert_eq!(page[0].event.opened().rumor.content, "everyone can read this");
    }
}
