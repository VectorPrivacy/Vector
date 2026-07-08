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

use nostr_sdk::prelude::{Event, Keys, PublicKey, SecretKey, Timestamp};

use super::super::transport::{Query, Transport};
use super::super::{version, ChannelId, Epoch};
use super::chat::{self, ChatEvent};
use super::community::{ChannelV2, CommunityV2};
use super::control;
use super::derive::{base_rekey_group_key, channel_group_key, channel_rekey_group_key, control_group_key, GroupKey};
use super::invite::{self, CommunityInvite};
use super::rekey::{self, Continuity, RekeyScope};
use super::{guestbook, stream, vsk};
use crate::community::edition::ParsedEdition;
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
pub fn bundle_of(
    community: &CommunityV2,
    creator: Option<PublicKey>,
    expires_at_ms: Option<u64>,
    label: Option<String>,
) -> CommunityInvite {
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
        label,
        extra: Default::default(),
    }
}

/// Gift-wrap a Direct Invite (kind 3313) of this community straight to `recipient`
/// and publish it to the community relays. `expires_at_ms` (unix ms) optionally
/// bounds its shelf life; `label` is echoed in the joiner's Guestbook Join. The
/// bundle hands over the keys; the recipient consents by accepting (nothing joins
/// on receipt). Returns the wrap.
pub async fn send_direct_invite<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    recipient: &PublicKey,
    expires_at_ms: Option<u64>,
    label: Option<String>,
) -> Result<Event, String> {
    let session = SessionGuard::capture();
    let inviter = local_keys()?;
    let bundle = bundle_of(community, Some(inviter.public_key()), expires_at_ms, label);
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
    expires_at_ms: Option<u64>,
    label: Option<String>,
) -> Result<MintedLink, String> {
    let session = SessionGuard::capture();
    let mut token = [0u8; super::derive::TOKEN_LEN];
    token.copy_from_slice(&super::super::random_32()[..super::derive::TOKEN_LEN]);
    let link_signer = Keys::generate();
    let bundle = bundle_of(community, Some(local_keys()?.public_key()), expires_at_ms, label);
    let bundle_key = super::derive::invite_bundle_key(&token);
    let bundle_event = invite::build_bundle_event(&link_signer, &bundle, &bundle_key).map_err(|e| e.to_string())?;
    let url = invite::build_invite_url(base, &link_signer.public_key(), &token, &community.relays).map_err(|e| e.to_string())?;

    if !session.is_valid() {
        return Err("account changed before minting link".to_string());
    }
    transport.publish_durable(&bundle_event, &community.relays).await?;
    Ok(MintedLink { url, bundle_event, link_signer, token })
}

/// Accept an already-unwrapped bundle: verify the owner commitment AND that the
/// delivered community_root is genuinely the owner's, persist the community, and
/// announce a Guestbook Join (with invite attribution). Shared tail of both accept
/// paths. Takes the caller's `SessionGuard` (captured BEFORE any network fetch the
/// caller did) so the `is_valid()` gate straddles that I/O.
async fn accept_bundle<T: Transport + ?Sized>(
    transport: &T,
    session: &SessionGuard,
    bundle: &CommunityInvite,
    invited_by: Option<PublicKey>,
) -> Result<CommunityV2, String> {
    let me = local_keys()?;
    let at_ms = now_ms();
    // Expiry gate: a past invite still previews but must not join (CORD-05 §1).
    if bundle.expired(at_ms) {
        return Err("this invite has expired".to_string());
    }
    // `from_bundle` re-validates bounds + the owner commitment fail-closed.
    let community = CommunityV2::from_bundle(bundle, at_ms)?;

    // Authenticate the delivered community_root before trusting it. The owner
    // commitment proves WHO the owner is, but community_root (and channel keys) are
    // NOT in that commitment, so a forged invite can pair a real (id, owner, salt)
    // with an attacker-chosen root and silently partition the joiner onto planes
    // only the attacker controls. Requiring the owner's genesis to open under the
    // delivered root closes that eclipse; also reconciles channel classification.
    let community = verify_owner_root_and_reconcile(transport, community).await?;

    // The account must not have swapped since the guard was captured (which was
    // before any fetch the caller / the verify above performed) — else we'd write
    // A's join into B.
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

/// Prove the delivered `community_root` is genuinely the owner's, and reconcile
/// channel classification from the owner's editions. `community_id` commits only
/// to `(owner_xonly, owner_salt)` — both semi-public (they ride every bundle and
/// every synced Community List) — so a forged invite can present a real community's
/// id/owner/salt with an attacker-chosen root; every plane then derives from that
/// root, silently eclipsing the joiner onto attacker-controlled addresses while the
/// owner commitment still "verifies". The defense: the owner's genesis metadata
/// edition (vsk-0, `eid == community_id`) only opens under the AUTHENTIC root — an
/// attacker can't forge the owner's seal — so its presence on the control plane
/// derived from the delivered root proves that root. Fail-closed: no owner genesis
/// (forged invite, or relays unreachable) → refuse to join. On success, folds the
/// owner's authoritative editions to heal a bundle that misclassified a channel.
async fn verify_owner_root_and_reconcile<T: Transport + ?Sized>(
    transport: &T,
    community: CommunityV2,
) -> Result<CommunityV2, String> {
    let owner = community.owner()?;
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let control_pk = control.pk_hex();

    // The control plane is writable by ANY community_root holder, so a single
    // newest-first window could be flooded to evict the owner's genesis and DoS
    // every join. Page OLDER with an `until` cursor until the owner genesis is
    // found or the plane is exhausted (a short page). This fully fixes the organic
    // case (a mature plane with >PAGE recent editions) and raises the flood bar to
    // MAX_PAGES*PAGE events; a determined flood that pins >PAGE junk at the exact
    // genesis timestamp is a residual whose real fix is binding the root into
    // community_id (protocol item, deferred to the coordinated upgrade). The common
    // case (fresh community) and the eclipse case (forged root, nothing owner-signed
    // to find) both resolve on the first page.
    const PAGE: usize = 500;
    const MAX_PAGES: usize = 6;
    let mut editions: Vec<ParsedEdition> = Vec::new();
    let mut found_genesis = false;
    let mut until: Option<u64> = None;
    for _ in 0..MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![control_pk.clone()],
            until,
            limit: Some(PAGE),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, &community.relays).await?;
        let got = wraps.len();
        let mut oldest: Option<u64> = None;
        for w in &wraps {
            let ts = w.created_at.as_secs();
            oldest = Some(oldest.map_or(ts, |o| o.min(ts)));
            if let Ok((ed, _)) = control::open_control_edition(w, &control) {
                if ed.author == owner {
                    if ed.vsk == vsk::COMMUNITY_METADATA && ed.entity_id == community.id().0 {
                        found_genesis = true;
                    }
                    editions.push(ed);
                }
            }
        }
        if found_genesis || got < PAGE {
            break; // found the owner genesis, or exhausted the plane.
        }
        match oldest {
            Some(o) if o > 0 => until = Some(o - 1),
            _ => break,
        }
    }
    if !found_genesis {
        return Err(
            "could not verify this community from its relays (the invite may be forged, the relays are unreachable, or the control plane is being flooded); not joining"
                .to_string(),
        );
    }
    Ok(apply_control_fold(&community, &editions).unwrap_or(community))
}

