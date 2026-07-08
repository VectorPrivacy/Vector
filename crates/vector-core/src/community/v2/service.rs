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

use nostr_sdk::prelude::{Event, Keys, PublicKey, Timestamp};

use super::super::transport::{Query, Transport};
use super::super::{ChannelId, Epoch};
use super::chat::{self, ChatEvent};
use super::community::CommunityV2;
use super::control;
use super::derive::channel_group_key;
use super::invite::{self, CommunityInvite};
use super::{guestbook, stream};
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

// ── Invites (CORD-05) ────────────────────────────────────────────────────────

/// Build the §1 invite bundle for this community. Every channel is granted: a
/// Public channel carries the `community_root` as its "key" (the joiner derives
/// the real secret from the root), a Private one its own key. The bundle
/// self-certifies the owner, so the inviter's identity is irrelevant to trust.
pub fn bundle_of(community: &CommunityV2, creator: Option<PublicKey>, expires_at_ms: Option<u64>) -> CommunityInvite {
    let hex = crate::simd::hex::bytes_to_hex_32;
    let channels = community
        .channels
        .iter()
        .map(|c| invite::ChannelGrant {
            id: hex(&c.id.0),
            key: hex(&c.key.unwrap_or(community.community_root)),
            epoch: c.epoch.0,
            name: c.name.clone(),
        })
        .collect();
    CommunityInvite {
        community_id: hex(&community.identity.community_id.0),
        owner: hex(&community.identity.owner_xonly),
        owner_salt: hex(&community.identity.owner_salt),
        community_root: hex(&community.community_root),
        root_epoch: community.root_epoch.0,
        channels,
        relays: community.relays.clone(),
        name: community.name.clone(),
        icon: None,
        expires_at: expires_at_ms,
        creator_npub: creator.map(|p| p.to_hex()),
        label: None,
        extra: Default::default(),
    }
}

/// Gift-wrap a Direct Invite (kind 3313) of this community straight to `recipient`
/// and publish it to the community relays. The bundle hands over the keys; the
/// recipient consents by accepting (nothing joins on receipt). Returns the wrap.
pub async fn send_direct_invite<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    recipient: &PublicKey,
) -> Result<Event, String> {
    let session = SessionGuard::capture();
    let inviter = local_keys()?;
    let bundle = bundle_of(community, Some(inviter.public_key()), None);
    let wrap = invite::build_direct_invite(&inviter, recipient, &bundle).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed before sending invite".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    Ok(wrap)
}

/// A minted public link: the shareable URL plus the addressable bundle event to
/// publish and the link keypair to retain (in the Invite List) for later refresh
/// or revocation.
pub struct MintedLink {
    pub url: String,
    pub bundle_event: Event,
    pub link_signer: Keys,
    pub token: [u8; super::derive::TOKEN_LEN],
}

/// Mint a public invite link for this community: a fresh token + link keypair, the
/// bundle encrypted under the token key and published at `(33301, link_signer,
/// "")`, and the `base/invite/<naddr>#<fragment>` URL. `base` is the deep-link
/// domain (e.g. `https://vectorapp.io`); the fragment carries the token + bootstrap
/// relays and never reaches a server.
pub async fn mint_public_link<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    base: &str,
) -> Result<MintedLink, String> {
    let session = SessionGuard::capture();
    let mut token = [0u8; super::derive::TOKEN_LEN];
    token.copy_from_slice(&super::super::random_32()[..super::derive::TOKEN_LEN]);
    let link_signer = Keys::generate();
    let bundle = bundle_of(community, Some(local_keys()?.public_key()), None);
    let bundle_key = super::derive::invite_bundle_key(&token);
    let bundle_event = invite::build_bundle_event(&link_signer, &bundle, &bundle_key).map_err(|e| e.to_string())?;
    let url = invite::build_invite_url(base, &link_signer.public_key(), &token, &community.relays).map_err(|e| e.to_string())?;

    if !session.is_valid() {
        return Err("account changed before minting link".to_string());
    }
    transport.publish_durable(&bundle_event, &community.relays).await?;
    Ok(MintedLink { url, bundle_event, link_signer, token })
}

/// Accept an already-unwrapped bundle: verify the owner commitment, persist the
/// community, and announce a Guestbook Join (with invite attribution). Shared tail
/// of both the Direct-Invite and public-link accept paths.
async fn accept_bundle<T: Transport + ?Sized>(
    transport: &T,
    bundle: &CommunityInvite,
    invited_by: Option<PublicKey>,
) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let at_ms = now_ms();
    // Expiry gate: a past invite still previews but must not join (CORD-05 §1).
    if bundle.expires_at.is_some_and(|exp| at_ms > exp) {
        return Err("this invite has expired".to_string());
    }
    let community = CommunityV2::from_bundle(bundle, at_ms)?;

    if !session.is_valid() {
        return Err("account changed during join".to_string());
    }
    crate::db::community::save_community_v2(&community)?;

    // Announce our Guestbook Join, echoing the invite attribution when present.
    let attribution = invited_by
        .map(|p| p.to_hex())
        .or_else(|| bundle.creator_npub.clone())
        .zip(Some(bundle.label.clone().unwrap_or_default()));
    let attr_ref = attribution.as_ref().map(|(c, l)| (c.as_str(), l.as_str()));
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let join_rumor = guestbook::build_join_rumor(me.public_key(), attr_ref, at_ms);
    if let Ok((join_wrap, _)) = guestbook::seal_guestbook_rumor(&join_rumor, &gb_group, &me, Timestamp::from_secs(at_ms / 1000)) {
        let _ = transport.publish(&join_wrap, &community.relays).await;
    }

    Ok(community)
}

/// Accept a Direct Invite: unwrap the 3313 giftwrap (Schnorr-verifying the seal),
/// then run the shared accept path. The recipient's consent IS this call.
pub async fn accept_direct_invite<T: Transport + ?Sized>(transport: &T, wrap: &Event) -> Result<CommunityV2, String> {
    let me = local_keys()?;
    let (inviter, bundle) = invite::unwrap_direct_invite(wrap, &me).map_err(|e| e.to_string())?;
    accept_bundle(transport, &bundle, Some(inviter)).await
}

/// Accept a public invite link: parse it, fetch the addressable bundle at
/// `(33301, link_signer, "")`, decrypt with the token key, and join. Refuses a
/// revoked link (the fetcher finds the grave, not keys).
pub async fn accept_public_link<T: Transport + ?Sized>(transport: &T, url: &str) -> Result<CommunityV2, String> {
    let parsed = invite::parse_invite_link(url).map_err(|e| e.to_string())?;
    // Fetch the bundle by its exact coordinate (author is the anti-squat guard).
    let query = Query {
        kinds: vec![super::kind::INVITE_BUNDLE],
        authors: vec![parsed.link_signer.to_hex()],
        d_tags: vec![String::new()],
        limit: Some(1),
        ..Default::default()
    };
    let relays = if parsed.bootstrap_relays.is_empty() {
        invite::stock_relays()
    } else {
        parsed.bootstrap_relays.clone()
    };
    let events = transport.fetch(&query, &relays).await?;
    let event = events.first().ok_or("invite bundle not found on relays")?;
    let bundle_key = super::derive::invite_bundle_key(&parsed.token);
    match invite::parse_bundle_event(event, &parsed.link_signer, &bundle_key).map_err(|e| e.to_string())? {
        invite::BundleState::Live(bundle) => accept_bundle(transport, &bundle, None).await,
        invite::BundleState::Revoked => Err("this invite link has been revoked".to_string()),
    }
}