/// Accept a Direct Invite: unwrap the 3313 giftwrap (Schnorr-verifying the seal),
/// then run the shared accept path. The recipient's consent IS this call. No
/// network await precedes the accept, so the guard captured here suffices.
pub async fn accept_direct_invite<T: Transport + ?Sized>(transport: &T, wrap: &Event) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let (inviter, bundle) = invite::unwrap_direct_invite(wrap, &me).map_err(|e| e.to_string())?;
    accept_bundle(transport, &session, &bundle, Some(inviter)).await
}

/// Accept a public invite link: parse it, fetch every event at `(33301,
/// link_signer, "")`, and join. **Revocation is authoritative-if-present**: if
/// ANY signer-valid tombstone is among the fetched events, refuse — never trust
/// fetch ordering (a cross-relay union has no global newest-first sort, so a
/// stale Live could otherwise win a partial-propagation race). Otherwise pick the
/// newest valid Live by `created_at`.
pub async fn accept_public_link<T: Transport + ?Sized>(transport: &T, url: &str) -> Result<CommunityV2, String> {
    // Capture BEFORE the network fetch so the join's is_valid() gate straddles it.
    let session = SessionGuard::capture();
    let parsed = invite::parse_invite_link(url).map_err(|e| e.to_string())?;
    let query = Query {
        kinds: vec![super::kind::INVITE_BUNDLE],
        authors: vec![parsed.link_signer.to_hex()],
        d_tags: vec![String::new()],
        ..Default::default()
    };
    let relays = if parsed.bootstrap_relays.is_empty() {
        invite::stock_relays()
    } else {
        parsed.bootstrap_relays.clone()
    };
    let events = transport.fetch(&query, &relays).await?;
    if !session.is_valid() {
        return Err("account changed during join".to_string());
    }
    let bundle_key = super::derive::invite_bundle_key(&parsed.token);

    // Scan EVERY event: a tombstone beats a Live unconditionally (order-independent).
    let mut newest_live: Option<(u64, CommunityInvite)> = None;
    let mut found_any = false;
    for event in &events {
        match invite::parse_bundle_event(event, &parsed.link_signer, &bundle_key) {
            Ok(invite::BundleState::Revoked) => return Err("this invite link has been revoked".to_string()),
            Ok(invite::BundleState::Live(bundle)) => {
                found_any = true;
                let at = event.created_at.as_secs();
                if newest_live.as_ref().is_none_or(|(t, _)| at > *t) {
                    newest_live = Some((at, *bundle));
                }
            }
            Err(_) => {} // a foreign/garbage event at the coordinate — ignore.
        }
    }
    match newest_live {
        Some((_, bundle)) => accept_bundle(transport, &session, &bundle, None).await,
        None if found_any => unreachable!(),
        None => Err("invite bundle not found on relays".to_string()),
    }
}

/// Leave a community: publish a Guestbook Leave and tear down the local hold.
pub async fn leave_community<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let at_ms = now_ms();
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let leave_rumor = guestbook::build_leave_rumor(me.public_key(), at_ms);
    if let Ok((wrap, _)) = guestbook::seal_guestbook_rumor(&leave_rumor, &gb_group, &me, Timestamp::from_secs(at_ms / 1000)) {
        let _ = transport.publish(&wrap, &community.relays).await;
    }
    if !session.is_valid() {
        return Err("account changed during leave".to_string());
    }
    crate::db::community::delete_community(&crate::simd::hex::bytes_to_hex_32(&community.id().0))?;
    Ok(())
}

/// Fold the Complete Memberlist from the Guestbook plane. The proven owner is
/// ALWAYS a member (derived from the self-certifying community_id — no network,
/// so a lost/evicted genesis Join can't drop them). Observed authors — anyone
/// seen publishing on a channel — are folded in FORWARD-only per CORD-02 §5, so a
/// member whose Join was lost still counts.
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
    // Observed authors: fold each held channel's recent authorship (real author +
    // newest ms), so a member who posted but whose Join was lost is still counted.
    let mut observed: std::collections::BTreeMap<PublicKey, u64> = std::collections::BTreeMap::new();
    for ch in &community.channels {
        if let Ok(page) = fetch_channel(transport, community, &ch.id, 200).await {
            for f in &page {
                let e = observed.entry(f.event.opened().author).or_insert(0);
                *e = (*e).max(f.event.opened().at_ms);
            }
        }
    }

    // Genesis / never-refounded community: NO snapshot authority (there is no
    // refounder — an owner who didn't mint the epoch has no snapshot power). When
    // rotation lands, thread the actual refounder of `root_epoch`.
    let no_snapshots: Option<&PublicKey> = None;
    // First-cut authority: only the owner may kick (full roster fold is a follow-up).
    let can_kick = move |actor: &PublicKey, _target: &PublicKey| *actor == owner;
    let coalesced = guestbook::coalesce(&events, now_ms(), no_snapshots, &can_kick);
    let banlist = std::collections::BTreeSet::new();
    let mut members = guestbook::complete_memberlist(&coalesced, &observed, &banlist);
    // The owner is a member by definition, independent of any fetched Join.
    if !banlist.contains(&owner) {
        members.insert(owner);
    }
    Ok(members.into_iter().collect())
}

// ── Live control-follow (CORD-02 §6 / CORD-03 §2) ────────────────────────────

/// Re-fold this community's Control Plane and apply the current metadata +
/// **public** channel set to the held community, persisting any change. Called
/// when a control-plane wrap arrives in realtime (a rename, a new channel, an
/// edited description) so a long-running bot tracks the community mid-session
/// instead of freezing at its join-time view.
///
/// **Authority (first cut): owner-authored editions only.** The roster fold that
/// resolves an admin's `MANAGE_METADATA`/`MANAGE_CHANNELS` grant is deferred, so
/// a non-owner edition is not folded yet — a safe under-approximation (it never
/// trusts a non-owner; it only lags an admin action until the owner's view lands
/// or roster-follow ships). The owner is proven by the self-certifying
/// community_id, so this needs no network trust.
///
/// **Private channels are skipped here:** a Private channel's Chat-Plane key is
/// delivered over the rekey plane (or an invite bundle), never derivable from a
/// control edition alone. A new Private channel therefore surfaces only once
/// [`follow_rekeys`] delivers its key. Public channels derive from the
/// community_root, so they fold in directly.
///
/// Returns the updated community iff something changed (so the caller can skip a
/// redundant re-subscribe + refresh notification).
pub async fn follow_control<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
) -> Result<Option<CommunityV2>, String> {
    let owner = community.owner()?;
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![control.pk_hex()],
        limit: Some(500),
        ..Default::default()
    };
    let wraps = transport.fetch(&query, &community.relays).await?;

    // Open + seal-verify every edition; keep only the owner's (the authority gate).
    let mut editions: Vec<ParsedEdition> = Vec::new();
    for w in &wraps {
        if let Ok((ed, _)) = control::open_control_edition(w, &control) {
            if ed.author == owner {
                editions.push(ed);
            }
        }
    }

    let Some(updated) = apply_control_fold(community, &editions) else {
        return Ok(None);
    };
    // The fetch straddled an await; a swap since the guard was captured must not
    // write account A's control state into B.
    if !session.is_valid() {
        return Err("account changed during control follow".to_string());
    }
    crate::db::community::save_community_v2(&updated)?;
    Ok(Some(updated))
}

/// Fold a set of owner-authored control editions into an updated community
/// (pure). Groups editions by `(vsk, entity_id)` and takes each entity's highest
/// version *present in the fetched set* via [`version::bootstrap_head`] — the lens
/// for a bootstrapping reader that holds no per-entity version floor and trusts
/// the owner's signature (already verified upstream). Applies community metadata
/// (name/description/relays) and public channel add/rename/delete. Returns `None`
/// if nothing changed.
///
/// **Known limitation (no persisted floor):** because there's no stored per-entity
/// version, this can be rolled BACKWARD by a relay that withholds the newest
/// owner editions and serves only an older (still owner-signed) prefix — reverting
/// a rename or resurrecting a deleted channel until the full set is re-fetched. It
/// never trusts a non-owner, and self-heals once the complete chain arrives, but a
/// tracking-grade fold (persist the applied version + `version::fold` with
/// refuse-downgrade) is the durable fix, deferred with the broader floor-
/// persistence work. See [[concord_v2_build]].
fn apply_control_fold(community: &CommunityV2, editions: &[ParsedEdition]) -> Option<CommunityV2> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<(String, [u8; 32]), Vec<&ParsedEdition>> = BTreeMap::new();
    for e in editions {
        groups.entry((e.vsk.clone(), e.entity_id)).or_default().push(e);
    }

    let mut out = community.clone();
    let mut changed = false;
    for ((vsk_code, eid), group) in &groups {
        let fold_eds: Vec<version::Edition> = group.iter().map(|p| p.to_fold_edition()).collect();
        let Some(hi) = version::bootstrap_head(&fold_eds, 0) else {
            continue;
        };
        let content = group[hi].content.as_str();

        if vsk_code == vsk::COMMUNITY_METADATA && *eid == community.id().0 {
            if let Ok(meta) = serde_json::from_str::<control::CommunityMetadata>(content) {
                changed |= apply_community_metadata(&mut out, meta);
            }
        } else if vsk_code == vsk::CHANNEL_METADATA {
            // A vsk-2 edition carries no community binding (shared v1 grammar), so a
            // same-owner cross-community replay can inject a phantom PUBLIC channel
            // here. Bounded: its key is this community's root (no foreign secret
            // bridges in) and eids don't collide, so real channels can't be
            // renamed/deleted. Binding community_id into the signed edition is a wire
            // change (breaks the shared edition_hash + Armada interop) — deferred.
            if let Ok(meta) = serde_json::from_str::<control::ChannelMetadata>(content) {
                changed |= apply_channel_metadata(&mut out, ChannelId(*eid), meta);
            }
        }
    }
    changed.then_some(out)
}

/// Apply a folded community-metadata head. Relays only overwrite when the edition
/// carries a non-empty list (a metadata edition that omits relays must not blank
/// the working set). Returns whether anything changed.
fn apply_community_metadata(out: &mut CommunityV2, meta: control::CommunityMetadata) -> bool {
    let mut changed = false;
    if out.name != meta.name {
        out.name = meta.name;
        changed = true;
    }
    if out.description != meta.description {
        out.description = meta.description;
        changed = true;
    }
    if !meta.relays.is_empty() && out.relays != meta.relays {
        out.relays = meta.relays;
        changed = true;
    }
    changed
}

/// Apply a folded channel-metadata head: delete removes the channel, a rename
/// updates an existing one, and a brand-new PUBLIC channel is added (a new Private
/// channel is skipped — its key arrives over the rekey plane). Returns whether
/// anything changed.
fn apply_channel_metadata(out: &mut CommunityV2, id: ChannelId, meta: control::ChannelMetadata) -> bool {
    let deleted = meta.deleted.unwrap_or(false);
    if deleted {
        let before = out.channels.len();
        out.channels.retain(|c| c.id.0 != id.0);
        return out.channels.len() != before;
    }
    match out.channels.iter_mut().find(|c| c.id.0 == id.0) {
        Some(existing) => {
            let mut changed = false;
            if existing.name != meta.name {
                existing.name = meta.name;
                changed = true;
            }
            // The owner's edition authoritatively declares visibility. A channel the
            // owner marks PUBLIC must derive from the root (key = None) — this heals a
            // bundle-time misclassification where an attacker set a public channel's
            // grant key to their own, silently addressing it at a plane only they read.
            // (public -> private needs a rekey-delivered key, so it's left to rekey.)
            if !meta.private && (existing.private || existing.key.is_some()) {
                existing.private = false;
                existing.key = None;
                changed = true;
            }
            changed
        }
        None if !meta.private => {
            // A public channel derives its Chat Plane from the community_root at the
            // current root epoch (key = None); its stored epoch mirrors the root.
            out.channels.push(ChannelV2 {
                id,
                name: meta.name,
                private: false,
                key: None,
                epoch: out.root_epoch,
            });
            true
        }
        None => false, // a new Private channel — deferred to rekey key delivery.
    }
}

// ── Live rekey-follow (CORD-06 §2/§3) ────────────────────────────────────────

/// The outcome of a rekey-follow pass.
pub struct RekeyFollow {
    /// The community after adopting every rotation it could catch up on, or `None`
    /// if nothing advanced.
    pub updated: Option<CommunityV2>,
    /// A base rotation removed us — the caller tears the local hold down (the
    /// updated community is not persisted in that case).
    pub self_removed: bool,
}