/// Fold the Complete Memberlist of a community from its Guestbook plane: fetch
/// the Guestbook, coalesce (owner is always a member via genesis Join), and
/// return the present npubs. Authority for kicks is resolved owner-only in the
/// first cut (full roster folding lands with moderation).
pub async fn memberlist<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<Vec<PublicKey>, String> {
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![gb_group.pk_hex()],
        limit: Some(500),
        ..Default::default()
    };
    let wraps = transport.fetch(&query, &community.relays).await?;

    let owner = community.owner()?;
    let mut events = Vec::new();
    for wrap in &wraps {
        if let Ok(opened) = stream::open_wrap(wrap, &gb_group) {
            if let Ok(ev) = guestbook::parse_guestbook_event(&opened) {
                events.push(ev);
            }
        }
    }
    // First-cut authority: only the owner may kick (full roster fold is a follow-up).
    let can_kick = move |actor: &PublicKey, _target: &PublicKey| *actor == owner;
    let coalesced = guestbook::coalesce(&events, now_ms(), Some(&owner), &can_kick);
    let banlist = std::collections::BTreeSet::new();
    let observed = std::collections::BTreeMap::new();
    let members = guestbook::complete_memberlist(&coalesced, &observed, &banlist);
    Ok(members.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::super::super::transport::memory::MemoryRelay;
    use super::*;

    /// A distinct npub-shaped account-dir name (bech32 charset) per counter.
    fn account_name(n: u32) -> String {
        const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut acct = String::from("npub1");
        let mut v = n as usize;
        for _ in 0..58 {
            acct.push(B[v % 32] as char);
            v = v / 32 + 7;
        }
        acct
    }

    /// One test participant: its identity keys and its isolated account DB dir.
    struct Actor {
        keys: Keys,
        account: String,
    }

    /// Two participants sharing one relay but isolated per-account DBs — the
    /// cross-account harness a real invite/join loop needs. `swap_to` mirrors a
    /// live `swap_session`: re-point the DB pool + rebind the identity + clear
    /// the per-account id caches, so account A's community is invisible to B
    /// until B legitimately joins.
    struct TestBed {
        _tmp: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
        relay: MemoryRelay,
        relays: Vec<String>,
    }

    impl TestBed {
        fn new() -> (TestBed, Actor, Actor) {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(70_000);
            let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            crate::db::close_database();
            crate::db::clear_id_caches();
            let tmp = tempfile::tempdir().unwrap();
            crate::db::set_app_data_dir(tmp.path().to_path_buf());

            let mut mk = || {
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let account = account_name(n);
                std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
                crate::db::set_current_account(account.clone()).unwrap();
                crate::db::init_database(&account).unwrap();
                Actor { keys: Keys::generate(), account }
            };
            let owner = mk();
            let member = mk();
            let _ = crate::state::take_nostr_client();
            let bed = TestBed {
                _tmp: tmp,
                _guard: guard,
                relay: MemoryRelay::new(),
                relays: vec!["wss://r".to_string()],
            };
            (bed, owner, member)
        }

        /// Become `actor`: swap the account DB + identity, as a real session swap.
        fn swap_to(&self, actor: &Actor) {
            crate::db::set_current_account(actor.account.clone()).unwrap();
            crate::db::init_database(&actor.account).unwrap();
            crate::db::clear_id_caches();
            crate::state::MY_SECRET_KEY.store_from_keys(&actor.keys, &[]);
            crate::state::set_my_public_key(actor.keys.public_key());
        }
    }

    /// Legacy single-actor helper (the create/send tests below).
    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, Keys) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(50_000);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let acct = account_name(n);
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

    /// Fetch a pending Direct Invite (kind 3313 giftwrap) addressed to `me` — the
    /// indexed inbox query CORD-05 §6 defines: `{1059, #p:[me], #k:["3313"]}`.
    async fn fetch_direct_invite(relay: &MemoryRelay, relays: &[String], me: &PublicKey) -> Event {
        let q = Query {
            kinds: vec![stream::KIND_WRAP],
            p_tags: vec![me.to_hex()],
            k_tags: vec!["3313".to_string()],
            ..Default::default()
        };
        relay.fetch(&q, relays).await.unwrap().into_iter().next().expect("a direct invite is waiting")
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

    // ── Two-actor end-to-end (the create → invite → join → message loop) ──────

    async fn texts_in(relay: &MemoryRelay, community: &CommunityV2, channel: &ChannelId) -> Vec<String> {
        fetch_channel(relay, community, channel, 100)
            .await
            .unwrap()
            .iter()
            .filter_map(|f| match &f.event {
                ChatEvent::Message { .. } => Some(f.event.opened().rumor.content.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn direct_invite_full_loop_owner_and_member_converse() {
        let (bed, owner, member) = TestBed::new();

        // Owner creates a community, posts, and Direct-Invites the member's npub.
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Guild", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "owner: welcome!").await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key()).await.unwrap();

        // Member (a DIFFERENT account, no prior knowledge) finds + accepts the invite.
        bed.swap_to(&member);
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "the member does not hold the community before joining"
        );
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();
        assert_eq!(joined.id().0, community.id().0, "joined the same community");
        assert!(joined.identity.verify(), "the joiner independently verifies the owner commitment");
        assert_eq!(joined.owner().unwrap(), owner.keys.public_key());

        // The member reads the owner's public-channel history and replies.
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["owner: welcome!"]);
        send_message(&bed.relay, &joined, &general, "member: thanks for the invite").await.unwrap();

        // The owner reads the member's reply.
        bed.swap_to(&owner);
        assert_eq!(
            texts_in(&bed.relay, &community, &general).await,
            vec!["owner: welcome!", "member: thanks for the invite"],
            "both actors' messages interleave in ms order on the shared channel"
        );

        // The Guestbook memberlist now folds both participants.
        let members = memberlist(&bed.relay, &community).await.unwrap();
        assert!(members.contains(&owner.keys.public_key()), "owner is a member (genesis Join)");
        assert!(members.contains(&member.keys.public_key()), "member is a member (invite Join)");
        assert_eq!(members.len(), 2);
    }

    #[tokio::test]
    async fn public_link_full_loop() {
        let (bed, owner, member) = TestBed::new();

        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Public Guild", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "come on in").await.unwrap();
        // Mint a shareable link (a non-stock relay so the fragment carries it).
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io").await.unwrap();
        assert!(link.url.starts_with("https://vectorapp.io/invite/"));
        assert!(link.url.contains('#'), "the fragment carries the token");

        // Member joins purely from the URL string.
        bed.swap_to(&member);
        let joined = accept_public_link(&bed.relay, &link.url).await.unwrap();
        assert_eq!(joined.id().0, community.id().0);
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["come on in"]);
    }

    #[tokio::test]
    async fn a_revoked_link_refuses_to_join() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Revoked", bed.relays.clone(), None).await.unwrap();
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io").await.unwrap();
        // Owner retires the link (re-posts the coordinate as a tombstone).
        let tombstone = invite::build_revocation(&link.link_signer).unwrap();
        bed.relay.publish_durable(&tombstone, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let err = accept_public_link(&bed.relay, &link.url).await.unwrap_err();
        assert!(err.contains("revoked"), "a retired link finds the grave, not keys: {err}");
    }

    #[tokio::test]
    async fn an_expired_direct_invite_refuses_to_join() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Expired", bed.relays.clone(), None).await.unwrap();
        // Hand-mint an invite that expired in the past.
        let inviter = owner.keys.clone();
        let mut bundle = bundle_of(&community, Some(inviter.public_key()), Some(1_000));
        bundle.expires_at = Some(1_000); // unix ms, long past
        let wrap = invite::build_direct_invite(&inviter, &member.keys.public_key(), &bundle).unwrap();
        bed.relay.publish(&wrap, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let err = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap_err();
        assert!(err.contains("expired"), "a past-expiry invite refuses to join: {err}");
    }

    #[tokio::test]
    async fn a_swapped_member_cannot_see_the_owners_community_until_joining() {
        // Multi-account isolation: after the swap, the member's DB holds nothing
        // of the owner's community — the dual-stack storage is per-account.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Private-so-far", bed.relays.clone(), None).await.unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_some());

        bed.swap_to(&member);
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "the owner's community must be invisible in the member's account DB"
        );
        assert_eq!(crate::db::community::list_community_ids().unwrap().len(), 0);
    }
}