/// Follow rekeys for a held community: advance the base (root) epoch and each
/// Private channel's epoch as far as owner-authored rotations allow, adopting the
/// fresh key we're still a recipient of at each step and dropping a scope we've
/// been removed from. Persists the result. Called when a rekey wrap arrives in
/// realtime so a long-running bot keeps decrypting after a rotation instead of
/// going silent.
///
/// **Authority (first cut): owner-authored rotations only** (`rotator == owner`).
/// The roster fold that would honour an admin's `BAN`/`MANAGE_CHANNELS` rotation
/// is deferred — a safe under-approximation (never adopts a non-owner's key, which
/// a malicious member could otherwise use to fork the follower onto a key only
/// they know). A legitimate admin-driven channel rekey is simply not followed yet.
///
/// **Continuity + fork resolution are spec-strict:** only a rotation whose
/// `prevcommit` extends the exact `(epoch, key)` I hold advances me, one epoch at
/// a time; a same-epoch owner fork resolves by the lexicographically lowest new
/// key ([`rekey::lowest_key_winner`]), so every follower converges. An incomplete
/// rotation (a missing chunk) never concludes removal — it just waits.
///
/// **First-cut limitation (documented):** no prior-epoch read archive. Adopting a
/// rotation moves the scope's read coordinate forward; history published under the
/// old epoch is not re-fetched afterwards (a live follower already received it).
/// The multi-epoch read archive layers on with the GUI history work.
pub async fn follow_rekeys<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
) -> Result<RekeyFollow, String> {
    let me = local_keys()?;
    let my_xonly = me.public_key().to_bytes();
    let owner = community.owner()?;
    let mut cur = community.clone();
    let mut changed = false;

    // Bound the catch-up: each real step consumes a valid owner rotation, so a
    // finite chain terminates naturally; the cap defends against a relay feeding a
    // pathological set.
    const MAX_STEPS: usize = 128;
    for _ in 0..MAX_STEPS {
        let mut advanced = false;

        // Private channels first: a removal-forced channel rekey can ride the PRIOR
        // root (CORD-06 D2), so read channels under the current root before a base
        // adopt moves it.
        let channel_ids: Vec<ChannelId> = cur.channels.iter().filter(|c| c.private).map(|c| c.id).collect();
        for cid in channel_ids {
            let (held_key, held_epoch) = match cur.channel(&cid) {
                Some(ch) => match ch.key {
                    Some(k) => (k, ch.epoch),
                    None => continue, // a keyless private channel can't be advanced.
                },
                None => continue,
            };
            let next = Epoch(held_epoch.0.saturating_add(1));
            let group = channel_rekey_group_key(&cur.community_root, &cid, next);
            let chunks = fetch_rekey_chunks(transport, &cur.relays, &group).await?;
            match advance_scope(&chunks, RekeyScope::Channel(cid), &owner, me.secret_key(), &my_xonly, held_epoch, &held_key, next) {
                Advance::Adopt { new_key } => {
                    if let Some(ch) = cur.channels.iter_mut().find(|c| c.id.0 == cid.0) {
                        ch.key = Some(new_key);
                        ch.epoch = next;
                    }
                    advanced = true;
                    changed = true;
                }
                Advance::Removed => {
                    cur.channels.retain(|c| c.id.0 != cid.0);
                    advanced = true;
                    changed = true;
                }
                Advance::Stay => {}
            }
        }

        // Base rotation (Refounding): advances the root + root_epoch, re-addressing
        // every public channel, the guestbook, and the control plane by derivation
        // (refresh_subscription recomputes the author-set from the new root).
        {
            let held_epoch = cur.root_epoch;
            let held_key = cur.community_root;
            let next = Epoch(held_epoch.0.saturating_add(1));
            let group = base_rekey_group_key(&cur.community_root, cur.id(), next);
            let chunks = fetch_rekey_chunks(transport, &cur.relays, &group).await?;
            match advance_scope(&chunks, RekeyScope::Root, &owner, me.secret_key(), &my_xonly, held_epoch, &held_key, next) {
                Advance::Adopt { new_key } => {
                    cur.community_root = new_key;
                    cur.root_epoch = next;
                    advanced = true;
                    changed = true;
                }
                Advance::Removed => {
                    if !session.is_valid() {
                        return Err("account changed during rekey follow".to_string());
                    }
                    return Ok(RekeyFollow { updated: None, self_removed: true });
                }
                Advance::Stay => {}
            }
        }

        if !advanced {
            break;
        }
    }

    if !changed {
        return Ok(RekeyFollow { updated: None, self_removed: false });
    }
    if !session.is_valid() {
        return Err("account changed during rekey follow".to_string());
    }
    crate::db::community::save_community_v2(&cur)?;
    Ok(RekeyFollow { updated: Some(cur), self_removed: false })
}

/// One scope's catch-up decision from the rekey chunks fetched at its next-epoch
/// address.
enum Advance {
    /// Adopt this fresh key for `next_epoch`.
    Adopt { new_key: [u8; 32] },
    /// A complete owner rotation at `next_epoch` dropped my blob — I'm removed.
    Removed,
    /// No owner rotation extends my held epoch (yet) — keep the current key.
    Stay,
}

/// Fetch + parse every seal-verified 3303 chunk at a rekey plane address.
async fn fetch_rekey_chunks<T: Transport + ?Sized>(
    transport: &T,
    relays: &[String],
    group: &GroupKey,
) -> Result<Vec<rekey::RekeyChunk>, String> {
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![group.pk_hex()],
        limit: Some(200),
        ..Default::default()
    };
    let wraps = transport.fetch(&query, relays).await?;
    let mut out = Vec::new();
    for w in &wraps {
        if let Ok(opened) = stream::open_wrap(w, group) {
            if let Ok(chunk) = rekey::parse_rekey_chunk(&opened) {
                out.push(chunk);
            }
        }
    }
    Ok(out)
}

/// Decide how a scope advances from a batch of rekey chunks (pure). Considers only
/// owner-authored, complete rotations of this scope that extend the held
/// `(epoch, key)` and target the immediate `next_epoch` (so a same-epoch fork
/// resolves among candidates at one continuity point). Among those, a rotation
/// carrying my blob yields a candidate key; the lexicographically lowest wins
/// (convergent). If every complete candidate dropped my blob, I'm removed; if none
/// qualifies, stay.
#[allow(clippy::too_many_arguments)]
fn advance_scope(
    chunks: &[rekey::RekeyChunk],
    scope: RekeyScope,
    owner: &PublicKey,
    my_sk: &SecretKey,
    my_xonly: &[u8; 32],
    held_epoch: Epoch,
    held_key: &[u8; 32],
    next_epoch: Epoch,
) -> Advance {
    let rotations = rekey::collect_rotations(chunks);
    let mut winners: Vec<[u8; 32]> = Vec::new();
    let mut saw_complete_candidate = false;
    for r in &rotations {
        if r.rotator != *owner
            || r.scope.id32() != scope.id32()
            || r.new_epoch.0 != next_epoch.0
            || r.continuity(held_epoch, held_key) != Continuity::Extends
            || !r.is_complete()
        {
            continue;
        }
        saw_complete_candidate = true;
        if let Some(blob) = rekey::find_my_blob(&r.blobs, &r.rotator.to_bytes(), my_xonly, r.scope, r.new_epoch) {
            if let Ok(k) = rekey::open_blob_local(my_sk, &r.rotator, r.scope, r.new_epoch, blob) {
                winners.push(k);
            }
        }
    }
    if !winners.is_empty() {
        // `collect_rotations` correlates on `(rotator, scope, new_epoch, prev_commit)`
        // and Extends pins `prev_commit`, so two owner candidates at this point merge
        // into ONE rotation — `winners` holds one key in practice. The lowest-key
        // tiebreak is defensive: it only engages if the owner double-mints one epoch
        // with two distinct keys, and still converges every follower deterministically.
        let idx = rekey::lowest_key_winner(&winners).expect("winners is non-empty");
        return Advance::Adopt { new_key: winners[idx] };
    }
    if saw_complete_candidate {
        Advance::Removed
    } else {
        Advance::Stay
    }
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

    /// A transport that simulates a session swap landing DURING a fetch await —
    /// so a join straddling the fetch sees an invalid session and aborts.
    struct SwapMidFetch {
        inner: MemoryRelay,
    }
    #[async_trait::async_trait]
    impl Transport for SwapMidFetch {
        async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.inner.publish(e, r).await
        }
        async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.inner.publish_durable(e, r).await
        }
        async fn fetch(&self, q: &Query, r: &[String]) -> Result<Vec<Event>, String> {
            let out = self.inner.fetch(q, r).await;
            crate::state::bump_session_generation();
            out
        }
    }

    /// A transport whose `fetch` returns a FIXED, UNSORTED event list — modelling
    /// the production `LiveTransport` union (first-responding relay's batch, no
    /// global newest-first sort), which `MemoryRelay` hides by sorting. This is
    /// the only harness that can exercise the revocation-race ordering.
    struct FixedFetch {
        events: Vec<Event>,
    }
    #[async_trait::async_trait]
    impl Transport for FixedFetch {
        async fn publish(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn publish_durable(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn fetch(&self, _q: &Query, _r: &[String]) -> Result<Vec<Event>, String> {
            Ok(self.events.clone())
        }
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
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

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
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
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
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
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
        let mut bundle = bundle_of(&community, Some(inviter.public_key()), Some(1_000), None);
        bundle.expires_at = Some(1_000); // unix ms, long past
        let wrap = invite::build_direct_invite(&inviter, &member.keys.public_key(), &bundle).unwrap();
        bed.relay.publish(&wrap, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let err = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap_err();
        assert!(err.contains("expired"), "a past-expiry invite refuses to join: {err}");
    }

    #[tokio::test]
    async fn a_tombstone_beats_a_live_bundle_regardless_of_fetch_order() {
        // The revocation-durability fix: if ANY signer-valid tombstone is among the
        // fetched events, refuse — even when a Live bundle is returned FIRST (the
        // production union has no newest-first sort, so a stale relay's Live can lead).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Rev", bed.relays.clone(), None).await.unwrap();
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
        let tombstone = invite::build_revocation(&link.link_signer).unwrap();

        // A relay union that hands back [Live, tombstone] — Live FIRST. Old
        // `events.first()` would join the Live; the scan-all fix must refuse.
        let union = FixedFetch { events: vec![link.bundle_event.clone(), tombstone] };

        bed.swap_to(&member);
        let err = accept_public_link(&union, &link.url).await.unwrap_err();
        assert!(err.contains("revoked"), "a tombstone must beat a Live returned first: {err}");
    }

    #[test]
    fn from_bundle_refuses_an_over_cap_bundle_before_allocating() {
        // The accept-side DoS bound: from_bundle (which accept_bundle calls)
        // rejects a >256-channel bundle via validate() BEFORE the Vec allocation.
        // (The Direct-Invite wire path is additionally bounded by NIP-44's 64KB
        // cap, which trips even earlier — but the count guard is the real defense
        // for the single-layer public-link bundle.)
        let owner = Keys::generate();
        let identity = super::super::control::CommunityIdentity::mint(&owner.public_key());
        let hex = crate::simd::hex::bytes_to_hex_32;
        let root = [0x11u8; 32];
        let mut bundle = CommunityInvite {
            community_id: hex(&identity.community_id.0),
            owner: hex(&identity.owner_xonly),
            owner_salt: hex(&identity.owner_salt),
            community_root: hex(&root),
            root_epoch: 0,
            channels: vec![],
            relays: vec!["wss://r".into()],
            name: "X".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };
        bundle.channels = (0..=invite::MAX_BUNDLE_CHANNELS)
            .map(|i| {
                let mut id = [0u8; 32];
                id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                invite::ChannelGrant { id: hex(&id), key: hex(&root), epoch: 0, name: "x".into() }
            })
            .collect();
        assert!(CommunityV2::from_bundle(&bundle, 0).is_err(), "an over-cap bundle is refused before allocating");
    }

    #[tokio::test]
    async fn a_join_swap_between_fetch_and_save_aborts_and_leaves_the_other_account_clean() {
        // The SessionGuard straddle: a public-link accept fetches then saves. If the
        // account swaps in that window, the join must abort — never write A's
        // community into B's DB. SwapMidFetch bumps the session generation during
        // the fetch await, exactly as a real swap_session would.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Straddle", bed.relays.clone(), None).await.unwrap();
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
        // A fresh swap-injecting transport holding the same bundle event.
        let swap_relay = SwapMidFetch { inner: MemoryRelay::new() };
        swap_relay.inner.publish_durable(&link.bundle_event, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let err = accept_public_link(&swap_relay, &link.url).await.unwrap_err();
        assert!(err.contains("account changed"), "a swap mid-join must abort: {err}");
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "the aborted join wrote nothing to the (member) account DB"
        );
    }

    #[tokio::test]
    async fn the_owner_is_a_member_even_without_a_fetched_genesis_join() {
        // The owner is derived from the self-certifying community_id, so the
        // memberlist includes them independent of any Guestbook fetch.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Owned", vec!["wss://r".into()], None).await.unwrap();
        // A memberlist over an EMPTY guestbook (fetch a community-relay-less view)
        // still contains the owner.
        let empty = MemoryRelay::new();
        let members = memberlist(&empty, &community).await.unwrap();
        assert_eq!(members, vec![owner.public_key()], "owner present with no fetched Join");
    }

    #[tokio::test]
    async fn an_expiring_minted_invite_refuses_after_the_deadline() {
        // The mint path can now produce an expiring invite, and the accept gate
        // trips on it (end-to-end through the real service, not a hand-built bundle).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Timed", bed.relays.clone(), None).await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), Some(1_000), Some("beta".into()))
            .await
            .unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        assert!(
            accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap_err().contains("expired"),
            "a minted expiring invite refuses past its deadline"
        );
    }

    #[tokio::test]
    async fn a_member_who_leaves_drops_from_the_memberlist() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Leaving", bed.relays.clone(), None).await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();
        // Let the leave land strictly after the join.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        leave_community(&bed.relay, &joined).await.unwrap();

        bed.swap_to(&owner);
        let members = memberlist(&bed.relay, &community).await.unwrap();
        assert!(members.contains(&owner.keys.public_key()));
        assert!(!members.contains(&member.keys.public_key()), "a member who left drops from the list");
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

    // ── Live control-follow ──────────────────────────────────────────────────

    /// Publish an owner-grammar channel edition straight to the control plane,
    /// signed by `signer` (the owner for a legit edit, a stranger for the
    /// authority test). `version`/`deleted` drive add-vs-rename-vs-delete.
    async fn publish_channel_edition(
        relay: &MemoryRelay,
        community: &CommunityV2,
        signer: &Keys,
        channel_id: &ChannelId,
        name: &str,
        private: bool,
        version: u64,
        deleted: bool,
    ) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let meta = control::ChannelMetadata { name: name.into(), private, deleted: deleted.then_some(true), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::CHANNEL_METADATA, &channel_id.0, version, None, &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish an owner-grammar community-metadata edition (rename etc.).
    async fn publish_community_meta(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, name: &str, version: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let meta = control::CommunityMetadata { name: name.into(), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, version, None, &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    #[tokio::test]
    async fn follow_control_is_a_noop_on_a_freshly_created_community() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Fresh", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        // Only the genesis editions exist; folding them reproduces the held view.
        assert!(follow_control(&relay, &community, &session).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn follow_control_adds_a_new_public_channel_and_re_subscribes_it() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Grow", vec!["wss://r".into()], None).await.unwrap();
        let new_id = ChannelId([0x5a; 32]);
        publish_channel_edition(&relay, &community, &owner, &new_id, "announcements", false, 1, false).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("a new channel changed the view");
        assert_eq!(updated.channels.len(), 2);
        let added = updated.channel(&new_id).expect("the new channel folded in");
        assert_eq!(added.name, "announcements");
        assert!(!added.private);
        assert_eq!(added.key, None, "a public channel derives from the root (no stored key)");

        // The new channel is now in the realtime author-set (it would be subscribed).
        let authors = super::super::realtime::plane_authors(std::slice::from_ref(&updated));
        let addr = channel_group_key(&updated.community_root, &new_id, updated.root_epoch).pk();
        assert!(authors.contains(&addr), "the added channel joins the live subscription");

        // Persisted: a reload sees it too.
        let reloaded = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(reloaded.channel(&new_id).is_some());
    }

    #[tokio::test]
    async fn follow_control_renames_the_community_and_an_existing_channel() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Old Name", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        // A v2 metadata edition renames the community; a v2 channel edition renames #general.
        publish_community_meta(&relay, &community, &owner, "New Name", 2).await;
        publish_channel_edition(&relay, &community, &owner, &general, "lobby", false, 2, false).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().unwrap();
        assert_eq!(updated.name, "New Name");
        assert_eq!(updated.channel(&general).unwrap().name, "lobby");
        assert_eq!(updated.channels.len(), 1, "a rename doesn't add a channel");
    }

    #[tokio::test]
    async fn follow_control_deletes_a_channel() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prune", vec!["wss://r".into()], None).await.unwrap();
        let extra = ChannelId([0x77; 32]);
        let session = SessionGuard::capture();

        // The channel is first added and folded into the held view.
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 1, false).await;
        let with_extra = follow_control(&relay, &community, &session).await.unwrap().expect("added");
        assert!(with_extra.channel(&extra).is_some());

        // Then it's tombstoned — the delete (higher version) folds the held one back out.
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 2, true).await;
        let updated = follow_control(&relay, &with_extra, &session).await.unwrap().expect("removed");
        assert!(updated.channel(&extra).is_none(), "a deleted channel folds out");
        assert_eq!(updated.channels.len(), 1, "only #general remains");
    }

    #[tokio::test]
    async fn follow_control_ignores_a_non_owner_edition() {
        // A member holds the community_root, so they CAN seal a control edition —
        // but they aren't the owner, so the authority gate drops it (first cut:
        // owner-only). The rogue channel must never appear.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Guarded", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        let rogue_id = ChannelId([0x99; 32]);
        publish_channel_edition(&relay, &community, &rogue, &rogue_id, "backdoor", false, 1, false).await;

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a non-owner control edition is not folded"
        );
    }

    #[tokio::test]
    async fn follow_control_skips_a_new_private_channel_without_a_key() {
        // A Private channel's key rides the rekey plane, not the control edition —
        // control-follow can't key it, so it's deferred (never added keyless, which
        // would wrongly read the public address).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Priv", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0x33; 32]);
        publish_channel_edition(&relay, &community, &owner, &priv_id, "mods", true, 1, false).await;

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a new private channel is skipped until its key arrives via rekey"
        );
    }

    // ── Live rekey-follow ────────────────────────────────────────────────────

    /// Publish an owner-grammar base rotation (Refounding) delivering `new_root`
    /// to each recipient. `rotator` is the seal signer (owner for a legit rotation,
    /// a stranger for the authority test); `prev_key` is the root it claims to
    /// extend (mismatch → a fork).
    async fn publish_base_rotation(
        relay: &MemoryRelay,
        community: &CommunityV2,
        rotator: &Keys,
        recipients: &[PublicKey],
        new_root: &[u8; 32],
        prev_key: &[u8; 32],
    ) {
        let new_epoch = Epoch(community.root_epoch.0 + 1);
        let prev_epoch = community.root_epoch;
        let prev_commit = super::super::derive::epoch_key_commitment(prev_epoch, prev_key);
        let group = base_rekey_group_key(&community.community_root, community.id(), new_epoch);
        let blobs: Vec<_> = recipients
            .iter()
            .map(|r| rekey::build_blob_local(rotator.secret_key(), &rotator.public_key().to_bytes(), r, RekeyScope::Root, new_epoch, new_root).unwrap())
            .collect();
        let events = rekey::build_rekey_chunks_local(rotator, &group, RekeyScope::Root, new_epoch, prev_epoch, &prev_commit, &blobs, 2_000).unwrap();
        for e in &events {
            relay.publish(e, &community.relays).await.unwrap();
        }
    }

    /// Attach a Private channel (key + epoch) to a held community and persist it.
    fn add_private_channel(community: &mut CommunityV2, id: ChannelId, key: [u8; 32], epoch: Epoch) {
        community.channels.push(ChannelV2 { id, name: "mods".into(), private: true, key: Some(key), epoch });
        crate::db::community::save_community_v2(community).unwrap();
    }

    #[tokio::test]
    async fn follow_rekeys_is_a_noop_without_rotations() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Still", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "no rotation → nothing to adopt");
    }

    #[tokio::test]
    async fn follow_rekeys_adopts_an_owner_base_rotation() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Refound", vec!["wss://r".into()], None).await.unwrap();
        let new_root = [0xB1; 32];
        // Owner rotates the base to epoch 1, delivering the new root to me.
        publish_base_rotation(&relay, &community, &owner, &[owner.public_key()], &new_root, &community.community_root).await;

        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("adopted");
        assert_eq!(updated.root_epoch, Epoch(1), "advanced one epoch");
        assert_eq!(updated.community_root, new_root, "adopted the fresh root");
        // The public channel now reads under the NEW root/epoch (its address moved).
        let addr = super::super::realtime::plane_authors(std::slice::from_ref(&updated));
        let general = updated.channels[0].id;
        let new_chat = channel_group_key(&new_root, &general, Epoch(1)).pk();
        assert!(addr.contains(&new_chat), "the public channel re-addresses under the new root");
    }

    #[tokio::test]
    async fn follow_rekeys_adopts_an_owner_private_channel_rotation() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let mut community = create_community(&relay, "PrivRot", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0x33; 32]);
        add_private_channel(&mut community, priv_id, [0x44; 32], Epoch(0));

        // Owner rotates the private channel to epoch 1 with a fresh key, delivered to me.
        let new_key = [0x55; 32];
        let prev_commit = super::super::derive::epoch_key_commitment(Epoch(0), &[0x44; 32]);
        let group = channel_rekey_group_key(&community.community_root, &priv_id, Epoch(1));
        let blob = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &owner.public_key(), RekeyScope::Channel(priv_id), Epoch(1), &new_key).unwrap();
        let events = rekey::build_rekey_chunks_local(&owner, &group, RekeyScope::Channel(priv_id), Epoch(1), Epoch(0), &prev_commit, &[blob], 2_000).unwrap();
        for e in &events {
            relay.publish(e, &community.relays).await.unwrap();
        }

        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("adopted");
        let ch = updated.channel(&priv_id).unwrap();
        assert_eq!(ch.epoch, Epoch(1), "the private channel advanced an epoch");
        assert_eq!(ch.key, Some(new_key), "adopted the fresh channel key");
        assert_eq!(updated.root_epoch, Epoch(0), "the base is untouched by a channel rotation");
    }

    #[tokio::test]
    async fn follow_rekeys_ignores_a_non_owner_rotation() {
        // A member holds the community_root, so they can derive the rekey group key
        // and mint a rotation — but they aren't the owner, so it's not adopted.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Guarded", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        publish_base_rotation(&relay, &community, &rogue, &[rogue.public_key()], &[0xEE; 32], &community.community_root).await;

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "a non-owner rotation is not adopted");
    }

    #[tokio::test]
    async fn follow_rekeys_ignores_a_rotation_off_the_wrong_prev() {
        // A rotation whose prevcommit doesn't match the key I hold is a fork, not an
        // extension — never adopted (would splice me onto an unrelated chain).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Forked", vec!["wss://r".into()], None).await.unwrap();
        // prev_key ≠ the real community_root → the continuity check reads Fork.
        publish_base_rotation(&relay, &community, &owner, &[owner.public_key()], &[0xB2; 32], &[0x00; 32]).await;

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none(), "a fork off the wrong prev is not adopted");
    }

    #[tokio::test]
    async fn follow_rekeys_holds_on_an_incomplete_rotation() {
        // A 2-chunk rotation with only chunk 1 present can never conclude — not an
        // adoption, and crucially NOT a removal (a missing chunk might carry my blob).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Partial", vec!["wss://r".into()], None).await.unwrap();
        let new_epoch = Epoch(1);
        let prev_commit = super::super::derive::epoch_key_commitment(Epoch(0), &community.community_root);
        let group = base_rekey_group_key(&community.community_root, community.id(), new_epoch);
        // Chunk 1 of a declared 2, carrying someone else's blob (not mine).
        let other = Keys::generate();
        let blob = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &other.public_key(), RekeyScope::Root, new_epoch, &[0xB3; 32]).unwrap();
        let rumor = rekey::build_rekey_rumor(owner.public_key(), RekeyScope::Root, new_epoch, Epoch(0), &prev_commit, &[blob], 1, 2, 2_000).unwrap();
        let (wrap, _) = rekey::seal_rekey_chunk(&rumor, &group, &owner, Timestamp::from_secs(2_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "an incomplete rotation neither adopts nor removes");
    }

    #[tokio::test]
    async fn follow_rekeys_removes_a_member_dropped_by_a_base_rotation() {
        // Realistic two-actor removal: the owner Refounds the base and delivers the
        // new root to a THIRD party, not the member — a complete rotation with no
        // blob for the member is a removal.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Evict", bed.relays.clone(), None).await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();

        // Owner rotates, delivering only to a stranger (the member is dropped).
        bed.swap_to(&owner);
        let stranger = Keys::generate();
        publish_base_rotation(&bed.relay, &community, &owner.keys, &[stranger.public_key()], &[0xC4; 32], &community.community_root).await;

        // The member's follow concludes removal (a complete rotation without their blob).
        bed.swap_to(&member);
        let session = SessionGuard::capture();
        let follow = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(follow.self_removed, "a complete base rotation dropping the member removes them");
        assert!(follow.updated.is_none(), "a removed member adopts nothing");
    }

    // ── Audit regressions ────────────────────────────────────────────────────

    #[tokio::test]
    async fn accept_rejects_a_bundle_with_a_forged_community_root() {
        // The eclipse: community_id commits only to (owner, salt) — both semi-public
        // — so a forged invite pairs the REAL triple with an attacker root, and every
        // plane derives from it. The join-time owner-genesis check must refuse.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Real", bed.relays.clone(), None).await.unwrap();

        let fake = crate::simd::hex::bytes_to_hex_32(&[0xEE; 32]);
        let mut forged = bundle_of(&community, None, None, None);
        forged.community_root = fake.clone();
        for ch in &mut forged.channels {
            ch.key = fake.clone();
        }
        let attacker = Keys::generate();
        let wrap = invite::build_direct_invite(&attacker, &member.keys.public_key(), &forged).unwrap();
        bed.relay.publish(&wrap, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let err = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap_err();
        assert!(err.contains("could not verify"), "a forged root fails the owner-genesis check: {err}");
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "a rejected join persists nothing"
        );
    }

    #[tokio::test]
    async fn follow_control_heals_a_bundle_misclassified_public_channel() {
        // A bundle can set a PUBLIC channel's grant key to the attacker's, so the
        // joiner addresses it at a plane only the attacker reads. The owner's genuine
        // public:false edition must override it on follow.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Heal", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let mut poisoned = community.clone();
        poisoned.channels[0].private = true;
        poisoned.channels[0].key = Some([0x66; 32]);
        crate::db::community::save_community_v2(&poisoned).unwrap();

        let session = SessionGuard::capture();
        let healed = follow_control(&relay, &poisoned, &session).await.unwrap().expect("healed");
        let ch = healed.channel(&general).unwrap();
        assert!(!ch.private, "the owner's public declaration overrides the bundle");
        assert_eq!(ch.key, None, "a healed public channel derives from the root");
    }

    #[tokio::test]
    async fn a_deleted_channel_does_not_resurrect_on_reload() {
        // save_community_v2 must prune orphan channel rows, or a control-follow delete
        // reappears (with a stale key) on the next reload.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prune", vec!["wss://r".into()], None).await.unwrap();
        let extra = ChannelId([0x77; 32]);
        let session = SessionGuard::capture();
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 1, false).await;
        let with_extra = follow_control(&relay, &community, &session).await.unwrap().unwrap();
        assert!(with_extra.channel(&extra).is_some());
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 2, true).await;
        let after = follow_control(&relay, &with_extra, &session).await.unwrap().unwrap();
        assert!(after.channel(&extra).is_none());

        let reloaded = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(reloaded.channel(&extra).is_none(), "a deleted channel must not resurrect on reload");
        assert_eq!(reloaded.channels.len(), 1);
    }

    #[tokio::test]
    async fn saving_a_channel_owned_by_another_community_is_refused() {
        // The channel-id hijack: channel_id is the sole DB primary key, so a bundle
        // reusing another community's channel_id would overwrite that row's key. The
        // save must refuse rather than clobber a foreign community's channel.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let a = create_community(&relay, "A", vec!["wss://r".into()], None).await.unwrap();
        let a_channel = a.channels[0].id;
        let mut b = create_community(&relay, "B", vec!["wss://r".into()], None).await.unwrap();
        b.channels[0].id = a_channel; // B tries to claim A's channel id.

        let err = crate::db::community::save_community_v2(&b).unwrap_err();
        assert!(err.contains("another community"), "reusing another community's channel id is refused: {err}");
        // A's channel row is untouched.
        let a_reloaded = crate::db::community::load_community_v2(a.id()).unwrap().unwrap();
        assert_eq!(a_reloaded.channels[0].id.0, a_channel.0);
        assert!(!a_reloaded.channels[0].private);
    }

    #[tokio::test]
    async fn join_verify_pages_past_a_control_plane_flood_to_find_the_genesis() {
        // The join-verify DoS mitigation: a member floods the control address with
        // junk NEWER than the genesis, burying it past the newest-500 window. The
        // `until`-paginated verify must walk past the flood and still find the owner
        // genesis, so a legit join is not fail-closed by a griefer.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Flooded", bed.relays.clone(), None).await.unwrap();

        // A rogue member (holds the root, so can sign at the control address) buries
        // the genesis under 520 junk editions dated far in the future.
        let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let rogue = Keys::generate();
        for i in 0..520u64 {
            let rumor = control::build_edition_rumor(
                rogue.public_key(),
                vsk::CHANNEL_METADATA,
                &[0xAB; 32],
                1,
                None,
                "{\"name\":\"junk\",\"private\":false}",
                2_000_000_000 + i,
                None,
            );
            let (wrap, _) = control::seal_control_edition(&rumor, &control, &rogue, Timestamp::from_secs(2_000_000_000 + i)).unwrap();
            bed.relay.publish(&wrap, &bed.relays).await.unwrap();
        }
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();
        assert_eq!(joined.id().0, community.id().0, "pagination walks past the flood to the genesis");
    }

    /// LIVE smoke test (network) — ignored by default. Creates a v2 community on a
    /// REAL relay via `LiveTransport`, sends a message, fetches it back, and mints
    /// a public link. A fresh throwaway identity in an isolated temp data dir, so
    /// it never touches real accounts. Run explicitly:
    /// ```sh
    /// cargo test -p vector-core -- --ignored --nocapture live_smoke
    /// ```
    #[tokio::test]
    #[ignore = "hits a real relay over the network"]
    async fn live_smoke_create_send_fetch_on_a_real_relay() {
        use crate::community::transport::LiveTransport;
        use nostr_sdk::prelude::ToBech32;

        let relay = std::env::var("VECTOR_SMOKE_RELAY").unwrap_or_else(|_| "wss://jskitty.com/nostr".to_string());
        let relays = vec![relay.clone()];

        // Isolated account + data dir (a fresh throwaway key — never a real account).
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        // Bring your own key (VECTOR_SMOKE_NSEC) to create a community you can log
        // into elsewhere; otherwise a fresh throwaway.
        let keys = match std::env::var("VECTOR_SMOKE_NSEC") {
            Ok(n) => Keys::parse(&n).expect("VECTOR_SMOKE_NSEC is not a valid nsec"),
            Err(_) => Keys::generate(),
        };
        let npub = keys.public_key().to_bech32().unwrap();
        // Off by default (never leak secrets from a committed test); set
        // VECTOR_SMOKE_PRINT_NSEC=1 to print the owner nsec for cross-client login.
        if std::env::var("VECTOR_SMOKE_PRINT_NSEC").is_ok() {
            println!("[smoke] OWNER nsec (throwaway — do NOT reuse): {}", keys.secret_key().to_bech32().unwrap());
        }
        std::fs::create_dir_all(tmp.path().join(&npub)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(npub.clone()).unwrap();
        crate::db::init_database(&npub).unwrap();
        crate::state::MY_SECRET_KEY.store_from_keys(&keys, &[]);
        crate::state::set_my_public_key(keys.public_key());
        println!("[smoke] throwaway identity {npub}");

        // A live client (LiveTransport rides the global NOSTR_CLIENT + warms relays).
        let client = nostr_sdk::ClientBuilder::new().signer(keys.clone()).build();
        client.pool().add_relay(relay.as_str(), nostr_sdk::RelayOptions::default()).await.ok();
        client.connect().await;
        crate::state::set_nostr_client(client);
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(15));

        // Create → send → fetch-back → verify.
        let community = create_community(&transport, "V2 Live Smoke", relays.clone(), None).await.expect("create");
        let general = community.channels[0].id;
        println!("[smoke] created community {} on {relay}", crate::simd::hex::bytes_to_hex_32(&community.id().0));

        let text = "hello from a Vector Concord v2 live smoke test";
        let sent_id = send_message(&transport, &community, &general, text).await.expect("send");
        println!("[smoke] sent message {sent_id}");

        // Give the relay a moment to store + be ready to serve it.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        let page = fetch_channel(&transport, &community, &general, 50).await.expect("fetch");
        let texts: Vec<String> = page
            .iter()
            .filter_map(|f| match &f.event {
                ChatEvent::Message { .. } => Some(f.event.opened().rumor.content.clone()),
                _ => None,
            })
            .collect();
        println!("[smoke] fetched {} message(s) back: {texts:?}", texts.len());
        assert!(texts.contains(&text.to_string()), "the message did not round-trip through the real relay");

        // Mint a shareable v2 link (the thing a bot hands out).
        let link = mint_public_link(&transport, &community, "https://vectorapp.io", None, None).await.expect("mint link");
        println!("[smoke] invite link: {}", link.url);
        println!("[smoke] PASS — v2 create+send+fetch+invite round-tripped on {relay}");
    }
}
