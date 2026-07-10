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
    // Seed the genesis edition heads (v1) as the owner's refuse-downgrade floor, so a
    // later edit can't be rolled back by a relay serving only the genesis prefix. The
    // live control sub is replay-free (limit 0), so the owner won't re-fold its own
    // genesis to seed the floor otherwise. Floors land BEFORE the community row
    // (floors-then-state ordering).
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    for wrap in &genesis.wraps {
        if let Ok((ed, _)) = control::open_control_edition(wrap, &control) {
            let entity_hex = crate::simd::hex::bytes_to_hex_32(&ed.entity_id);
            crate::db::community::set_edition_head_at_epoch(&cid_hex, &entity_hex, ed.version, &ed.self_hash, &ed.inner_id, community.root_epoch.0)?;
        }
    }
    crate::db::community::save_community_v2(&community)?;
    // Archive the genesis root at epoch 0, so a later Refounding leaves this epoch's
    // Public-channel history readable (CORD-03 §3 multi-epoch read).
    let _ = crate::db::community::store_epoch_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, community.root_epoch.0, &community.community_root);

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

    // Sync the new membership across devices (CORD-02 §8) — best-effort.
    let _ = republish_community_list(transport).await;
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
    send_chat_message(transport, community, channel_id, content, None, &[], vec![]).await
}

/// Full chat send: threaded reply (NIP-C7 `q`, the parent's `(rumor_id, author)`
/// hex pair), NIP-30 custom-emoji pairs, and verbatim extra tags (NIP-92 `imeta`
/// attachments). Returns the message's rumor id (hex).
pub async fn send_chat_message<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    content: &str,
    reply_to: Option<(&str, &str)>,
    emoji: &[(&str, &str)],
    extra_tags: Vec<nostr_sdk::prelude::Tag>,
) -> Result<String, String> {
    let (author, group, epoch, session) = chat_send_context(community, channel_id)?;
    let at_ms = now_ms();
    let rumor = chat::build_message_rumor(author.public_key(), channel_id, epoch, content, reply_to, emoji, extra_tags, at_ms);
    publish_chat(transport, community, &session, &group, &author, channel_id, epoch, rumor, at_ms, false).await
}

/// React to a channel message (kind 7, NIP-25 shape). `target_id_hex` /
/// `target_author_hex` name the reacted-to message; `target_kind` is its rumor
/// kind (`kind::MESSAGE`, or `kind::COMMENT` for a threaded reply); `emoji`
/// carries the NIP-30 pair when `emoji_content` is a custom `:shortcode:`.
#[allow(clippy::too_many_arguments)]
pub async fn send_reaction<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    target_id_hex: &str,
    target_author_hex: &str,
    target_kind: u16,
    emoji_content: &str,
    emoji: Option<(&str, &str)>,
) -> Result<String, String> {
    let (author, group, epoch, session) = chat_send_context(community, channel_id)?;
    let at_ms = now_ms();
    let rumor =
        chat::build_reaction_rumor(author.public_key(), channel_id, epoch, target_id_hex, target_author_hex, target_kind, emoji_content, emoji, at_ms);
    publish_chat(transport, community, &session, &group, &author, channel_id, epoch, rumor, at_ms, false).await
}

/// Edit one of your own messages (kind 3302): peers re-render `target_id_hex`
/// with the replacement text. Author-enforced on the read side — only the
/// original author's edit folds.
pub async fn send_edit<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    target_id_hex: &str,
    new_content: &str,
) -> Result<String, String> {
    let (author, group, epoch, session) = chat_send_context(community, channel_id)?;
    let at_ms = now_ms();
    let rumor = chat::build_edit_rumor(author.public_key(), channel_id, epoch, target_id_hex, new_content, at_ms);
    publish_chat(transport, community, &session, &group, &author, channel_id, epoch, rumor, at_ms, false).await
}

/// Cooperative in-plane delete (kind 5, NIP-09 semantics): peers stop rendering
/// `target_id_hex`. The wrap ciphertext on relays needs a separate NIP-09 scrub
/// by its ephemeral key — not retained in this cut.
pub async fn send_delete<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    target_id_hex: &str,
    target_kind: u16,
) -> Result<String, String> {
    let (author, group, epoch, session) = chat_send_context(community, channel_id)?;
    let at_ms = now_ms();
    let rumor = chat::build_delete_rumor(author.public_key(), channel_id, epoch, target_id_hex, target_kind, at_ms);
    publish_chat(transport, community, &session, &group, &author, channel_id, epoch, rumor, at_ms, false).await
}

/// Ephemeral typing indicator (kind 23311 in a 21059 wrap — relays never store it).
pub async fn send_typing<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
) -> Result<(), String> {
    let (author, group, epoch, session) = chat_send_context(community, channel_id)?;
    let at_ms = now_ms();
    let rumor = chat::build_typing_rumor(author.public_key(), channel_id, epoch, at_ms);
    publish_chat(transport, community, &session, &group, &author, channel_id, epoch, rumor, at_ms, true).await.map(|_| ())
}

/// Everything a chat-plane send needs: local keys, the channel's group key +
/// epoch, and the session snapshot taken BEFORE any await. Refuses a dissolved
/// community (every honest member sealed it read-only) and a keyless Private
/// channel — deriving from the root would post to the public plane; its key
/// arrives over the rekey plane.
fn chat_send_context(community: &CommunityV2, channel_id: &ChannelId) -> Result<(Keys, GroupKey, Epoch, SessionGuard), String> {
    let session = SessionGuard::capture();
    let author = local_keys()?;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    if crate::db::community::get_community_dissolved(&cid_hex).unwrap_or(false) {
        return Err("this community has been dissolved".to_string());
    }
    // A self-ban: every honest peer drops our events (CORD-04 §4) and the send
    // echo would silently no-op, so fail loudly instead of a message that seems
    // to send but shows up nowhere.
    if crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default().contains(&author.public_key().to_hex()) {
        return Err("you are banned from this community".to_string());
    }
    let ch = community.channel(channel_id).ok_or("no such channel in this community")?;
    if ch.private && ch.key.is_none() {
        return Err("this private channel has no key yet (awaiting rekey delivery)".to_string());
    }
    let (secret, epoch) = community.channel_secret(ch);
    Ok((author, channel_group_key(&secret, channel_id, epoch), epoch, session))
}

/// Seal one chat rumor, re-check the session, publish, and echo the send into the
/// shared store. Returns the rumor id (hex).
#[allow(clippy::too_many_arguments)]
async fn publish_chat<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
    group: &GroupKey,
    author: &Keys,
    channel_id: &ChannelId,
    epoch: Epoch,
    rumor: nostr_sdk::prelude::UnsignedEvent,
    at_ms: u64,
    ephemeral: bool,
) -> Result<String, String> {
    let rumor_id = rumor.id.ok_or("rumor has no id")?.to_hex();
    let (wrap, _ephemeral_keys) = chat::seal_chat_rumor(&rumor, group, author, Timestamp::from_secs(at_ms / 1000), ephemeral)
        .map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed before send".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    // Local echo (v1 parity): open our OWN wrap through the exact inbound path so
    // send-then-read works with no listen loop, and the relay's re-delivery dedups
    // against this row instead of re-firing callbacks. Best-effort — the publish
    // already succeeded. Ephemeral kinds (typing) apply to nothing and skip out.
    if !ephemeral {
        if let Ok(event) = chat::open_chat_event(&wrap, group, channel_id, epoch) {
            let channel_hex = crate::simd::hex::bytes_to_hex_32(&channel_id.0);
            let outcome = {
                let mut st = crate::state::STATE.lock().await;
                if !session.is_valid() {
                    return Ok(rumor_id); // swapped on the lock await — never echo into another account.
                }
                super::inbound::apply_chat_to_state(&mut st, &event, &channel_hex, &author.public_key())
            };
            if let Some(outcome) = outcome {
                if !session.is_valid() {
                    return Ok(rumor_id);
                }
                super::inbound::persist_chat(&channel_hex, &outcome).await;
            }
        }
    }
    Ok(rumor_id)
}

/// A chat event opened from a channel fetch, tagged with the epoch its key
/// decrypted under.
pub struct FetchedEvent {
    pub event: ChatEvent,
    pub epoch: Epoch,
}

/// Fetch a channel's newest messages — one page of [`fetch_channel_history`].
/// `limit` is one relay-side bound across the whole epoch-author OR-set, not
/// per epoch; deeper history pages backwards via the walk.
pub async fn fetch_channel<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    limit: usize,
) -> Result<Vec<FetchedEvent>, String> {
    fetch_channel_history(transport, community, channel_id, limit, 1, |_| true).await
}

/// Walk a channel's history newest-first (CORD-03 §3 "clients load a Channel
/// newest-first and paginate backwards"), querying every held epoch's Chat-Plane
/// address one `page`-sized query at a time until `max_pages`, a drained relay,
/// or `keep_paging` returns false for a page (the caller's "I already hold
/// these" early stop — consulted only on pages that opened something, so junk
/// at the address can't fake exhaustion). Pages step by INCLUSIVE `until` with
/// wrap-id dedup, so a page boundary landing mid-second can't skip siblings; a
/// full page of only-already-seen wraps is a same-second WALL (relay filters
/// are second-granular) and steps past it accepting that unseen same-second
/// siblings beyond the relay cap are unreachable — logged, and a protocol-level
/// limitation (the `ms` tag can't be filtered server-side).
///
/// Returns everything opened, deduped by rumor id, oldest→newest.
pub async fn fetch_channel_history<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    page: usize,
    max_pages: usize,
    mut keep_paging: impl FnMut(&[FetchedEvent]) -> bool,
) -> Result<Vec<FetchedEvent>, String> {
    let ch = community.channel(channel_id).ok_or("no such channel in this community")?;
    // A Public channel reads across EVERY held base-root epoch, and a Private one
    // across its OWN held epochs (CORD-03 §3), so history spanning a rotation stays
    // continuous either way. A keyless Private channel is unreadable — never derived
    // from the root (that would address the public plane).
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let coords: Vec<([u8; 32], Epoch)> = if ch.private {
        let Some(current) = ch.key else {
            return Ok(Vec::new());
        };
        let ch_hex = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
        let mut held = crate::db::community::held_epoch_keys(&cid_hex, &ch_hex).unwrap_or_default();
        if !held.iter().any(|(ep, _)| *ep == ch.epoch) {
            held.push((ch.epoch, current));
        }
        // Only real grants are archived, but keep the invariant local: a private
        // plane is never read with the root value.
        held.into_iter().filter(|(_, k)| *k != community.community_root).map(|(ep, k)| (k, ep)).collect()
    } else {
        let mut roots = crate::db::community::held_epoch_keys(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap_or_default();
        if !roots.iter().any(|(ep, _)| *ep == community.root_epoch) {
            roots.push((community.root_epoch, community.community_root));
        }
        roots.into_iter().map(|(ep, root)| (root, ep)).collect()
    };
    if coords.is_empty() {
        return Ok(Vec::new());
    }

    // Address every held epoch by its Chat-Plane pubkey.
    let authors: Vec<String> = coords
        .iter()
        .map(|(secret, epoch)| channel_group_key(secret, channel_id, *epoch).pk_hex())
        .collect();

    let mut seen_wraps: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    let mut seen_rumors = std::collections::HashSet::new();
    let mut out: Vec<(u64, FetchedEvent)> = Vec::new();
    let mut until: Option<u64> = None;
    let mut oldest: Option<u64> = None;
    for _ in 0..max_pages {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: authors.clone(),
            until,
            limit: Some(page),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, &community.relays).await?;
        if wraps.is_empty() {
            break;
        }
        let mut fresh = 0usize;
        let mut page_events: Vec<FetchedEvent> = Vec::new();
        for wrap in &wraps {
            if !seen_wraps.insert(wrap.id) {
                continue;
            }
            fresh += 1;
            let at = wrap.created_at.as_secs();
            if oldest.is_none_or(|o| at < o) {
                oldest = Some(at);
            }
            // Select the epoch whose group key authored this wrap (no trial decrypt).
            for (secret, epoch) in &coords {
                let group = channel_group_key(secret, channel_id, *epoch);
                if wrap.pubkey != group.pk() {
                    continue;
                }
                if let Ok(event) = chat::open_chat_event(wrap, &group, channel_id, *epoch) {
                    let id = event.opened().rumor_id;
                    if seen_rumors.insert(id) {
                        page_events.push(FetchedEvent { event, epoch: *epoch });
                    }
                }
                break;
            }
        }
        if fresh == 0 {
            if wraps.len() < page {
                break; // drained — the relay has nothing older.
            }
            // A full page of already-seen wraps: a same-second WALL. Step past it;
            // same-second siblings beyond the relay's cap are unreachable by a
            // second-granular filter.
            let Some(o) = oldest else { break };
            if o == 0 {
                break;
            }
            crate::log_warn!("v2: same-second history wall at {o} — stepping past it (messages beyond the relay page cap in that second are unreachable)");
            until = Some(o - 1);
            continue;
        }
        let stop = !page_events.is_empty() && !keep_paging(&page_events);
        out.extend(page_events.into_iter().map(|e| (e.event.opened().at_ms, e)));
        if stop {
            break; // the caller holds everything from here back.
        }
        until = oldest; // inclusive — wrap-id dedup absorbs the boundary overlap.
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
        // A KEYLESS private channel can't be granted (we hold no key) — carrying the
        // root placeholder would make the joiner classify it PUBLIC and address a
        // private channel at the public plane. It joins their view via control-follow
        // (keyless) and keys up at the channel's next rotation.
        .filter(|c| !(c.private && c.key.is_none()))
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
    let bundle = bundle_of(community, Some(local_keys()?.public_key()), expires_at_ms, label.clone());
    let bundle_key = super::derive::invite_bundle_key(&token);
    let bundle_event = invite::build_bundle_event(&link_signer, &bundle, &bundle_key).map_err(|e| e.to_string())?;
    let url = invite::build_invite_url(base, &link_signer.public_key(), &token, &community.relays).map_err(|e| e.to_string())?;

    if !session.is_valid() {
        return Err("account changed before minting link".to_string());
    }
    transport.publish_durable(&bundle_event, &community.relays).await?;
    let minted = MintedLink { url, bundle_event, link_signer, token };
    // Sync the link across the creator's devices (13303) + publish the Registry
    // (vsk-8) so members see the community is Public. Best-effort — the link works
    // without the sync.
    let _ = record_minted_link(transport, community, &minted).await;
    // Local mirror so `list_public_invites` stays a sync local read (v1 parity);
    // the 13303 list remains the cross-device record. Re-check the session: the
    // publishes above straddled awaits, and this write must not land account A's
    // link (secret token included) in a swapped-in account's DB.
    if session.is_valid() {
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let token_hex = crate::simd::hex::bytes_to_hex_16(&minted.token);
        let _ = crate::db::community::save_public_invite(&token_hex, &cid_hex, &minted.url, expires_at_ms.map(|e| e as i64), label.as_deref());
    }
    Ok(minted)
}

// ── The Invite Registry (vsk 8) + Invite List (13303), CORD-05 §4/§5 ──────────

/// Fetch the creator's own 13303 Invite List from `relays` (newest wins; a
/// decrypt/parse failure is "no news", never a clobber of the local mirror).
async fn fetch_invite_list<T: Transport + ?Sized>(transport: &T, relays: &[String]) -> Option<invite::InviteList> {
    let me = local_keys().ok()?;
    let query = Query {
        kinds: vec![super::kind::INVITE_LIST],
        authors: vec![me.public_key().to_hex()],
        limit: Some(4),
        ..Default::default()
    };
    let events = transport.fetch(&query, relays).await.ok()?;
    events
        .into_iter()
        .filter_map(|e| invite::parse_invite_list_event(&e, &me).ok().map(|l| (e.created_at.as_secs(), l)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, l)| l)
}

/// The creator's LIVE (non-tombstoned) link-signer pubkeys for one community — the
/// Registry's content (CORD-05 §5), derived from the stored link secrets.
fn live_signers_for(list: &invite::InviteList, community_id_hex: &str) -> Vec<PublicKey> {
    let dead: std::collections::HashSet<&str> = list.tombstones.iter().map(|t| t.token.as_str()).collect();
    list.entries
        .iter()
        .filter(|e| e.community_id == community_id_hex && !dead.contains(e.token.as_str()))
        .filter_map(|e| Keys::parse(&e.signer_sk).ok().map(|k| k.public_key()))
        .collect()
}

/// Publish the creator's Registry (vsk-8) edition — their live link signers for this
/// community — so members fold it into the Public/Private source of truth (a
/// non-empty aggregate = Public).
async fn publish_invite_registry<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, session: &SessionGuard, live_signers: &[PublicKey]) -> Result<(), String> {
    let me = local_keys()?;
    let eid = super::derive::invite_links_locator(community.id(), &me.public_key().to_bytes());
    let content = invite::build_registry_content(live_signers);
    publish_control_edition(transport, community, session, vsk::INVITE_LINKS, &eid, &content, None).await
}

/// Record a freshly-minted public link across the creator's devices: append it to the
/// 13303 Invite List and refresh the Registry (CORD-05 §4/§5).
async fn record_minted_link<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, minted: &MintedLink) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let token_hex = crate::simd::hex::bytes_to_hex_16(&minted.token);
    let mut list = fetch_invite_list(transport, &community.relays).await.unwrap_or_default();
    if !list.entries.iter().any(|e| e.token == token_hex) {
        list.entries.push(invite::InviteEntry {
            token: token_hex,
            signer_sk: minted.link_signer.secret_key().to_secret_hex(),
            community_id: cid_hex.clone(),
            url: minted.url.clone(),
            label: None,
            created_at: now_ms() / 1000,
            expires_at: None,
            extra: Default::default(),
        });
    }
    if !session.is_valid() {
        return Err("account changed during link record".to_string());
    }
    let event = invite::build_invite_list_event(&me, &list).map_err(|e| e.to_string())?;
    transport.publish(&event, &community.relays).await?;
    let signers = live_signers_for(&list, &cid_hex);
    publish_invite_registry(transport, community, &session, &signers).await
}

/// Revoke a public link by its token hex (CORD-05 §2/§5): re-post its coordinate as a
/// revocation tombstone (retiring the bundle behind the URL, so a fetcher finds the
/// grave), tombstone the Invite List entry, and refresh the Registry. Retiring the
/// LAST live link empties the Registry → the community reads Private (a Refounding is
/// the owner's separate read-cut).
pub async fn revoke_public_link<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, token_hex: &str) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let mut list = fetch_invite_list(transport, &community.relays).await.ok_or("no invite list found to revoke from")?;
    let entry = list
        .entries
        .iter()
        .find(|e| e.token == token_hex && e.community_id == cid_hex)
        .cloned()
        .ok_or("no such link in the invite list")?;
    // Re-post the bundle coordinate as a revocation tombstone (creator-signed).
    let link_signer = Keys::parse(&entry.signer_sk).map_err(|_| "malformed link signer")?;
    let revocation = invite::build_revocation(&link_signer).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed during revoke".to_string());
    }
    transport.publish_durable(&revocation, &community.relays).await?;
    // Tombstone the Invite List entry (permanent — a stale device can't resurrect it).
    list.tombstones.push(invite::InviteTombstone { token: token_hex.to_string(), community_id: cid_hex.clone(), extra: Default::default() });
    list.entries.retain(|e| e.token != token_hex);
    let event = invite::build_invite_list_event(&me, &list).map_err(|e| e.to_string())?;
    transport.publish(&event, &community.relays).await?;
    let signers = live_signers_for(&list, &cid_hex);
    publish_invite_registry(transport, community, &session, &signers).await?;
    // Drop the local mirror row (sibling of the mint-time save) — only if still our session.
    if session.is_valid() {
        let _ = crate::db::community::delete_public_invite(token_hex);
    }
    Ok(())
}

/// Refresh every live public link's bundle behind its stable URL (CORD-05 §2) — e.g.
/// after a Rekey/Refounding rolled the keys — by re-posting the bundle at the same
/// coordinate with the CURRENT community state, so a link shared once keeps working
/// across rotations. Best-effort.
pub async fn refresh_public_links<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let Some(list) = fetch_invite_list(transport, &community.relays).await else {
        return Ok(());
    };
    let creator = local_keys()?.public_key();
    let dead: std::collections::HashSet<&str> = list.tombstones.iter().map(|t| t.token.as_str()).collect();
    for entry in &list.entries {
        if entry.community_id != cid_hex || dead.contains(entry.token.as_str()) || entry.token.len() != 2 * super::derive::TOKEN_LEN {
            continue;
        }
        let Ok(link_signer) = Keys::parse(&entry.signer_sk) else { continue };
        let token = crate::simd::hex::hex_to_bytes_16(&entry.token);
        let bundle = bundle_of(community, Some(creator), entry.expires_at, entry.label.clone());
        let bundle_key = super::derive::invite_bundle_key(&token);
        if let Ok(event) = invite::build_bundle_event(&link_signer, &bundle, &bundle_key) {
            if !session.is_valid() {
                return Err("account changed during link refresh".to_string());
            }
            let _ = transport.publish_durable(&event, &community.relays).await;
        }
    }
    Ok(())
}

/// Whether this community is PUBLIC (CORD-05 §5): fold every creator's Registry
/// (vsk-8) that its author is authorized for (`CREATE_INVITE`, bound to their
/// coordinate) into an aggregate live-link set — non-empty ⇒ a live link exists ⇒
/// Public; empty ⇒ Private. Retiring the last link is what flips it back.
pub async fn community_is_public<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> bool {
    use crate::community::roles::Permissions;
    use std::collections::BTreeMap;
    let Ok(owner) = community.owner() else { return false };
    let owner_hex = owner.to_hex();
    let cid = community.id();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid.0);
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();
    let control = control_group_key(&community.community_root, cid, community.root_epoch);
    let query = Query { kinds: vec![stream::KIND_WRAP], authors: vec![control.pk_hex()], limit: Some(FOLLOW_PAGE), ..Default::default() };
    let editions: Vec<ParsedEdition> = transport
        .fetch(&query, &community.relays)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|w| control::open_control_edition(w, &control).ok().map(|(ed, _)| ed))
        .collect();
    let authority = fold_authority(community, &editions, &floors);

    let mut by_eid: BTreeMap<[u8; 32], Vec<&ParsedEdition>> = BTreeMap::new();
    for e in &editions {
        if e.vsk == vsk::INVITE_LINKS {
            by_eid.entry(e.entity_id).or_default().push(e);
        }
    }
    for (eid, group) in &by_eid {
        let fold_eds: Vec<version::Edition> = group.iter().map(|p| p.to_fold_edition()).collect();
        let (Some(hi), _) = fold_head(&fold_eds, floors.get(&crate::simd::hex::bytes_to_hex_32(eid))) else { continue };
        let head = group[hi];
        // The creator must hold CREATE_INVITE AND own this coordinate.
        if !authority.roles.is_authorized(&head.author.to_hex(), Some(&owner_hex), Permissions::CREATE_INVITE) {
            continue;
        }
        if super::derive::invite_links_locator(cid, &head.author.to_bytes()) != *eid {
            continue;
        }
        if invite::parse_registry_content(&head.content).map(|s| !s.is_empty()).unwrap_or(false) {
            return true;
        }
    }
    false
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
    let (community, join_heads) = verify_owner_root_and_reconcile(transport, community).await?;

    // A dissolved community is a grave (CORD-02 §9): refuse to join it.
    if is_dissolved(transport, &community).await {
        return Err("this community has been dissolved".to_string());
    }

    // The account must not have swapped since the guard was captured (which was
    // before any fetch the caller / the verify above performed) — else we'd write
    // A's join into B.
    if !session.is_valid() {
        return Err("account changed during join".to_string());
    }
    // Seed the verified heads as the initial refuse-downgrade floor BEFORE the
    // community row lands (floors-then-state, so a mid-seed error can't leave saved
    // state outrunning its floor); the first post-join follow then can't persist a
    // state below what this join already showed.
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    for h in &join_heads {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, community.root_epoch.0)?;
    }
    crate::db::community::save_community_v2(&community)?;
    // Archive the joined root at its epoch, so this member reads Public-channel
    // history from their join epoch onward across later Refoundings (CORD-03 §3).
    let _ = crate::db::community::store_epoch_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, community.root_epoch.0, &community.community_root);
    // Same for each granted Private-channel key: the archive is what lets its
    // history stay readable after the channel rotates away from this key.
    for ch in &community.channels {
        if let (true, Some(key)) = (ch.private, ch.key) {
            let _ = crate::db::community::store_epoch_key(&cid_hex, &crate::simd::hex::bytes_to_hex_32(&ch.id.0), ch.epoch.0, &key);
        }
    }

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

    // Record the membership across devices (CORD-02 §8) — best-effort.
    let _ = republish_community_list(transport).await;
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
) -> Result<(CommunityV2, Vec<FoldedHead>), String> {
    let owner = community.owner()?;
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let control_pk = control.pk_hex();

    // Authenticity = the owner's GENESIS metadata edition (vsk-0, `eid ==
    // community_id`) at the root-derived control plane. The genesis eid pins it to
    // THIS community, and it lives ONLY under the real root — so a forged root can't
    // produce one: an edition's seal carries no community binding, but another
    // community's genesis has a different eid, and this community's own genesis is
    // unreadable without its real root (which the forger lacks). ("Any owner edition"
    // is NOT sound: an owner sig from any co-owned community, rewrapped onto the fake
    // plane, would pass — reopening the eclipse.) The residual — a T-member replaying
    // T's genesis onto a fake root to MITM another T-joiner — is closed only by
    // binding the root into community_id (protocol, deferred).
    //
    // Seed `until` with a FAR-FUTURE constant (NOT now-based): `until.is_some()` takes
    // the transport's AUTHORITATIVE drain-ALL-relays path (an open `until` returns only
    // a fast relay's partial window and misses a genesis on a lagging relay — routine
    // over Tor), while a constant beyond any real created_at clips NOTHING — so neither
    // a clock-skewed future-dated genesis nor a >1h-slow-clock joiner is excluded (a
    // now-based bound could clip either). Break on an EMPTY page (a short page is a
    // relay cap). A forged root walks to exhaustion and rejects; a flood/deep plane
    // that buries the genesis past the walk is the deferred protocol residual.
    const PAGE: usize = 500;
    const MAX_PAGES: usize = 4;
    const FAR_FUTURE_SECS: u64 = 4_102_444_800; // ~year 2100 — above any real edition, safe as a relay `until`.
    let mut editions: Vec<ParsedEdition> = Vec::new();
    let mut found_genesis = false;
    let mut until: Option<u64> = Some(FAR_FUTURE_SECS);
    let mut seen_wraps: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    for _ in 0..MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![control_pk.clone()],
            until,
            limit: Some(PAGE),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, &community.relays).await?;
        // INCLUSIVE `until` + wrap-id dedup: a `-1` step can skip same-second
        // siblings at a page boundary (and the genesis with them); re-served
        // boundary events are free, and no-new-events means exhausted.
        let mut oldest = u64::MAX;
        let mut fresh = 0usize;
        for w in &wraps {
            if !seen_wraps.insert(w.id) {
                continue;
            }
            fresh += 1;
            oldest = oldest.min(w.created_at.as_secs());
            if let Ok((ed, _)) = control::open_control_edition(w, &control) {
                if ed.author == owner {
                    if ed.vsk == vsk::COMMUNITY_METADATA && ed.entity_id == community.id().0 {
                        found_genesis = true;
                    }
                    editions.push(ed);
                }
            }
        }
        if found_genesis || fresh == 0 {
            break; // authenticated (the owner genesis), or the relay is exhausted.
        }
        until = Some(oldest);
    }
    if !found_genesis {
        return Err(
            "could not verify this community from its relays (the invite may be forged, the relays are unreachable, or the control plane is being flooded); not joining"
                .to_string(),
        );
    }
    // Join-time reconcile: the joiner holds no floors yet (empty map → bootstrap per
    // entity). The heads this fold verified are returned for the caller to SEED as
    // the initial floor once the community row is saved — without that, the first
    // post-join follow would bootstrap floor-less and could persist a state BELOW
    // what this join already verified and showed.
    // Join-time reconcile folds only the owner's editions (genesis-authenticated
    // above), and the owner is supreme — so owner-only authority suffices. The full
    // roster (admins) folds on the first post-join follow_control.
    let empty_floors = Floors::new();
    let authority = AuthoritySet::owner_only();
    let fold = apply_control_fold(&community, &editions, &empty_floors, &authority);
    Ok((fold.updated.unwrap_or(community), fold.heads))
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

/// Accept a PARKED Direct Invite from its stored bundle JSON (the wrap was already
/// unwrapped + owner-verified at park time). Re-parses through the same fail-closed
/// bundle validation, then runs the shared accept path (which re-verifies the owner
/// root over the network). `inviter_hex` is the parked seal signer, for Guestbook
/// Join attribution.
pub async fn accept_parked_invite<T: Transport + ?Sized>(
    transport: &T,
    bundle_json: &str,
    inviter_hex: Option<&str>,
) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let bundle = CommunityInvite::from_bundle_json(bundle_json).map_err(|e| e.to_string())?;
    let invited_by = inviter_hex.and_then(|h| PublicKey::parse(h).ok());
    accept_bundle(transport, &session, &bundle, invited_by).await
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
    for event in &events {
        match invite::parse_bundle_event(event, &parsed.link_signer, &bundle_key) {
            Ok(invite::BundleState::Revoked) => return Err("this invite link has been revoked".to_string()),
            Ok(invite::BundleState::Live(bundle)) => {
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
    // Tombstone the membership across devices (CORD-02 §8) BEFORE the local delete,
    // to the leaving community's own relays (it's about to be gone locally) —
    // best-effort.
    let _ = tombstone_community_list(transport, community.id(), &community.relays).await;
    // The tombstone publish straddled an await — never delete from a swapped-in DB.
    if !session.is_valid() {
        return Err("account changed during leave".to_string());
    }
    crate::db::community::delete_community(&crate::simd::hex::bytes_to_hex_32(&community.id().0))?;
    Ok(())
}

/// Cooperative Kick (CORD-04 §6, Guestbook plane): name the target; every reader
/// honors it iff the signer holds KICK and strictly outranks them (the coalesce's
/// `can_kick`), so publishing without authority is inert. A kicked member may
/// rejoin with a fresh invite — cryptographic severance is the ban/refound path.
pub async fn kick_member<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, target: &PublicKey) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    // Fast local pre-check; readers re-verify independently. The `vac` citation
    // rides with the deferred citation-completeness pass (owner needs none).
    let authority = fetch_authority(transport, community).await;
    let owner_hex = community.owner()?.to_hex();
    if !authority.roles.can_act_on_member(
        &me.public_key().to_hex(),
        Some(&owner_hex),
        &target.to_hex(),
        crate::community::roles::Permissions::KICK,
    ) {
        return Err("not authorized to kick this member".to_string());
    }
    let at_ms = now_ms();
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let rumor = guestbook::build_kick_rumor(me.public_key(), *target, None, at_ms);
    let (wrap, _) = guestbook::seal_guestbook_rumor(&rumor, &gb_group, &me, Timestamp::from_secs(at_ms / 1000))
        .map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed before send".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    Ok(())
}

/// A community's folded, delegation-authorized authority — the on-demand read
/// view (a paged control-plane fetch + fold, nothing persisted). `roles` is the
/// owner-seeded authorized roster (shared algebra with v1); `banned` the
/// enforced banlist. `floored`/`head_entities` let a writer detect a WITHHELD
/// entity (floored locally but no head folded) before replacing it blind.
pub struct AuthorityView {
    pub roles: crate::community::roles::CommunityRoles,
    pub banned: std::collections::BTreeSet<String>,
    /// Any authority entity's fold hit a floor gap (withheld / evicted link).
    pub gapped: bool,
    /// Entity hexes holding a persisted floor at this epoch (all vsk kinds).
    pub floored: std::collections::BTreeSet<String>,
    /// Authority entities (role/grant/banlist) that folded a head this fetch.
    pub head_entities: std::collections::BTreeSet<String>,
}

/// Fetch + fold the community's current authority (CORD-04), paging older like
/// `follow_control` while the fold is gapped so a busy control plane can't push
/// the roster off the newest window. A fetch failure degrades fail-safe:
/// owner-only authority plus the PERSISTED banlist — nobody gains standing from
/// an outage, and a ban never lifts on withheld data.
pub async fn fetch_authority<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> AuthorityView {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);

    let mut editions: Vec<ParsedEdition> = Vec::new();
    let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    let mut seen_wraps: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    let mut oldest: Option<u64> = None;
    let mut until: Option<u64> = None;
    // Seed from an EMPTY fold, not owner_only(): a fold over zero editions yields
    // owner-only roles AND retains the PERSISTED banlist. So a first-page transport
    // error returns the stored bans (fail-safe), never an empty banlist that would
    // silently un-ban on withheld data.
    let mut a = fold_authority(community, &[], &floors);
    for _ in 0..FOLLOW_MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![control.pk_hex()],
            until,
            limit: Some(FOLLOW_PAGE),
            ..Default::default()
        };
        let Ok(wraps) = transport.fetch(&query, &community.relays).await else { break };
        let mut fresh = 0usize;
        for w in &wraps {
            if !seen_wraps.insert(w.id) {
                continue;
            }
            fresh += 1;
            let at = w.created_at.as_secs();
            if oldest.is_none_or(|o| at < o) {
                oldest = Some(at);
            }
            if let Ok((ed, _)) = control::open_control_edition(w, &control) {
                if seen.insert(ed.inner_id) {
                    editions.push(ed);
                }
            }
        }
        a = fold_authority(community, &editions, &floors);
        if !a.gapped || fresh == 0 {
            break;
        }
        until = oldest;
    }
    AuthorityView {
        roles: a.roles,
        banned: a.banned,
        gapped: a.gapped,
        floored: floors.keys().cloned().collect(),
        head_entities: a.heads.iter().map(|h| h.entity_hex.clone()).collect(),
    }
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

    // Fold the Control Plane roster + banlist (CORD-04) for Kick authority and the
    // ban subtraction. A control fetch failure degrades to owner-only authority + no
    // bans (fail-open on availability is safe here: a Kick still needs a real signer,
    // and a missed ban only fails to HIDE, never to wrongly admit authority).
    let authority = fetch_authority(transport, community).await;
    let owner_hex = owner.to_hex();

    // Snapshot authority (CORD-02 §5): a refounding rolls `root_epoch` and re-seeds the
    // new epoch's Guestbook with a 3312 snapshot of the survivors. Refounding is OWNER-only,
    // so the owner is the refounder whose snapshot is honored — without this, every silent
    // survivor vanishes from the memberlist until they re-post. A genesis community
    // (root_epoch 0) has no refounder, hence no snapshot power.
    let snapshot_authority = (community.root_epoch.0 > 0).then_some(&owner);
    // Kick authority (CORD-04 §6): the signer must hold KICK AND strictly outrank the
    // target (the owner is supreme; equal cannot kick equal).
    let can_kick = |actor: &PublicKey, target: &PublicKey| {
        authority
            .roles
            .can_act_on_member(&actor.to_hex(), Some(&owner_hex), &target.to_hex(), crate::community::roles::Permissions::KICK)
    };
    let coalesced = guestbook::coalesce(&events, now_ms(), snapshot_authority, &can_kick);
    // The authorized banlist, as pubkeys (a malformed hex entry is simply dropped).
    let banlist: std::collections::BTreeSet<PublicKey> =
        authority.banned.iter().filter_map(|h| PublicKey::from_hex(h).ok()).collect();
    let mut members = guestbook::complete_memberlist(&coalesced, &observed, &banlist);
    // The owner is a member by definition, independent of any fetched Join.
    if !banlist.contains(&owner) {
        members.insert(owner);
    }
    Ok(members.into_iter().collect())
}

// ── Dissolution (CORD-02 §9) ─────────────────────────────────────────────────

/// Owner dissolution / "Delete Community" (CORD-02 §9): publish the terminal
/// tombstone at the dissolved plane (`community_id`-derived, epoch-free, so every
/// past or present member resolves the same grave and a Refounding can never strand
/// it). The tombstone's presence IS the state; only the owner's seal counts.
/// Irreversible — on success the local hold is sealed read-only.
pub async fn dissolve_community<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    if community.owner()? != me.public_key() {
        return Err("only the owner can dissolve a community".to_string());
    }
    let at = now_ms() / 1000;
    let rumor = super::dissolution::dissolved_tombstone_rumor(me.public_key(), community.id(), at);
    let wrap = super::dissolution::seal_dissolved(&rumor, community.id(), &me, Timestamp::from_secs(at)).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed during dissolve".to_string());
    }
    // Durable broadcast: death must propagate (a rekey racing a dissolution loses).
    transport.publish_durable(&wrap, &community.relays).await?;
    crate::db::community::set_community_dissolved(&crate::simd::hex::bytes_to_hex_32(&community.id().0))?;
    Ok(())
}

/// Whether a valid owner-signed dissolution tombstone exists for this community on
/// its relays (CORD-02 §9). A join refuses a dead community, and a live follow seals
/// on sight. Fail-OPEN on a fetch error (absence of proof is not death), but any
/// owner-verified tombstone found is authoritative.
pub async fn is_dissolved<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> bool {
    let group = super::derive::dissolved_group_key(community.id());
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![group.pk_hex()],
        limit: Some(20),
        ..Default::default()
    };
    let Ok(wraps) = transport.fetch(&query, &community.relays).await else {
        return false;
    };
    wraps.iter().any(|w| super::dissolution::verify_dissolved(w, &community.identity))
}

// ── Refounding (CORD-06 §3) ──────────────────────────────────────────────────

/// Owner/admin Refounding (CORD-06 §3): roll the `community_root` to
/// cryptographically remove `removed` from a Private community (a Ban's read-cut).
/// Compacts the Control Plane under the new root (re-wraps each head VERBATIM — the
/// inner owner/actor signatures survive, so no re-authoring), rekeys the base plus
/// every Private channel (each sealed under the PRIOR root, D2, so a base-fork loser
/// can still open them), and seeds the new epoch's Guestbook snapshot. Requires BAN.
///
/// **Acquire-before-commit:** the compaction is fetched + re-sealed BEFORE any
/// publish, and a head we can't fetch ABORTS with ZERO published state — so a
/// transient miss never strands a published rekey with a half-anchored plane.
pub async fn refound_community<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, removed: &[PublicKey]) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let cid = community.id();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid.0);
    // Death wins every race: a dissolved community never re-founds (CORD-02 §9).
    if crate::db::community::get_community_dissolved(&cid_hex).unwrap_or(false) {
        return Err("this community has been dissolved; it cannot be re-founded".to_string());
    }
    let me = local_keys()?;
    // Serialize with the follow worker for the whole rotation: the commit tail
    // whole-row-saves, and an unserialized concurrent follow could otherwise be
    // rolled back (or adopt a half-published sibling of this very rotation).
    let lock = super::realtime::follow_lock(cid);
    let _guard = lock.lock().await;
    // Reload the FRESHEST base state: a stale caller struct would address the rotation
    // under a superseded root (a base fork with no heal). The community_id is
    // self-certifying + stable, so re-loading by it is safe.
    let fresh = crate::db::community::load_community_v2(cid)?.ok_or("community gone before re-founding")?;
    let community = &fresh;
    let owner = community.owner()?;

    // OWNER-ONLY send: the receive counterpart (`advance_scope`) honors ONLY the
    // owner's rotation, so a non-owner BAN-holder's Refounding would fork onto a root
    // nobody follows and fail to sever the target. A non-owner's ban still silences
    // (Banlist) + strips authority (Grant); the read-cut is the owner's action alone
    // (CORD-06 §3 partial-removal degradation). Owner ⊃ BAN (supreme), so this is the
    // stricter gate.
    if me.public_key() != owner {
        return Err("only the owner can re-found (the cryptographic read-cut)".to_string());
    }

    // Fold the current roster: the opened editions are reused for the compaction (their
    // seals re-wrap under the new epoch), and the roster gates which admin-authored
    // heads carry forward.
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)?
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();
    let current_control = control_group_key(&community.community_root, cid, community.root_epoch);
    // Page the ENTIRE control plane, not just the newest window: the compaction MUST
    // carry EVERY committed (floored) entity to the new epoch, so a head buried under a
    // flood of newer editions (100 roles + 400 grants already exceeds one page) or a
    // head a relay withholds can't silently drop. CORD-06 §3 mandates aborting if the
    // Refounder cannot fold all Control Events — a dropped Banlist would unban a member
    // at the new epoch a fresh joiner bootstraps.
    let mut opened: Vec<(ParsedEdition, super::stream::OpenedStream)> = Vec::new();
    let mut seen_wraps: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    let mut oldest: Option<u64> = None;
    let mut until: Option<u64> = None;
    for _ in 0..FOLLOW_MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![current_control.pk_hex()],
            until,
            limit: Some(FOLLOW_PAGE),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, &community.relays).await?;
        let mut fresh = 0usize;
        for w in &wraps {
            if !seen_wraps.insert(w.id) {
                continue;
            }
            fresh += 1;
            let at = w.created_at.as_secs();
            if oldest.is_none_or(|o| at < o) {
                oldest = Some(at);
            }
            if let Ok(parsed) = control::open_control_edition(w, &current_control) {
                opened.push(parsed);
            }
        }
        // Page until every committed entity has its editions in hand (raw coverage),
        // so the floor-driven compaction below can fold each head.
        let present: std::collections::HashSet<String> =
            opened.iter().map(|(e, _)| crate::simd::hex::bytes_to_hex_32(&e.entity_id)).collect();
        if floors.keys().all(|k| present.contains(k)) || fresh == 0 {
            break;
        }
        until = oldest;
    }

    let prev_epoch = community.root_epoch;
    let new_epoch = Epoch(prev_epoch.0.checked_add(1).ok_or("root epoch overflow")?);
    let prev_commit = super::derive::epoch_key_commitment(prev_epoch, &community.community_root);
    // Mint-or-REUSE the new root, keyed by (scope, new_epoch) and archived BEFORE any
    // publish: a retried Refounding re-delivers the SAME root at this epoch/address, so
    // it can't double-mint two roots a receiver's correlation dedup would collapse into
    // a permanent fork (CORD-06 §3 idempotency).
    let new_root = mint_or_reuse_rotation_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, new_epoch.0)?;
    let new_control = control_group_key(&new_root, cid, new_epoch);
    let at = now_ms();
    let at_secs = at / 1000;

    // ACQUIRE + COVERAGE GATE (CORD-06 §3 MUST): re-wrap the head of EVERY committed
    // (floored) entity under the new epoch — FLOOR-driven, so nothing silently drops,
    // including entities the metadata/roster folds don't touch (the invite Registry
    // vsk-8, whose coordinate survives the rekey per CORD-05 §5). A floor whose head
    // can't be folded (buried past the pager / withheld) ABORTS before any publish.
    use std::collections::BTreeMap;
    let mut by_eid: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (e, _)) in opened.iter().enumerate() {
        by_eid.entry(crate::simd::hex::bytes_to_hex_32(&e.entity_id)).or_default().push(i);
    }
    let mut carried: Vec<(FoldedHead, Event)> = Vec::new();
    for (floor_key, floor) in &floors {
        // Re-wrap the AUTHORIZED head — the exact edition the persisted floor commits to
        // (its self_hash). The floor advances ONLY to authorized heads (author-aware fold),
        // so matching it is authority-correct across EVERY entity type. `fold_head`'s
        // version-chain TIP is author-BLIND: a member can seal a forged higher-version
        // edition chaining onto the floor, which the tip would carry and honest folders
        // then DROP as unauthorized — silently suppressing that role/grant/banlist across
        // the refounding. Abort if the committed head isn't served (fail-closed).
        let head_idx = by_eid
            .get(floor_key)
            .and_then(|v| v.iter().copied().find(|&i| opened[i].0.self_hash == floor.1));
        let Some(head_idx) = head_idx else {
            return Err(format!("re-founding aborted: the committed head of control entity {floor_key} (v{}) was not served; no state published", floor.0));
        };
        let (head_ed, head_os) = &opened[head_idx];
        let h = FoldedHead { entity_hex: floor_key.clone(), version: head_ed.version, self_hash: head_ed.self_hash, inner_id: head_ed.inner_id };
        let (rewrapped, _) = super::stream::rewrap_seal(&head_os.seal, &new_control, Timestamp::from_secs(at_secs)).map_err(|e| e.to_string())?;
        carried.push((h, rewrapped));
    }
    if !session.is_valid() {
        return Err("account changed during re-founding acquire".to_string());
    }

    // Recipients: the current members minus `removed`, plus me (multi-device).
    let members = memberlist(transport, community).await?;
    let removed_set: std::collections::HashSet<[u8; 32]> = removed.iter().map(|p| p.to_bytes()).collect();
    let mut recipients: Vec<PublicKey> = members.into_iter().filter(|m| !removed_set.contains(&m.to_bytes())).collect();
    if !recipients.iter().any(|p| *p == me.public_key()) {
        recipients.push(me.public_key());
    }

    // Base rekey blobs (the new root to each recipient), sealed under the PRIOR root.
    let mut base_blobs = Vec::new();
    for r in &recipients {
        base_blobs.push(
            super::rekey::build_blob_local(me.secret_key(), &me.public_key().to_bytes(), r, super::rekey::RekeyScope::Root, new_epoch, &new_root)
                .map_err(|e| e.to_string())?,
        );
    }
    let base_group = super::derive::base_rekey_group_key(&community.community_root, cid, new_epoch);
    let base_chunks =
        super::rekey::build_rekey_chunks_local(&me, &base_group, super::rekey::RekeyScope::Root, new_epoch, prev_epoch, &prev_commit, &base_blobs, at_secs)
            .map_err(|e| e.to_string())?;

    // Private-channel rekeys: each mints a fresh key at its next channel-epoch, sealed
    // under the PRIOR root (D2). Public channels ride the base — no per-channel rekey.
    let mut channel_updates: Vec<(ChannelId, [u8; 32], Epoch)> = Vec::new();
    let mut channel_chunk_sets: Vec<Vec<Event>> = Vec::new();
    for ch in &community.channels {
        let (Some(old_key), true) = (ch.key, ch.private) else { continue };
        let ch_new_epoch = Epoch(ch.epoch.0.checked_add(1).ok_or("channel epoch overflow")?);
        // Mint-or-reuse per channel too, keyed by (channel_id, next epoch) — same
        // retry-idempotency as the base root.
        let ch_new_key = mint_or_reuse_rotation_key(&cid_hex, &crate::simd::hex::bytes_to_hex_32(&ch.id.0), ch_new_epoch.0)?;
        let ch_prev_commit = super::derive::epoch_key_commitment(ch.epoch, &old_key);
        let mut ch_blobs = Vec::new();
        for r in &recipients {
            ch_blobs.push(
                super::rekey::build_blob_local(me.secret_key(), &me.public_key().to_bytes(), r, super::rekey::RekeyScope::Channel(ch.id), ch_new_epoch, &ch_new_key)
                    .map_err(|e| e.to_string())?,
            );
        }
        let ch_group = super::derive::channel_rekey_group_key(&community.community_root, &ch.id, ch_new_epoch);
        let ch_chunks = super::rekey::build_rekey_chunks_local(&me, &ch_group, super::rekey::RekeyScope::Channel(ch.id), ch_new_epoch, ch.epoch, &ch_prev_commit, &ch_blobs, at_secs)
            .map_err(|e| e.to_string())?;
        channel_updates.push((ch.id, ch_new_key, ch_new_epoch));
        channel_chunk_sets.push(ch_chunks);
    }
    if !session.is_valid() {
        return Err("account changed during re-founding prepare".to_string());
    }

    // COMMIT (durable publishes only — all fetching is done). Base rekey first
    // (delivers the new root), then channel rekeys, then the compacted control.
    for c in &base_chunks {
        transport.publish_durable(c, &community.relays).await?;
    }
    for set in &channel_chunk_sets {
        for c in set {
            transport.publish_durable(c, &community.relays).await?;
        }
    }
    for (_, wrap) in &carried {
        transport.publish_durable(wrap, &community.relays).await?;
    }
    // Guestbook snapshot at the new epoch — best-effort (a Refounding succeeds without
    // it; an omitted member heals by publishing their own Join).
    let gb_group = super::derive::guestbook_group_key(&new_root, cid, new_epoch);
    let snap_id = crate::community::random_32();
    for rumor in guestbook::build_snapshot_rumors(me.public_key(), &recipients, snap_id, at) {
        if let Ok((wrap, _)) = guestbook::seal_guestbook_rumor(&rumor, &gb_group, &me, Timestamp::from_secs(at_secs)) {
            let _ = transport.publish(&wrap, &community.relays).await;
        }
    }

    // COMMIT locally, only now that the new root + compacted plane are on relays.
    if !session.is_valid() {
        return Err("account changed during re-founding commit".to_string());
    }
    if crate::db::community::community_protocol(cid)?.is_none() {
        return Ok(community.clone()); // left/deleted mid-rotation — don't resurrect.
    }
    // Save the new root/epoch + rekeyed channel keys in ONE tx FIRST, so a crash can
    // never leave the base root advanced while the channel keys lag (which would
    // re-derive the channel rekey address under the wrong root and orphan them).
    let mut updated = community.clone();
    updated.community_root = new_root;
    updated.root_epoch = new_epoch;
    for (id, key, ep) in &channel_updates {
        if let Some(c) = updated.channels.iter_mut().find(|c| c.id.0 == id.0) {
            c.key = Some(*key);
            c.epoch = *ep;
        }
    }
    crate::db::community::save_community_v2(&updated)?;
    // Archive the new epoch key + confirm the monotonic base head (the root was already
    // archived by mint_or_reuse, so this is idempotent). Record the carried heads at
    // the NEW epoch; if a crash skips this, the epoch-filtered floors bootstrap the
    // compacted control on the next follow, so they self-heal.
    crate::db::community::advance_server_root_epoch(&cid_hex, new_epoch.0, &new_root)?;
    for (h, _) in &carried {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, new_epoch.0)?;
    }
    // Refresh any live public links so their bundles carry the NEW root behind the
    // same URL (a link shared once survives the rotation, CORD-05 §2). Best-effort.
    let _ = refresh_public_links(transport, &updated).await;
    Ok(updated)
}

/// Mint a fresh 32-byte rotation key for `(scope, new_epoch)`, or REUSE the one
/// already archived from a prior (aborted) attempt — so a retried Refounding re-
/// delivers the SAME key at the same epoch/address instead of double-minting two roots
/// a receiver's correlation dedup would collapse into a permanent fork (CORD-06 §3
/// idempotency). Archived BEFORE the first publish; `scope` is the all-zero server-root
/// sentinel for a base rotation, else the channel_id hex.
fn mint_or_reuse_rotation_key(community_id_hex: &str, scope_hex: &str, new_epoch: u64) -> Result<[u8; 32], String> {
    if let Some(existing) = crate::db::community::held_epoch_key(community_id_hex, scope_hex, new_epoch)? {
        return Ok(existing);
    }
    let fresh = crate::community::random_32();
    crate::db::community::store_epoch_key(community_id_hex, scope_hex, new_epoch, &fresh)?;
    Ok(fresh)
}

// ── The Community List (kind 13302, CORD-02 §8) ──────────────────────────────

/// This community's MEMBERSHIP subset for the 13302 list (CORD-02 §8): never the
/// icon (a rehydrating device folds it from the Control Plane), never the link
/// fields. Only PRIVATE channel keys ride — public channels derive from the root.
fn join_material(community: &CommunityV2) -> super::list::JoinMaterial {
    let hex = crate::simd::hex::bytes_to_hex_32;
    let channels = community
        .channels
        .iter()
        .filter(|c| c.private)
        .filter_map(|c| {
            c.key.map(|k| super::list::ChannelKeyRef { id: hex(&c.id.0), key: hex(&k), epoch: c.epoch.0, name: c.name.clone() })
        })
        .collect();
    super::list::JoinMaterial {
        community_id: hex(&community.identity.community_id.0),
        owner: hex(&community.identity.owner_xonly),
        owner_salt: hex(&community.identity.owner_salt),
        community_root: hex(&community.community_root),
        root_epoch: community.root_epoch.0,
        channels,
        relays: community.relays.clone(),
        name: community.name.clone(),
        extra: Default::default(),
    }
}

/// Rebuild an invite bundle from list join material, for a cross-device rehydrate
/// (the material IS the membership subset of a bundle). The owner root is still
/// verified over the network before the community is trusted (accept_bundle).
fn material_to_invite(jm: &super::list::JoinMaterial) -> CommunityInvite {
    let channels = jm
        .channels
        .iter()
        .map(|c| invite::ChannelGrant { id: c.id.clone(), key: c.key.clone(), epoch: c.epoch, name: c.name.clone() })
        .collect();
    CommunityInvite {
        community_id: jm.community_id.clone(),
        owner: jm.owner.clone(),
        owner_salt: jm.owner_salt.clone(),
        community_root: jm.community_root.clone(),
        root_epoch: jm.root_epoch,
        channels,
        relays: jm.relays.clone(),
        name: jm.name.clone(),
        icon: None,
        expires_at: None,
        creator_npub: None,
        label: None,
        extra: Default::default(),
    }
}

/// The union of every held v2 community's relays — where this account's 13302 list
/// lives (a fresh device that opens any held community reaches the same set).
fn held_v2_relays() -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(ids) = crate::db::community::list_community_ids() {
        for id in ids {
            if matches!(crate::db::community::community_protocol(&id), Ok(Some(crate::community::ConcordProtocol::V2))) {
                if let Ok(Some(c)) = crate::db::community::load_community_v2(&id) {
                    set.extend(c.relays);
                }
            }
        }
    }
    set.into_iter().collect()
}

/// Fetch this account's own 13302 Community List from `relays` (the newest wins;
/// a decrypt/parse failure is "no news", never a clobber of the local mirror).
async fn fetch_community_list<T: Transport + ?Sized>(transport: &T, relays: &[String]) -> Option<super::list::CommunityList> {
    let me = local_keys().ok()?;
    let query = Query {
        kinds: vec![super::kind::COMMUNITY_LIST],
        authors: vec![me.public_key().to_hex()],
        limit: Some(4),
        ..Default::default()
    };
    let events = transport.fetch(&query, relays).await.ok()?;
    events
        .into_iter()
        .filter_map(|e| super::list::parse_list_event(&e, &me).ok().map(|l| (e.created_at.as_secs(), l)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, l)| l)
}

/// Rebuild this account's 13302 from its held v2 communities, MERGE with the remote
/// copy (preserving tombstones, other-device entries, unknown fields), and publish.
/// Idempotent; called after any membership change. Best-effort — a list-publish
/// failure never fails the membership change itself.
pub async fn republish_community_list<T: Transport + ?Sized>(transport: &T) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let relays = held_v2_relays();
    if relays.is_empty() {
        return Ok(()); // nothing held → nothing to sync
    }
    let remote = fetch_community_list(transport, &relays).await.unwrap_or_default();
    let now = now_ms();
    let mut local = super::list::CommunityList::default();
    for id in crate::db::community::list_community_ids()? {
        if !matches!(crate::db::community::community_protocol(&id), Ok(Some(crate::community::ConcordProtocol::V2))) {
            continue;
        }
        let Some(c) = crate::db::community::load_community_v2(&id)? else { continue };
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&c.id().0);
        // Keep an already-live entry's add time (no churn); a new or previously-left
        // (now re-held) community adds/resurrects at `now`.
        let added_at = if remote.is_live(&cid_hex) {
            remote.entries.iter().find(|e| e.community_id == cid_hex).map(|e| e.added_at).unwrap_or(now)
        } else {
            now
        };
        let jm = join_material(&c);
        local.entries.push(super::list::CommunityListEntry { community_id: cid_hex, seed: jm.clone(), current: jm, added_at, extra: Default::default() });
    }
    let merged = remote.merge(&local);
    merged.assert_fits().map_err(|e| e.to_string())?;
    let event = super::list::build_list_event(&me, &merged).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed during community-list publish".to_string());
    }
    transport.publish(&event, &relays).await
}

/// Record a permanent leave tombstone for `community_id` in the 13302, published to
/// `relays` (the leaving community's own, since it's about to be deleted locally).
async fn tombstone_community_list<T: Transport + ?Sized>(transport: &T, community_id: &crate::community::CommunityId, relays: &[String]) -> Result<(), String> {
    let me = local_keys()?;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community_id.0);
    let mut doc = fetch_community_list(transport, relays).await.unwrap_or_default();
    let now = now_ms();
    doc.tombstones.retain(|t| t.community_id != cid_hex);
    doc.tombstones.push(super::list::Tombstone { community_id: cid_hex, removed_at: now, extra: Default::default() });
    doc.assert_fits().map_err(|e| e.to_string())?;
    let event = super::list::build_list_event(&me, &doc).map_err(|e| e.to_string())?;
    transport.publish(&event, relays).await
}

/// Sync memberships from the 13302 across devices: fetch this account's list from
/// `bootstrap_relays` (its held communities' relays plus any caller-supplied set for
/// a fresh device), and JOIN every live entry not already held — reconstructing the
/// community from its join material and re-verifying the owner root. Returns the
/// newly-rehydrated communities (so the caller can subscribe + notify).
pub async fn sync_community_list<T: Transport + ?Sized>(transport: &T, bootstrap_relays: &[String]) -> Result<Vec<CommunityV2>, String> {
    let session = SessionGuard::capture();
    let mut relays = held_v2_relays();
    relays.extend(bootstrap_relays.iter().cloned());
    relays.sort();
    relays.dedup();
    if relays.is_empty() {
        return Ok(vec![]);
    }
    let Some(list) = fetch_community_list(transport, &relays).await else {
        return Ok(vec![]);
    };
    let mut joined = Vec::new();
    for entry in list.live_entries() {
        let Some(cid) = crate::simd::hex::hex_to_bytes_32_checked(&entry.community_id) else { continue };
        if crate::db::community::load_community_v2(&crate::community::CommunityId(cid)).ok().flatten().is_some() {
            continue; // already held
        }
        if !session.is_valid() {
            return Err("account changed during community-list sync".to_string());
        }
        // The material IS a bundle; accept_bundle re-verifies the owner root, saves,
        // seeds floors, and announces our Join (idempotent for an existing member).
        let bundle = material_to_invite(&entry.current);
        if let Ok(community) = accept_bundle(transport, &session, &bundle, None).await {
            joined.push(community);
        }
    }
    Ok(joined)
}

// ── Control edition authoring (CORD-04 roles / CORD-02 §6 / CORD-03 §2) ──────

/// Publish one control edition (a role, grant, banlist, community-metadata, or
/// channel-metadata edit) at the next version for its entity, chaining `prev` from
/// our held head, and advance our local floor. Authority is enforced by every
/// reader's roster fold (CORD-04 §5: authority is rejection, not prevention), so this
/// requires only a valid local signer; a well-behaved client checks its own rank
/// first, but a reader drops an unauthorized edition regardless.
async fn publish_control_edition<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
    vsk: &str,
    entity_id: &[u8; 32],
    content: &str,
    citation: Option<&crate::community::edition::AuthorityCitation>,
) -> Result<(), String> {
    let me = local_keys()?;
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let entity_hex = crate::simd::hex::bytes_to_hex_32(entity_id);
    let (version, prev) = match crate::db::community::get_edition_head(&cid_hex, &entity_hex)? {
        Some((v, h)) => (v + 1, Some(h)),
        None => (1, None),
    };
    let at = now_ms() / 1000;
    let rumor = control::build_edition_rumor(me.public_key(), vsk, entity_id, version, prev.as_ref(), content, at, citation);
    let (wrap, _) = control::seal_control_edition(&rumor, &control, &me, Timestamp::from_secs(at)).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed before control publish".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    // Advance our own floor so a follow-up edit chains from this head and refuse-
    // downgrade holds; open our own wrap to recover the self_hash + inner_id.
    // Re-check the session AFTER the publish await: a swap mid-publish means the
    // pool now points at another account's DB — skipping is safe (the next own
    // edit rebuilds the same head from the relay's copy).
    if !session.is_valid() {
        return Ok(());
    }
    if let Ok((ed, _)) = control::open_control_edition(&wrap, &control) {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &entity_hex, ed.version, &ed.self_hash, &ed.inner_id, community.root_epoch.0)?;
    }
    Ok(())
}

/// Create or edit a Role (vsk 1, CORD-04 §2). `role.role_id` is the coordinate; a
/// rename or permission change is a versioned edit of the same id. Gated on the
/// reader side by `MANAGE_ROLES` + outrank.
pub async fn set_role<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, role: &crate::community::roles::Role) -> Result<(), String> {
    let session = SessionGuard::capture();
    super::roles::validate_role(role)?;
    let content = super::roles::role_content_json(role)?;
    let role_id = crate::simd::hex::hex_to_bytes_32_checked(&role.role_id).ok_or("role_id must be 32-byte hex")?;
    publish_control_edition(transport, community, &session, vsk::ROLE, &role_id, &content, None).await
}

/// Grant or revoke a member's Roles (vsk 3, CORD-04 §2). Empty `role_ids` is a
/// revoke. Gated on the reader side by `MANAGE_ROLES` + outrank of every role + the
/// member.
pub async fn grant_roles<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, member: &PublicKey, role_ids: Vec<String>) -> Result<(), String> {
    let session = SessionGuard::capture();
    let grant = crate::community::roles::MemberGrant { member: member.to_hex(), role_ids };
    let content = super::roles::grant_content_json(&grant)?;
    let eid = super::derive::grant_locator(community.id(), &member.to_bytes());
    publish_control_edition(transport, community, &session, vsk::GRANT, &eid, &content, None).await
}

/// The community's @admin role id: the folded Server-scope ADMIN_ALL role when one
/// exists, else (with `create_if_missing`) a DETERMINISTIC mint — the same id on
/// every device, so concurrent grants converge as editions of ONE entity instead
/// of forking two Admin roles.
pub async fn ensure_admin_role<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    view: &AuthorityView,
    create_if_missing: bool,
) -> Result<Option<String>, String> {
    use crate::community::roles::{Permissions, Role, RoleScope};
    if let Some(r) = view
        .roles
        .roles
        .iter()
        .find(|r| matches!(r.scope, RoleScope::Server) && r.permissions.contains(Permissions::ADMIN_ALL))
    {
        return Ok(Some(r.role_id.clone()));
    }
    if !create_if_missing {
        return Ok(None);
    }
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let role_id = crate::crypto::sha256_hex(format!("vector/v2/role/admin/{cid_hex}").as_bytes());
    set_role(transport, community, &Role::admin(role_id.clone())).await?;
    Ok(Some(role_id))
}

/// Grant the @admin role (minting it deterministically when absent), MERGED into
/// the member's existing grant — a grant entity replaces whole (CORD-04 §2), so a
/// blind push would erase their other roles. Owner-only: the position-1 Admin is
/// manageable only by position 0 (an equal never outranks it), and refusing
/// before any publish keeps an unauthorized edition of the DETERMINISTIC admin
/// entity from advancing this device's own floor onto a head readers reject.
pub async fn grant_admin<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, member: &PublicKey) -> Result<(), String> {
    // Guard spans the multi-page fetch below: a swap mid-fetch must not let the
    // downstream publish's own (post-swap) guard write account A's floor into B.
    let session = SessionGuard::capture();
    let me = local_keys()?;
    if me.public_key() != community.owner()? {
        return Err("only the community owner can grant @admin".to_string());
    }
    let view = fetch_authority(transport, community).await;
    if !session.is_valid() {
        return Err("account changed during grant".to_string());
    }
    let member_hex = member.to_hex();
    require_grant_head(community, &view, &member_hex)?;
    let role_id = ensure_admin_role(transport, community, &view, true)
        .await?
        .expect("create_if_missing yields an id");
    let mut role_ids = view
        .roles
        .grants
        .iter()
        .find(|g| g.member == member_hex)
        .map(|g| g.role_ids.clone())
        .unwrap_or_default();
    if role_ids.contains(&role_id) {
        return Ok(()); // already admin — don't bump the grant edition for nothing.
    }
    role_ids.push(role_id);
    grant_roles(transport, community, member, role_ids).await
}

/// Strip the @admin role from the member's grant, preserving their other roles.
/// A no-op when they don't hold it. Owner-only, like [`grant_admin`].
pub async fn revoke_admin<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, member: &PublicKey) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    if me.public_key() != community.owner()? {
        return Err("only the community owner can revoke @admin".to_string());
    }
    let view = fetch_authority(transport, community).await;
    if !session.is_valid() {
        return Err("account changed during revoke".to_string());
    }
    let member_hex = member.to_hex();
    require_grant_head(community, &view, &member_hex)?;
    let Some(role_id) = ensure_admin_role(transport, community, &view, false).await? else {
        return Ok(()); // no admin role exists — nothing to revoke.
    };
    let mut role_ids = view
        .roles
        .grants
        .iter()
        .find(|g| g.member == member_hex)
        .map(|g| g.role_ids.clone())
        .unwrap_or_default();
    let before = role_ids.len();
    role_ids.retain(|r| r != &role_id);
    if role_ids.len() == before {
        return Ok(());
    }
    grant_roles(transport, community, member, role_ids).await
}

/// A grant replaces whole — refuse the merge when this member's grant is FLOORED
/// locally but no head folded (withheld / evicted): a blind push at that point
/// would erase their other roles at a higher version.
fn require_grant_head(community: &CommunityV2, view: &AuthorityView, member_hex: &str) -> Result<(), String> {
    let Some(member) = crate::simd::hex::hex_to_bytes_32_checked(member_hex) else {
        return Err("malformed member key".to_string());
    };
    let eid_hex = crate::simd::hex::bytes_to_hex_32(&super::derive::grant_locator(community.id(), &member));
    if view.floored.contains(&eid_hex) && !view.head_entities.contains(&eid_hex) {
        return Err("this member's current grant could not be fetched; try again once relays serve the control plane".to_string());
    }
    Ok(())
}

/// Replace the Banlist (vsk 4, CORD-04 §4) with `banned` (lowercase-hex npubs), the
/// whole list on every edit. Gated on the reader side by `BAN`.
pub async fn set_banlist<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, banned: &[String]) -> Result<(), String> {
    let session = SessionGuard::capture();
    super::roles::validate_banlist(banned)?;
    let content = super::roles::banlist_content_json(banned)?;
    let eid = super::derive::banlist_locator(community.id());
    publish_control_edition(transport, community, &session, vsk::BANLIST, &eid, &content, None).await
}

/// Edit the community metadata (vsk 0, CORD-02 §6). Gated on the reader side by
/// `MANAGE_METADATA`.
pub async fn edit_community_metadata<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, meta: &control::CommunityMetadata) -> Result<(), String> {
    let session = SessionGuard::capture();
    control::validate_community_metadata(meta).map_err(|e| e.to_string())?;
    let content = serde_json::to_string(meta).map_err(|e| e.to_string())?;
    publish_control_edition(transport, community, &session, vsk::COMMUNITY_METADATA, &community.id().0, &content, None).await
}

/// Add or edit a channel's metadata (vsk 2, CORD-03 §2). `channel_id` is the
/// coordinate. Gated on the reader side by `MANAGE_CHANNELS`.
pub async fn edit_channel_metadata<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel_id: &ChannelId, meta: &control::ChannelMetadata) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    ensure_channel_manager(community, &me.public_key())?;
    // Public → private CONVERSION is a key rotation (CORD-03 §2) this build doesn't
    // mint yet — refuse the flag flip rather than publish an edition no reader can
    // key (members would keep posting on the root-derived plane, splitting the
    // channel). Private → public works (readers heal to the root derivation).
    if meta.private {
        if let Some(held) = community.channel(channel_id) {
            if !held.private {
                return Err("converting a public channel to private is not supported yet".to_string());
            }
        }
    }
    control::validate_channel_metadata(meta).map_err(|e| e.to_string())?;
    let content = serde_json::to_string(meta).map_err(|e| e.to_string())?;
    publish_control_edition(transport, community, &session, vsk::CHANNEL_METADATA, &channel_id.0, &content, None).await
}

/// The local mirror of the reader's `MANAGE_CHANNELS` fold gate (CORD-03 §2): the
/// owner, or a roster-authorized manager who isn't banned. Refusing BEFORE any
/// publish keeps an unauthorized device from advancing its own edition floor onto
/// a head every reader rejects (wedging its later, legitimately-authorized edits
/// behind a rejected chain).
fn ensure_channel_manager(community: &CommunityV2, me: &PublicKey) -> Result<(), String> {
    let owner = community.owner()?;
    if *me == owner {
        return Ok(());
    }
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let me_hex = me.to_hex();
    if crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default().contains(&me_hex) {
        return Err("you are banned from this community".to_string());
    }
    let roster = crate::db::community::get_community_roles(&cid_hex)?;
    if roster.is_authorized(&me_hex, Some(&owner.to_hex()), crate::community::roles::Permissions::MANAGE_CHANNELS) {
        Ok(())
    } else {
        Err("managing channels here needs the MANAGE_CHANNELS permission".to_string())
    }
}

/// Create a new PUBLIC channel (CORD-03 §2): mint a fresh id, publish its metadata
/// edition (vsk 2), and add it to the held community. A Public channel derives its Chat
/// Plane from the `community_root` (no per-channel key), so other members fold it in on
/// their next control follow with nothing to distribute. Returns the new channel id.
/// Reader-gated by `MANAGE_CHANNELS`.
pub async fn create_public_channel<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, name: &str) -> Result<ChannelId, String> {
    let session = SessionGuard::capture();
    // Serialize with the follow worker: the save below writes the WHOLE community
    // row from this caller's struct, so an unserialized concurrent follow adopting
    // a rotation would be rolled back to a stale root (a deaf community).
    let lock = super::realtime::follow_lock(community.id());
    let _guard = lock.lock().await;
    let me = local_keys()?;
    ensure_channel_manager(community, &me.public_key())?;
    let channel_id = ChannelId(super::super::random_32());
    let meta = control::ChannelMetadata { name: name.to_string(), private: false, voice: None, deleted: None, custom: None, extra: Default::default() };
    control::validate_channel_metadata(&meta).map_err(|e| e.to_string())?;
    let content = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
    publish_control_edition(transport, community, &session, vsk::CHANNEL_METADATA, &channel_id.0, &content, None).await?;
    if !session.is_valid() {
        return Err("account changed during channel create".to_string());
    }
    // Add locally + persist so the creator can post immediately (peers fold it in).
    let mut updated = community.clone();
    updated.channels.push(ChannelV2 { id: channel_id, name: name.to_string(), private: false, key: None, epoch: updated.root_epoch });
    crate::db::community::save_community_v2(&updated)?;
    Ok(channel_id)
}

/// Create a new PRIVATE channel (CORD-03 §2): mint a fresh id + an independent
/// random key at channel-epoch 1, deliver the key to every current member over the
/// rekey plane (CORD-06 §1), then announce the channel (vsk 2, `private`). Epoch 0
/// is the root generation ("the first privatisation is epoch 1"), so the delivery
/// commits its continuity to `(0, community_root)` — verifiable by every member and
/// bound to THIS community's root. The key ships BEFORE the announcement: an
/// aborted attempt leaves only an unannounced crate (invisible), and a retry mints
/// a fresh id, so there is no same-coordinate double-mint to fork on. Live public
/// links are refreshed so a joiner's bundle carries the key; a member who joins
/// through the stale-bundle window keys up at the channel's next rotation.
pub async fn create_private_channel<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, name: &str) -> Result<ChannelId, String> {
    let session = SessionGuard::capture();
    // Serialize with the follow worker across the whole fetch→publish→save span
    // (the memberlist fetch is seconds long; an unserialized follow adopting a
    // rotation meanwhile would be rolled back by the whole-row save below).
    let lock = super::realtime::follow_lock(community.id());
    let _guard = lock.lock().await;
    let me = local_keys()?;
    ensure_channel_manager(community, &me.public_key())?;
    let meta = control::ChannelMetadata { name: name.to_string(), private: true, voice: None, deleted: None, custom: None, extra: Default::default() };
    control::validate_channel_metadata(&meta).map_err(|e| e.to_string())?;
    let content = serde_json::to_string(&meta).map_err(|e| e.to_string())?;

    let channel_id = ChannelId(super::super::random_32());
    let channel_key = super::super::random_32();
    let epoch = Epoch(1);

    // Recipients: every current member, plus me (multi-device).
    let mut recipients = memberlist(transport, community).await?;
    if !recipients.iter().any(|p| *p == me.public_key()) {
        recipients.push(me.public_key());
    }
    let prev_commit = super::derive::epoch_key_commitment(Epoch(0), &community.community_root);
    let mut blobs = Vec::with_capacity(recipients.len());
    for r in &recipients {
        blobs.push(
            rekey::build_blob_local(me.secret_key(), &me.public_key().to_bytes(), r, RekeyScope::Channel(channel_id), epoch, &channel_key)
                .map_err(|e| e.to_string())?,
        );
    }
    let group = channel_rekey_group_key(&community.community_root, &channel_id, epoch);
    let at_secs = now_ms() / 1000;
    let chunks = rekey::build_rekey_chunks_local(&me, &group, RekeyScope::Channel(channel_id), epoch, Epoch(0), &prev_commit, &blobs, at_secs)
        .map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed during channel create".to_string());
    }
    for c in &chunks {
        transport.publish_durable(c, &community.relays).await?;
    }
    publish_control_edition(transport, community, &session, vsk::CHANNEL_METADATA, &channel_id.0, &content, None).await?;
    if !session.is_valid() {
        return Err("account changed during channel create".to_string());
    }
    // A leave/delete raced the create: saving would resurrect the community row.
    if crate::db::community::community_protocol(community.id())?.is_none() {
        return Err("community removed during channel create".to_string());
    }
    let mut updated = community.clone();
    updated.channels.push(ChannelV2 { id: channel_id, name: name.to_string(), private: true, key: Some(channel_key), epoch });
    crate::db::community::save_community_v2(&updated)?;
    // Archive the epoch-1 key so this channel's history stays readable across its
    // future rotations (CORD-03 §3).
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    crate::db::community::store_epoch_key(&cid_hex, &crate::simd::hex::bytes_to_hex_32(&channel_id.0), epoch.0, &channel_key)?;
    // Future joiners are handed the key in their (refreshed) bundle, CORD-05 §1.
    let _ = refresh_public_links(transport, &updated).await;
    Ok(channel_id)
}

/// Tombstone a channel (CORD-03 §2, `deleted: true`) + drop it locally. Reader-gated by
/// `MANAGE_CHANNELS`; the coordinate stays folded as a grave so peers hide it.
pub async fn delete_channel<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel_id: &ChannelId, name: &str) -> Result<(), String> {
    let session = SessionGuard::capture();
    // Whole-row save below — serialize with the follow worker (see create_*_channel).
    let lock = super::realtime::follow_lock(community.id());
    let _guard = lock.lock().await;
    let me = local_keys()?;
    ensure_channel_manager(community, &me.public_key())?;
    let meta = control::ChannelMetadata { name: name.to_string(), private: false, voice: None, deleted: Some(true), custom: None, extra: Default::default() };
    let content = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
    publish_control_edition(transport, community, &session, vsk::CHANNEL_METADATA, &channel_id.0, &content, None).await?;
    if !session.is_valid() {
        return Err("account changed during channel delete".to_string());
    }
    let mut updated = community.clone();
    updated.channels.retain(|c| c.id.0 != channel_id.0);
    crate::db::community::save_community_v2(&updated)?;
    Ok(())
}

// ── Live control-follow (CORD-02 §6 / CORD-03 §2) ────────────────────────────

/// Re-fold this community's Control Plane and apply the current metadata +
/// **public** channel set to the held community, persisting any change. Called
/// when a control-plane wrap arrives in realtime (a rename, a new channel, an
/// edited description) so a long-running bot tracks the community mid-session
/// instead of freezing at its join-time view.
///
/// **Authority (CORD-04 §5):** the roster (roles/grants/banlist) folds first into
/// the owner-seeded authorized set ([`fold_authority`]), then each metadata/channel
/// edition is eligible only if its signer CURRENTLY holds the entity's management
/// bit (`MANAGE_METADATA`/`MANAGE_CHANNELS`) — so an authorized admin's edits fold,
/// a demoted one's drop. The owner is supreme, proven by the self-certifying
/// community_id (no network trust).
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
    community.owner()?; // fail fast if the community is somehow unproven.
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);

    // Per-entity refuse-downgrade floors for the CURRENT epoch only. A head recorded
    // under a prior epoch is excluded, so that entity auto-bootstraps after a
    // Refounding (Armada accepts a compacted head across a dangling prev — matched).
    // A read error FAILS CLOSED: an empty map would silently re-open the rollback
    // window the floor exists to shut.
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)?
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();

    // Newest window first; page OLDER only while a tracking entity is gapped (its
    // floor link evicted from the window — H1/M8 refetch), bounded like the join
    // verifier. A withholding relay still converges to fail-closed after the cap.
    let mut editions: Vec<ParsedEdition> = Vec::new();
    let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    let mut seen_wraps: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    let mut oldest: Option<u64> = None;
    let mut until: Option<u64> = None;
    let mut fold = ControlFold { updated: None, heads: Vec::new(), gapped: false };
    let mut authority = AuthoritySet::owner_only();
    for _ in 0..FOLLOW_MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![control.pk_hex()],
            until,
            limit: Some(FOLLOW_PAGE),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, &community.relays).await?;
        // The `until` cursor is INCLUSIVE (a `-1` step can skip same-second siblings
        // at a page boundary); the wrap-id dedup makes re-served boundary events
        // free, and a page with nothing new means the relay is exhausted.
        let mut fresh = 0usize;
        for w in &wraps {
            if !seen_wraps.insert(w.id) {
                continue;
            }
            fresh += 1;
            let at = w.created_at.as_secs();
            if oldest.is_none_or(|o| at < o) {
                oldest = Some(at);
            }
            // Open + seal-verify every edition; authority is resolved by the roster
            // fold (CORD-04 §5), not by a signer filter here — an admin's edits fold.
            if let Ok((ed, _)) = control::open_control_edition(w, &control) {
                if seen.insert(ed.inner_id) {
                    editions.push(ed);
                }
            }
        }
        // Roster first (roles/grants/banlist → authorized set), then the authority-
        // gated metadata/channel fold over the same edition set.
        authority = fold_authority(community, &editions, &floors);
        fold = apply_control_fold(community, &editions, &floors, &authority);
        if !(fold.gapped || authority.gapped) || fresh == 0 {
            break;
        }
        until = oldest;
    }

    // The fetches straddled awaits; a swap since the guard was captured must not
    // write account A's control state into B.
    if !session.is_valid() {
        return Err("account changed during control follow".to_string());
    }
    // A leave/delete raced this follow: writing now would resurrect the community
    // row and orphan floor rows past delete_community's wipe.
    if crate::db::community::community_protocol(community.id())?.is_none() {
        return Ok(None);
    }
    // Persist advanced floors BEFORE the state save (a failed floor write must not
    // let saved state outrun its floor), stamping the epoch this fold ran under —
    // not the row's write-time value, which a concurrent re-founding can bump. Both
    // the metadata/channel heads and the roster/banlist heads advance their floors;
    // run the advance (v+1) and same-version convergence (fork tiebreak) paths.
    for h in fold.heads.iter().chain(authority.heads.iter()) {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, community.root_epoch.0)?;
        crate::db::community::converge_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, community.root_epoch.0)?;
    }
    // Persist the authorized banlist content (retained/withholding folds carry None,
    // so the stored banlist is left intact — an anti-roster never silently un-bans).
    if let Some((banned, version)) = &authority.banlist_persist {
        crate::db::community::set_community_banlist(&cid_hex, banned, *version as i64)?;
    }
    // Persist the authorized roster so capabilities/roles stay sync LOCAL reads
    // (v1 parity: the passive follow folds, reads never fetch). Guarded like v1's
    // fetch path: only an aggregate built from roster editions at least as new as
    // the stored one may replace it — a withholding relay serving NO roster
    // editions folds an empty-but-ungapped aggregate (absence raises no gap flag),
    // and that must RETAIN the stored roster, never wipe standing.
    let newest_roster_at: i64 = editions
        .iter()
        .filter(|e| e.vsk == vsk::ROLE || e.vsk == vsk::GRANT || e.vsk == vsk::BANLIST)
        .map(|e| e.created_at as i64)
        .max()
        .unwrap_or(0);
    // Completeness gate: the `gapped` flag only covers entities present in the window.
    // A role/grant floored on this device but with ZERO editions fetched (aged out of
    // the paging reach) folds absent yet raises no gap — persisting would silently drop
    // it. So if any CURRENTLY-STORED entity is floored but folded no head this round,
    // RETAIN. A real revoke still folds a head (see select_authorized), so it persists.
    let stored = crate::db::community::get_community_roles(&cid_hex).unwrap_or_default();
    let head_ents: std::collections::HashSet<&str> = authority.heads.iter().map(|h| h.entity_hex.as_str()).collect();
    let stored_complete = stored.roles.iter().all(|r| !floors.contains_key(&r.role_id) || head_ents.contains(r.role_id.as_str()))
        && stored.grants.iter().all(|g| {
            crate::simd::hex::hex_to_bytes_32_checked(&g.member).is_none_or(|m| {
                let eid = crate::simd::hex::bytes_to_hex_32(&super::derive::grant_locator(community.id(), &m));
                !floors.contains_key(&eid) || head_ents.contains(eid.as_str())
            })
        });
    if !authority.gapped && stored_complete && newest_roster_at >= crate::db::community::get_community_roles_at(&cid_hex)? {
        crate::db::community::set_community_roles(&cid_hex, &authority.roles, newest_roster_at)?;
    }
    match fold.updated {
        Some(u) => {
            crate::db::community::save_community_v2(&u)?;
            Ok(Some(u))
        }
        None => Ok(None),
    }
}

/// Control-follow paging bounds: enough depth to re-anchor a long-offline floor
/// (H1/M8 refetch) without letting a flooding relay stall the follow queue.
const FOLLOW_MAX_PAGES: usize = 4;
const FOLLOW_PAGE: usize = 500;

/// A folded control head to persist as the per-entity refuse-downgrade floor.
#[derive(Clone)]
struct FoldedHead {
    entity_hex: String,
    version: u64,
    self_hash: [u8; 32],
    inner_id: [u8; 32],
}

/// The outcome of a floor-aware control fold: the updated community (if content
/// changed), the heads to persist as the new floor (returned even when content is
/// unchanged, so the floor still seeds/advances), and whether any TRACKING entity
/// hit an unresolvable gap — the caller's signal to page older history and re-fold
/// (CORD-04 H1/M8's refetch).
struct ControlFold {
    updated: Option<CommunityV2>,
    heads: Vec<FoldedHead>,
    gapped: bool,
}

/// Per-entity floor: `(version, self_hash, inner_id)` of the committed head.
type Floors = std::collections::HashMap<String, (u64, [u8; 32], Option<[u8; 32]>)>;

/// Fold owner-authored control editions into an updated community using the
/// PERSISTED per-entity version floor (refuse-downgrade). Per entity, fold with
/// [`version::fold`]`(floor, floor_hash)`:
///   - ANCHORED: adopt the chain-verified head. A `gap` ABOVE it (withheld middles)
///     doesn't block the verified prefix — refuse-downgrade holds for everything
///     applied — but flags `gapped` so the caller pages for the rest.
///   - UNANCHORED under a held floor: one legitimate cause is a same-version owner
///     fork AT the floor whose deterministic winner (lower inner id; a NULL held id
///     is always replaceable, mirroring v1's `decide()`) isn't our held edition —
///     the floor CONVERGES to the winner and the chain re-anchors on it, so every
///     client lands on the same head where a hash-strict floor would wedge forever.
///     Anything else is withholding → fail closed + `gapped`.
///   - BOOTSTRAPPING (`floor == 0` — a fresh joiner, or a fresh epoch after a
///     Refounding, since the caller epoch-filters the floor) takes the highest
///     signed head (author already owner-filtered).
/// This matches CORD-04 §1 and mirrors v1's `fold_roster`. Epoch-filtering makes a
/// compaction at a new epoch auto-bootstrap, converging with Armada's acceptance of
/// a compacted head across a dangling `prev` (Armada doesn't persist a floor, so a
/// Vector floor only makes Vector STRICTER locally — no wire change, honest-case
/// convergence preserved).
fn apply_control_fold(community: &CommunityV2, editions: &[ParsedEdition], floors: &Floors, authority: &AuthoritySet) -> ControlFold {
    use crate::community::roles::Permissions;
    use std::collections::BTreeMap;

    let owner_hex = community.owner().ok().map(|o| o.to_hex());

    let mut groups: BTreeMap<(String, [u8; 32]), Vec<&ParsedEdition>> = BTreeMap::new();
    for e in editions {
        groups.entry((e.vsk.clone(), e.entity_id)).or_default().push(e);
    }

    let mut out = community.clone();
    let mut changed = false;
    let mut heads = Vec::new();
    let mut gapped = false;
    for ((vsk_code, eid), group) in &groups {
        // This fold applies exactly two entities: community metadata (eid ==
        // community_id) and channel metadata. A vsk-2 whose eid equals the community
        // id is excluded — the floor row keys on the entity alone, so it would share
        // (and corrupt) the metadata chain's floor.
        let is_meta = vsk_code == vsk::COMMUNITY_METADATA && *eid == community.id().0;
        let is_channel = vsk_code == vsk::CHANNEL_METADATA && *eid != community.id().0;
        if !is_meta && !is_channel {
            continue;
        }
        // Authority gate (CORD-04 §5): only editions whose author CURRENTLY holds the
        // entity's management bit are eligible. Pre-filtering before the fold means a
        // demoted admin's (possibly higher-version) edition can't be the head; the
        // highest AUTHORIZED head wins. The owner is supreme.
        let required = if is_meta { Permissions::MANAGE_METADATA } else { Permissions::MANAGE_CHANNELS };
        let authed: Vec<&ParsedEdition> = group
            .iter()
            .copied()
            .filter(|e| {
                let author = e.author.to_hex();
                // A banned npub's edits are dropped (CORD-04 §4), even if they still
                // held a bit via a not-yet-stripped grant.
                !authority.banned.contains(&author) && authority.roles.is_authorized(&author, owner_hex.as_deref(), required)
            })
            .collect();
        if authed.is_empty() {
            continue;
        }
        let entity_hex = crate::simd::hex::bytes_to_hex_32(eid);
        let fold_eds: Vec<version::Edition> = authed.iter().map(|p| p.to_fold_edition()).collect();
        let (hi, entity_gapped) = fold_head(&fold_eds, floors.get(&entity_hex));
        gapped |= entity_gapped;
        let Some(hi) = hi else { continue };

        let head = authed[hi];
        heads.push(FoldedHead { entity_hex, version: head.version, self_hash: head.self_hash, inner_id: head.inner_id });
        if is_meta {
            if let Ok(meta) = serde_json::from_str::<control::CommunityMetadata>(&head.content) {
                changed |= apply_community_metadata(&mut out, meta);
            }
        } else if let Ok(meta) = serde_json::from_str::<control::ChannelMetadata>(&head.content) {
            // vsk-2 carries no community binding (shared v1 grammar); a same-owner
            // cross-community replay can inject a phantom PUBLIC channel (bounded:
            // root-scoped key, eids don't collide). Binding is a deferred wire change.
            changed |= apply_channel_metadata(&mut out, ChannelId(*eid), meta);
        }
    }
    ControlFold { updated: changed.then_some(out), heads, gapped }
}

/// Fold one entity's editions against its persisted floor into a head index (into the
/// input slice) plus whether a TRACKING gap was hit (the caller pages older history).
/// Encapsulates the W2 refuse-downgrade policy: bootstrap at floor 0 (highest signed
/// head, what Armada shows across a compaction's dangling prev); adopt the chain-
/// anchored head, paging on an upper gap; converge a same-version fork at the floor to
/// the lower-inner-id winner; and fail closed otherwise.
fn fold_head(fold_eds: &[version::Edition], floor: Option<&(u64, [u8; 32], Option<[u8; 32]>)>) -> (Option<usize>, bool) {
    let floor_v = floor.map(|f| f.0).unwrap_or(0);
    if floor_v == 0 {
        return (version::bootstrap_head(fold_eds, 0), false);
    }
    let floor_hash = floor.map(|f| &f.1);
    let held_inner = floor.and_then(|f| f.2);
    let result = version::fold(fold_eds, floor_v, floor_hash);
    if result.anchored {
        return (result.head, result.gap); // verified prefix; page any upper gap.
    }
    if result.head.is_none() && !result.gap {
        return (None, false); // everything below floor — a stale relay, no paging.
    }
    // Unanchored under a held floor: converge a same-version fork at the floor to its
    // deterministic winner (lower inner id; a NULL held id is always replaceable),
    // else fail closed.
    let fork = fold_eds.iter().enumerate().filter(|(_, e)| e.version == floor_v).min_by_key(|(_, e)| e.tiebreak_id);
    let win_hash = match fork {
        Some((_, w)) if floor_hash != Some(&w.self_hash) && held_inner.is_none_or(|h| w.tiebreak_id < h) => w.self_hash,
        _ => return (None, true), // detached from our committed head → withholding.
    };
    let re = version::fold(fold_eds, floor_v, Some(&win_hash));
    if !re.anchored {
        return (None, true);
    }
    (re.head, re.gap)
}

/// The folded, delegation-AUTHORIZED control-plane authority (CORD-04): the roster
/// (roles + grants, owner-seeded fixpoint), the enforced banlist, and the
/// role/grant/banlist heads to persist as refuse-downgrade floors. The owner is
/// recomputed from the self-certifying community_id at each use.
struct AuthoritySet {
    roles: crate::community::roles::CommunityRoles,
    banned: std::collections::BTreeSet<String>,
    heads: Vec<FoldedHead>,
    gapped: bool,
    /// The authorized banlist `(content, version)` to persist when an authorized head
    /// advanced the floor. `None` when the banlist was retained (no new authorized
    /// head) or is empty — the caller then leaves the stored banlist untouched.
    banlist_persist: Option<(Vec<String>, u64)>,
}

impl AuthoritySet {
    /// Bootstrap authority for a community with no roster editions folded yet: only
    /// the owner is authorized (supreme), nobody banned.
    fn owner_only() -> Self {
        AuthoritySet { roles: Default::default(), banned: Default::default(), heads: vec![], gapped: false, banlist_persist: None }
    }
}

/// Fold the roster/banlist entities (vsk 1/3/4) from the control editions into the
/// delegation-AUTHORIZED roster + enforced banlist (CORD-04 §2-§5). Each entity binds
/// to its coordinate (role at role_id, grant at grant_locator(cid, member), banlist at
/// banlist_locator(cid)); a content whose coordinate doesn't match is dropped. Roles
/// cap at the 100 lowest role_ids, a member at 64 roles, the banlist at 500. The
/// banlist is enforced only if its head's signer held BAN in the authorized roster.
fn fold_authority(community: &CommunityV2, editions: &[ParsedEdition], floors: &Floors) -> AuthoritySet {
    use crate::community::roles::Permissions;
    use std::collections::BTreeMap;

    let cid = community.id();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid.0);
    let owner = community.owner().ok();
    let owner_hex = owner.map(|o| o.to_hex());
    let banlist_eid = super::derive::banlist_locator(cid);
    let banlist_hex = crate::simd::hex::bytes_to_hex_32(&banlist_eid);

    let mut groups: BTreeMap<[u8; 32], Vec<&ParsedEdition>> = BTreeMap::new();
    for e in editions {
        if e.vsk == vsk::ROLE || e.vsk == vsk::GRANT || e.vsk == vsk::BANLIST {
            groups.entry(e.entity_id).or_default().push(e);
        }
    }

    // Per-entity CANDIDATE lists — every ≥floor edition of a role/grant, highest
    // version first (lowest inner-id as the deterministic tiebreak). CORD-04 §1: an
    // edition whose signer isn't authorized is SIMPLY DROPPED and the fold continues
    // to the next candidate, so a forged higher-version edition can't suppress the
    // authorized head beneath it (the author-blind collapse-to-one-head it replaces
    // let any member vanish a role or a member's grant). `gapped` (drives older-
    // paging) stays fold_head's per-entity flag.
    let mut role_cands: BTreeMap<String, Vec<AuthorityCand>> = BTreeMap::new();
    let mut grant_cands: BTreeMap<String, Vec<AuthorityCand>> = BTreeMap::new();
    let mut gapped = false;

    for (eid, group) in &groups {
        // The banlist is folded author-aware AFTER the roster is known (below).
        if *eid == banlist_eid {
            continue;
        }
        let entity_hex = crate::simd::hex::bytes_to_hex_32(eid);
        let fold_eds: Vec<version::Edition> = group.iter().map(|p| p.to_fold_edition()).collect();
        let (_hi, entity_gapped) = fold_head(&fold_eds, floors.get(&entity_hex));
        gapped |= entity_gapped;
        let floor_v = floors.get(&entity_hex).map(|f| f.0).unwrap_or(0);

        for p in group {
            // Refuse-downgrade: never consider an edition below the persisted floor.
            if p.version < floor_v {
                continue;
            }
            let head = FoldedHead { entity_hex: entity_hex.clone(), version: p.version, self_hash: p.self_hash, inner_id: p.inner_id };
            match p.vsk.as_str() {
                vsk::ROLE => {
                    // Bind: the content's role_id IS the coordinate; position 0 is the owner's.
                    if let Some(role) = super::roles::parse_role_content(&p.content) {
                        if role.role_id == entity_hex && role.position != 0 {
                            role_cands.entry(entity_hex.clone()).or_default().push(AuthorityCand { role: Some(role), grant: None, author: p.author, head });
                        }
                    }
                }
                vsk::GRANT => {
                    if let Some(mut grant) = super::roles::parse_grant_content(&p.content) {
                        if let Some(member) = crate::simd::hex::hex_to_bytes_32_checked(&grant.member) {
                            if super::derive::grant_locator(cid, &member) == *eid {
                                grant.role_ids.truncate(super::roles::MAX_ROLES_PER_MEMBER);
                                grant_cands.entry(entity_hex.clone()).or_default().push(AuthorityCand { role: None, grant: Some(grant), author: p.author, head });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for cands in role_cands.values_mut().chain(grant_cands.values_mut()) {
        cands.sort_by(|a, b| b.head.version.cmp(&a.head.version).then(a.head.inner_id.cmp(&b.head.inner_id)));
    }

    let empty: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Preliminary roster (bans not yet applied) — the authority view the banlist head
    // is judged against.
    let (prelim, _) = select_authorized(&role_cands, &grant_cands, owner_hex.as_deref(), &empty);

    // Banlist (CORD-04 §4), folded AUTHORITY-aware so its two anti-roster hazards are
    // both closed:
    //   - head selection: the head is the highest version whose author CURRENTLY holds
    //     BAN — an unauthorized higher-version edition can't erase existing bans
    //     (fail-open), and the floor never advances to one;
    //   - per-target: each entry is kept only if the author STRICTLY OUTRANKS that
    //     target (`can_act_on_member` — an admin can't ban a peer/superior, and the
    //     owner is unbannable);
    //   - withholding: when no authorized head is served, the persisted banlist is
    //     RETAINED (an anti-roster must not un-ban on a relay withholding the ban).
    let persisted_banned: Vec<String> = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
    // An ALREADY-banned npub can't author the banlist (a banned member vanishes, §4), or
    // a BAN-holder whose grant-strip hasn't yet folded could publish a list omitting their
    // OWN ban to un-ban themselves (removals aren't outrank-checked). Exclude them from
    // head eligibility, not just from the roster.
    let banned_authors: std::collections::HashSet<&str> = persisted_banned.iter().map(String::as_str).collect();
    let banlist_authored: Vec<&ParsedEdition> = groups
        .get(&banlist_eid)
        .map(|g| {
            g.iter()
                .copied()
                .filter(|e| {
                    let ah = e.author.to_hex();
                    !banned_authors.contains(ah.as_str()) && prelim.is_authorized(&ah, owner_hex.as_deref(), Permissions::BAN)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut banlist_persist: Option<(Vec<String>, u64)> = None;
    let mut banlist_head: Option<FoldedHead> = None;
    let banned: std::collections::BTreeSet<String> = if banlist_authored.is_empty() {
        persisted_banned.into_iter().collect()
    } else {
        let fold_eds: Vec<version::Edition> = banlist_authored.iter().map(|p| p.to_fold_edition()).collect();
        let (hi, g) = fold_head(&fold_eds, floors.get(&banlist_hex));
        gapped |= g;
        match hi {
            Some(hi) => {
                let head = banlist_authored[hi];
                let ah = head.author.to_hex();
                let list: Vec<String> = super::roles::parse_banlist_content(&head.content)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| prelim.can_act_on_member(&ah, owner_hex.as_deref(), t, Permissions::BAN))
                    .take(super::roles::MAX_BANLIST)
                    .collect();
                banlist_head = Some(FoldedHead { entity_hex: banlist_hex.clone(), version: head.version, self_hash: head.self_hash, inner_id: head.inner_id });
                banlist_persist = Some((list.clone(), head.version));
                list.into_iter().collect()
            }
            None => persisted_banned.into_iter().collect(),
        }
    };

    // Final roster (CORD-04 §4: a banned npub vanishes — every edition it authored is
    // dropped, and a grant TO a banned member carries no rank). Re-run selection with
    // the banned set excluded so a banned admin loses authority.
    let (mut authorized, mut heads) = select_authorized(&role_cands, &grant_cands, owner_hex.as_deref(), &banned);
    if let Some(bh) = banlist_head {
        heads.push(bh);
    }

    // Cap the AUTHORIZED community at the 100 lowest role_ids — applied AFTER
    // authorization, so an attacker's unauthorized roles can't consume cap slots and
    // evict a legitimate one (the pre-authorize cap they replace let 100 forged low-id
    // roles empty the roster).
    if authorized.roles.len() > super::roles::MAX_ROLES_PER_COMMUNITY {
        authorized.roles.sort_by(|a, b| a.role_id.cmp(&b.role_id));
        authorized.roles.truncate(super::roles::MAX_ROLES_PER_COMMUNITY);
        let kept: std::collections::HashSet<&str> = authorized.roles.iter().map(|r| r.role_id.as_str()).collect();
        authorized.grants.iter_mut().for_each(|g| g.role_ids.retain(|rid| kept.contains(rid.as_str())));
        authorized.grants.retain(|g| !g.role_ids.is_empty());
    }

    AuthoritySet { roles: authorized, banned, heads, gapped, banlist_persist }
}

/// One candidate edition of a role/grant entity — the pool [`select_authorized`]
/// draws the highest AUTHORIZED head from (exactly one of `role`/`grant` is set).
struct AuthorityCand {
    role: Option<crate::community::roles::Role>,
    grant: Option<crate::community::roles::MemberGrant>,
    author: PublicKey,
    head: FoldedHead,
}

/// The owner-seeded delegation fixpoint (CORD-04 §1/§2), author-AWARE: per entity it
/// takes the highest-version candidate whose author is authorized to author it under
/// the roster resolved SO FAR, dropping unauthorized higher versions rather than
/// vanishing the entity. Authority resolves outward from the owner (proven by
/// `community_id`, never a Role), and the strict-outrank rule (no edition at/above its
/// signer's own position) keeps the fixpoint monotone, so it converges. Returns the
/// authorized roster plus the per-entity heads of the SELECTED editions (the floor
/// advances only to authorized heads — an unauthorized forgery never poisons it).
fn select_authorized(
    role_cands: &std::collections::BTreeMap<String, Vec<AuthorityCand>>,
    grant_cands: &std::collections::BTreeMap<String, Vec<AuthorityCand>>,
    owner_hex: Option<&str>,
    excluded: &std::collections::BTreeSet<String>,
) -> (crate::community::roles::CommunityRoles, Vec<FoldedHead>) {
    use crate::community::roles::{CommunityRoles, Permissions};
    let mut accepted = CommunityRoles::default();
    let mut heads: Vec<FoldedHead> = Vec::new();
    // Jacobi iteration: authority propagates one delegation level per round, so a
    // generous multiple of the entity count is an ample bound. Non-convergence (never
    // seen for an owner-rooted chain) falls through fail-safe: only authorized editions
    // are ever selected.
    let bound = 2 * (role_cands.len() + grant_cands.len()) + 8;
    for _ in 0..bound {
        let mut next = CommunityRoles::default();
        let mut next_heads: Vec<FoldedHead> = Vec::new();

        for cands in role_cands.values() {
            for c in cands {
                let Some(role) = &c.role else { continue };
                let ah = c.author.to_hex();
                if excluded.contains(&ah) {
                    continue;
                }
                if role.position != 0 && accepted.can_act_on_position(&ah, owner_hex, role.position, Permissions::MANAGE_ROLES) {
                    next.roles.push(role.clone());
                    next_heads.push(c.head.clone());
                    break; // highest authorized candidate for this entity
                }
            }
        }
        for cands in grant_cands.values() {
            for c in cands {
                let Some(grant) = &c.grant else { continue };
                let ah = c.author.to_hex();
                if excluded.contains(&ah) || excluded.contains(&grant.member) {
                    continue;
                }
                // The granter must outrank every granted role (resolved against the
                // accepted roster) AND the member — the escalation defense (CORD-04 §2).
                let positions: Option<Vec<u32>> = grant.role_ids.iter().map(|rid| accepted.role(rid).map(|r| r.position)).collect();
                let Some(positions) = positions else { continue };
                if positions.iter().all(|p| accepted.can_act_on_position(&ah, owner_hex, *p, Permissions::MANAGE_ROLES))
                    && accepted.can_act_on_member(&ah, owner_hex, &grant.member, Permissions::MANAGE_ROLES)
                {
                    // Record the head even for an EMPTY grant (a revoke is a real chain
                    // advance a completeness check must see), but don't carry the husk
                    // into the roster.
                    next_heads.push(c.head.clone());
                    if !grant.role_ids.is_empty() {
                        next.grants.push(grant.clone());
                    }
                    break;
                }
            }
        }

        let converged = next.roles == accepted.roles && next.grants == accepted.grants;
        accepted = next;
        heads = next_heads;
        if converged {
            break;
        }
    }
    (accepted, heads)
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
/// updates an existing one, a brand-new PUBLIC channel is added, and a brand-new
/// PRIVATE one is recorded KEYLESS (unreadable until its key arrives over the
/// rekey plane or a fresh bundle). Returns whether anything changed.
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
            // Public → private CONVERSION is DEFERRED: the flip is IGNORED here (the
            // record stays public) until the convert flow (key mint + cursor rebase
            // to the conversion's channel epoch) lands — the send side refuses to
            // publish one, and a foreign client's conversion won't move us.
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
        None => {
            // A brand-new PRIVATE channel: record it KEYLESS at epoch 0 (the root
            // generation — CORD-03 §2 numbers the first private key epoch 1). The
            // epoch then doubles as [`follow_rekeys`]' scan cursor. Until a rotation
            // delivers a key, every read/send/subscribe path skips the channel; the
            // root-fallback in `channel_secret` is never taken for it.
            out.channels.push(ChannelV2 { id, name: meta.name, private: true, key: None, epoch: Epoch(0) });
            true
        }
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
    /// An owner tombstone sits on the dissolved plane (CORD-02 §9) — the local
    /// flag is already set; the caller surfaces the death and stops following.
    pub dissolved: bool,
}

/// The most archived base roots a channel-rekey lookup fans across per step. A
/// standalone rekey rides the minter's then-current root and a removal's rides the
/// PRIOR root (CORD-06 §3), so a follower whose base already advanced must look
/// back. A channel stranded DEEPER than this (its next-epoch crate addressed under
/// an older root than the fan reaches) only heals via a fresh invite bundle — the
/// walk is strictly sequential, so a later rotation can't be reached either.
const MAX_ADDRESSING_ROOTS: usize = 8;

/// Follow rekeys for a held community: advance the base (root) epoch and each
/// Private channel's epoch as far as authorized rotations allow, adopting the
/// fresh key we're still a recipient of at each step and dropping a scope we've
/// been removed from. Persists the result. Called when a rekey wrap arrives in
/// realtime so a long-running bot keeps decrypting after a rotation instead of
/// going silent.
///
/// **Authority (CORD-06 §Authority):** a BASE rotation is honored from the owner
/// only — the deliberate mirror of the owner-only Refounding send (a non-owner's
/// ban silences + strips; the read-cut is the owner's). A CHANNEL rotation is
/// honored from the owner or a `MANAGE_CHANNELS` holder under the PERSISTED
/// roster (folded + persisted by `follow_control`), minus the banlist — so an
/// admin-created private channel keys up on every member.
///
/// **Addressing fans across held base roots:** each channel step queries its
/// next-epoch rekey address under the current root AND the archived prior roots,
/// so a base adopt landing before a Refounding's prior-root-addressed channel
/// rekeys (or before a creation delivery minted under an older root) can't
/// strand the channel.
///
/// **Continuity + fork resolution are spec-strict:** a rotation must extend the
/// exact `(epoch, key)` I hold, one epoch at a time; a same-epoch fork resolves
/// by the lexicographically lowest new key ([`rekey::lowest_key_winner`]), so
/// every follower converges. An incomplete rotation (a missing chunk) never
/// concludes removal — it just waits. A KEYLESS channel (announced by vsk-2, key
/// not yet delivered) holds no chain, so continuity is vacuous for it (CORD-06
/// §2: "a convergence check, not a secrecy mechanism") — authority is its
/// boundary; its epoch is the scan cursor, advancing past complete rotations
/// that exclude us so the walk converges on the channel's current epoch.
pub async fn follow_rekeys<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
) -> Result<RekeyFollow, String> {
    // Death wins every race (CORD-02 §9): a dissolved community honors no epoch advance
    // past its tombstone — don't adopt a rotation into a grave.
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    if crate::db::community::get_community_dissolved(&cid_hex).unwrap_or(false) {
        return Ok(RekeyFollow { updated: None, self_removed: false, dissolved: true });
    }
    // An offline member must also LEARN of a death: the tombstone rides its own
    // public plane, which the live sub watches but no catch-up fetch touched —
    // without this, a member who slept through a dissolution follows (and posts
    // into) a grave forever. Fail-open on transport failure: availability is
    // never death.
    if is_dissolved(transport, community).await {
        if session.is_valid() {
            let _ = crate::db::community::set_community_dissolved(&cid_hex);
        }
        return Ok(RekeyFollow { updated: None, self_removed: false, dissolved: true });
    }
    let me = local_keys()?;
    let my_xonly = me.public_key().to_bytes();
    let owner = community.owner()?;
    let owner_hex = owner.to_hex();
    let mut cur = community.clone();
    let mut changed = false;

    // The channel-rotator gates. Loaded once per follow: a roster change lands via
    // follow_control (which the worker runs right after this), so at worst an
    // admin's rotation adopts one pass late — never early.
    let roster = crate::db::community::get_community_roles(&cid_hex).unwrap_or_default();
    let banned = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
    let me_hex = me.public_key().to_hex();
    let channel_rotator_ok = |rotator: &PublicKey| -> bool {
        if *rotator == owner {
            return true;
        }
        let rh = rotator.to_hex();
        !banned.contains(&rh) && roster.is_authorized(&rh, Some(&owner_hex), crate::community::roles::Permissions::MANAGE_CHANNELS)
    };
    // Concluding MY removal takes more than the bit: the rotator must strictly
    // outrank ME (CORD-06 §Authority — "the Rotator must strictly outrank every
    // removed target"), so an equal-rank admin can never silently evict a peer
    // (or the owner) by minting a complete rotation that skips their blob.
    let channel_rotator_outranks_me = |rotator: &PublicKey| -> bool {
        if *rotator == owner {
            return true;
        }
        let rh = rotator.to_hex();
        !banned.contains(&rh) && roster.can_act_on_member(&rh, Some(&owner_hex), &me_hex, crate::community::roles::Permissions::MANAGE_CHANNELS)
    };
    let base_rotator_ok = |rotator: &PublicKey| -> bool { *rotator == owner };

    // Bound the catch-up: each real step consumes a valid authorized rotation, so a
    // finite chain terminates naturally; the cap defends against a relay feeding a
    // pathological set.
    const MAX_STEPS: usize = 128;
    for _ in 0..MAX_STEPS {
        let mut advanced = false;

        // The roots a channel rekey may be addressed under, freshest first: the
        // current root plus the archived priors (re-read each pass — a base adopt
        // below changes the head, and its predecessor is already archived).
        let mut addressing_roots: Vec<[u8; 32]> = vec![cur.community_root];
        let mut archived = crate::db::community::held_epoch_keys(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap_or_default();
        archived.sort_by(|a, b| b.0 .0.cmp(&a.0 .0));
        for (_, r) in archived {
            if !addressing_roots.contains(&r) {
                addressing_roots.push(r);
            }
        }
        addressing_roots.truncate(MAX_ADDRESSING_ROOTS);

        // Private channels first: a removal-forced channel rekey rides the PRIOR
        // root (CORD-06 D2), so read channels before a base adopt moves it.
        let channel_ids: Vec<ChannelId> = cur.channels.iter().filter(|c| c.private).map(|c| c.id).collect();
        for cid in channel_ids {
            let (held_key, held_epoch) = match cur.channel(&cid) {
                Some(ch) => (ch.key, ch.epoch),
                None => continue,
            };
            let next = Epoch(held_epoch.0.saturating_add(1));
            let mut batches: Vec<(Vec<rekey::RekeyChunk>, Option<(Epoch, [u8; 32])>)> = Vec::new();
            for root in &addressing_roots {
                let group = channel_rekey_group_key(root, &cid, next);
                let chunks = fetch_rekey_chunks(transport, &cur.relays, &group).await?;
                if chunks.is_empty() {
                    continue;
                }
                batches.push((chunks, held_key.map(|k| (held_epoch, k))));
            }
            // Keyless-adopt residual (documented, deferred hardening): a malicious
            // AUTHORIZED admin can fork a keyless member onto an orphan low-key
            // rotation nothing extends (keyed members' continuity filters it out).
            // Recoverable via a fresh bundle; an insider with MANAGE_CHANNELS can
            // exclude the member outright anyway, so the marginal harm is the wedge
            // outliving their demotion.
            match advance_scope(&batches, RekeyScope::Channel(cid), &channel_rotator_ok, &channel_rotator_outranks_me, me.secret_key(), &my_xonly, next) {
                Advance::Adopt { new_key } => {
                    if let Some(ch) = cur.channels.iter_mut().find(|c| c.id.0 == cid.0) {
                        ch.key = Some(new_key);
                        ch.epoch = next;
                    }
                    // The adopter's own multi-epoch archive (the minter archived at
                    // mint) — this channel's history stays readable across rotations.
                    // fetch_channel compensates for the CURRENT epoch, so a failed
                    // archive only bites after the NEXT rotation — surface it.
                    if let Err(e) = crate::db::community::store_epoch_key(&cid_hex, &crate::simd::hex::bytes_to_hex_32(&cid.0), next.0, &new_key) {
                        crate::log_warn!("v2: channel epoch-key archive failed (history across this rotation may not read back): {e}");
                    }
                    advanced = true;
                    changed = true;
                }
                Advance::Removed => {
                    match held_key {
                        // A complete rotation dropped my blob — cut from the channel.
                        Some(_) => {
                            cur.channels.retain(|c| c.id.0 != cid.0);
                        }
                        // Keyless scan: this epoch's rotation completed without me.
                        // Advance the cursor so the walk converges on the channel's
                        // CURRENT epoch — my entry point is its next rotation (whose
                        // recipients are the members at that time) or a fresh bundle.
                        None => {
                            if let Some(ch) = cur.channels.iter_mut().find(|c| c.id.0 == cid.0) {
                                ch.epoch = next;
                            }
                        }
                    }
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
            let batches = vec![(chunks, Some((held_epoch, held_key)))];
            match advance_scope(&batches, RekeyScope::Root, &base_rotator_ok, &base_rotator_ok, me.secret_key(), &my_xonly, next) {
                Advance::Adopt { new_key } => {
                    cur.community_root = new_key;
                    cur.root_epoch = next;
                    // Archive on adopt: without this, a member who lived through TWO
                    // Refoundings loses the middle epoch's public history (only the
                    // minter archived it).
                    if let Err(e) = crate::db::community::store_epoch_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, next.0, &new_key) {
                        crate::log_warn!("v2: base epoch-key archive failed (this epoch's history may not read back after the next rotation): {e}");
                    }
                    advanced = true;
                    changed = true;
                }
                Advance::Removed => {
                    if !session.is_valid() {
                        return Err("account changed during rekey follow".to_string());
                    }
                    return Ok(RekeyFollow { updated: None, self_removed: true, dissolved: false });
                }
                Advance::Stay => {}
            }
        }

        if !advanced {
            break;
        }
    }

    if !changed {
        return Ok(RekeyFollow { updated: None, self_removed: false, dissolved: false });
    }
    if !session.is_valid() {
        return Err("account changed during rekey follow".to_string());
    }
    // A leave/delete raced this follow: saving would resurrect the community row
    // (the save is an upsert) with no floor rows behind it.
    if crate::db::community::community_protocol(community.id())?.is_none() {
        return Ok(RekeyFollow { updated: None, self_removed: false, dissolved: false });
    }
    crate::db::community::save_community_v2(&cur)?;
    Ok(RekeyFollow { updated: Some(cur), self_removed: false, dissolved: false })
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
    // A rekey plane address is community_root-derived, so ANY member can seal junk
    // 3303s there — a flood (or, organically, a large community's own multi-chunk
    // rotation past the newest window) could bury the genuine owner/admin rotation
    // in a single fixed page. PAGE backwards (inclusive until + wrap-id dedup, the
    // control pager's discipline) so a buried authorized chunk is still recovered;
    // the seal + authority filter downstream drops the junk. Bounded — a sustained
    // flood past this depth degrades to "adopt one pass late", never a false state.
    const REKEY_PAGE: usize = 200;
    const REKEY_MAX_PAGES: usize = 6;
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    let mut until: Option<u64> = None;
    let mut oldest: Option<u64> = None;
    for _ in 0..REKEY_MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![group.pk_hex()],
            until,
            limit: Some(REKEY_PAGE),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, relays).await?;
        let mut fresh = 0usize;
        for w in &wraps {
            if !seen.insert(w.id) {
                continue;
            }
            fresh += 1;
            let at = w.created_at.as_secs();
            if oldest.is_none_or(|o| at < o) {
                oldest = Some(at);
            }
            if let Ok(opened) = stream::open_wrap(w, group) {
                if let Ok(chunk) = rekey::parse_rekey_chunk(&opened) {
                    out.push(chunk);
                }
            }
        }
        // Drained, or a same-second wall the pager can't step past (second-granular
        // until) — either way stop; the accumulated set is what advance_scope folds.
        if fresh == 0 || wraps.len() < REKEY_PAGE {
            break;
        }
        match oldest {
            Some(o) if o > 0 => until = Some(o),
            _ => break,
        }
    }
    Ok(out)
}

/// Decide how a scope advances from per-addressing-root chunk batches (pure). Each
/// batch pairs the chunks fetched under one root with the continuity to demand of
/// them: a rotation qualifies when it's rotator-authorized (`rotator_ok`),
/// complete, targets the immediate `next_epoch`, and — when I hold a chain —
/// extends my exact `(epoch, key)`. A KEYLESS batch (`held` = None) has no chain
/// to extend, so it qualifies on authority + completeness alone (CORD-06 §2:
/// continuity is "a convergence check, not a secrecy mechanism"; the rotator's
/// seal authority is the boundary). Among qualifying rotations carrying my blob
/// the lexicographically lowest new key wins (convergent). All complete
/// candidates without my blob conclude Removed for a KEYED holder only when one
/// came from a rotator who may remove ME (`rotator_may_remove_me`, the CORD-06
/// strict-outrank rule) — else Stay; for a keyless holder they merely advance the
/// scan cursor (any bit-holder's real rotation is scan progress, never a loss).
fn advance_scope(
    batches: &[(Vec<rekey::RekeyChunk>, Option<(Epoch, [u8; 32])>)],
    scope: RekeyScope,
    rotator_ok: &dyn Fn(&PublicKey) -> bool,
    rotator_may_remove_me: &dyn Fn(&PublicKey) -> bool,
    my_sk: &SecretKey,
    my_xonly: &[u8; 32],
    next_epoch: Epoch,
) -> Advance {
    let mut winners: Vec<[u8; 32]> = Vec::new();
    let mut saw_complete_candidate = false;
    let mut saw_outranking_candidate = false;
    let keyed = batches.iter().any(|(_, held)| held.is_some());
    for (chunks, held) in batches {
        let rotations = rekey::collect_rotations(chunks);
        for r in &rotations {
            if !rotator_ok(&r.rotator) || r.scope.id32() != scope.id32() || r.new_epoch.0 != next_epoch.0 || !r.is_complete() {
                continue;
            }
            if let Some((held_epoch, held_key)) = held {
                if r.continuity(*held_epoch, held_key) != Continuity::Extends {
                    continue;
                }
            }
            saw_complete_candidate = true;
            saw_outranking_candidate |= rotator_may_remove_me(&r.rotator);
            if let Some(blob) = rekey::find_my_blob(&r.blobs, &r.rotator.to_bytes(), my_xonly, r.scope, r.new_epoch) {
                if let Ok(k) = rekey::open_blob_local(my_sk, &r.rotator, r.scope, r.new_epoch, blob) {
                    winners.push(k);
                }
            }
        }
    }
    if !winners.is_empty() {
        // `collect_rotations` correlates on `(rotator, scope, new_epoch, prev_commit)`,
        // so a single rotator's blobs merge into ONE rotation (and a retried Refounding
        // MINT-OR-REUSES its root, so it never emits two distinct roots to fork on).
        // The lowest-key tiebreak engages only for CONCURRENT DISTINCT rotators racing
        // the same epoch (separate rotations): every follower converges on the same
        // lowest new key. A wrap served under two addressing roots can't double-count:
        // each rekey wrap opens under exactly one root's group key.
        let idx = rekey::lowest_key_winner(&winners).expect("winners is non-empty");
        return Advance::Adopt { new_key: winners[idx] };
    }
    if saw_complete_candidate && (!keyed || saw_outranking_candidate) {
        Advance::Removed
    } else {
        Advance::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::transport::memory::MemoryRelay;
    use super::*;
    use crate::community::roles::{MemberGrant, Permissions, Role, RoleScope};

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

    async fn texts_in<T: crate::community::transport::Transport + ?Sized>(relay: &T, community: &CommunityV2, channel: &ChannelId) -> Vec<String> {
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
    /// The entity's current head `self_hash` on the relay (highest version wins),
    /// so a helper can chain a new edition the way a real owner client does.
    async fn head_hash_on_relay(relay: &MemoryRelay, community: &CommunityV2, entity_id: &[u8; 32]) -> Option<[u8; 32]> {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let query = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
        let wraps = relay.fetch(&query, &community.relays).await.ok()?;
        let mut head: Option<(u64, [u8; 32])> = None;
        for w in &wraps {
            if let Ok((ed, _)) = control::open_control_edition(w, &group) {
                if ed.entity_id == *entity_id && head.is_none_or(|(v, _)| ed.version > v) {
                    head = Some((ed.version, ed.self_hash));
                }
            }
        }
        head.map(|(_, h)| h)
    }

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
        let prev = head_hash_on_relay(relay, community, &channel_id.0).await;
        let meta = control::ChannelMetadata { name: name.into(), private, deleted: deleted.then_some(true), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::CHANNEL_METADATA, &channel_id.0, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish an owner-grammar community-metadata edition (rename etc.), chained
    /// to the current relay head like a real owner client.
    async fn publish_community_meta(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, name: &str, version: u64) {
        publish_community_meta_at(relay, community, signer, name, version, 1_000).await;
    }

    /// As [`publish_community_meta`] with an explicit timestamp, for tests that need
    /// relay-side newest-first ordering (paging/eviction scenarios).
    async fn publish_community_meta_at(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, name: &str, version: u64, at_secs: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let prev = head_hash_on_relay(relay, community, &community.id().0).await;
        let meta = control::CommunityMetadata { name: name.into(), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, version, prev.as_ref(), &content, at_secs, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(at_secs)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish a Role edition (vsk 1) signed by `signer`, chained to the current head.
    async fn publish_role(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, role: &Role, version: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let role_id = crate::simd::hex::hex_to_bytes_32_checked(&role.role_id).unwrap();
        let prev = head_hash_on_relay(relay, community, &role_id).await;
        let content = crate::community::v2::roles::role_content_json(role).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::ROLE, &role_id, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish a Grant edition (vsk 3) signed by `signer`, at grant_locator(cid, member).
    async fn publish_grant(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, member: &PublicKey, role_ids: Vec<String>, version: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let eid = crate::community::v2::derive::grant_locator(community.id(), &member.to_bytes());
        let prev = head_hash_on_relay(relay, community, &eid).await;
        let grant = MemberGrant { member: member.to_hex(), role_ids };
        let content = crate::community::v2::roles::grant_content_json(&grant).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::GRANT, &eid, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish a Banlist edition (vsk 4) signed by `signer`, at banlist_locator(cid).
    async fn publish_banlist(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, banned: &[String], version: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let eid = crate::community::v2::derive::banlist_locator(community.id());
        let prev = head_hash_on_relay(relay, community, &eid).await;
        let content = crate::community::v2::roles::banlist_content_json(banned).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::BANLIST, &eid, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    fn admin_role(role_id: &str, perms: u64) -> Role {
        Role { role_id: role_id.into(), name: "Admin".into(), position: 1, permissions: Permissions(perms), scope: RoleScope::Server, color: 0 }
    }

    // ── CORD-04 §1 author-aware fold: a seat-holder (holds community_root, so can seal
    // any control edition) must not be able to SUPPRESS a role or grant by forging a
    // higher version at its coordinate. Owner-only signers mask this entirely, so every
    // attacker below signs as a NON-owner member.

    #[tokio::test]
    async fn a_non_owner_cannot_suppress_the_admin_role_by_forging_a_higher_version() {
        let (bed, owner, attacker) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "AttackA", bed.relays.clone(), None).await.unwrap();
        let victim = Keys::generate().public_key();
        grant_admin(&bed.relay, &community, &victim).await.unwrap();

        // The admin role sits at a deterministic, publicly-computable coordinate.
        let admin_rid = fetch_authority(&bed.relay, &community)
            .await
            .roles
            .roles
            .iter()
            .find(|r| r.permissions.contains(Permissions::ADMIN_ALL))
            .unwrap()
            .role_id
            .clone();
        // Attacker forges v2 of that exact role, stripping its powers.
        publish_role(
            &bed.relay,
            &community,
            &attacker.keys,
            &Role { role_id: admin_rid.clone(), name: "pwned".into(), position: 1, permissions: Permissions(0), scope: RoleScope::Server, color: 0 },
            2,
        )
        .await;

        let authority = fold_authority(&community, &fetch_control(&bed.relay, &community).await, &load_floors(&community));
        assert!(authority.roles.is_admin(&victim.to_hex()), "the forged strip is DROPPED; the owner's admin role survives beneath it");
        assert!(
            authority.heads.iter().any(|h| h.entity_hex == admin_rid && h.version == 1),
            "the floor advances only to the AUTHORIZED head (owner v1)"
        );
        assert!(!authority.heads.iter().any(|h| h.version == 2), "the forged v2 never poisons the floor");
    }

    #[tokio::test]
    async fn a_non_owner_cannot_strip_a_members_grant_by_forging_a_higher_version() {
        let (bed, owner, attacker) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "AttackC", bed.relays.clone(), None).await.unwrap();
        let victim = Keys::generate();
        grant_admin(&bed.relay, &community, &victim.public_key()).await.unwrap();

        // Attacker forges a higher-version EMPTY grant at the victim's grant coordinate.
        publish_grant(&bed.relay, &community, &attacker.keys, &victim.public_key(), vec![], 9).await;

        let authority = fold_authority(&community, &fetch_control(&bed.relay, &community).await, &load_floors(&community));
        assert!(
            authority.roles.is_admin(&victim.public_key().to_hex()),
            "the forged strip is dropped; the owner's grant survives and the victim keeps admin"
        );
    }

    #[tokio::test]
    async fn forged_low_id_roles_by_a_non_owner_never_enter_the_authorized_roster() {
        let (bed, owner, attacker) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "AttackB", bed.relays.clone(), None).await.unwrap();
        let victim = Keys::generate().public_key();
        grant_admin(&bed.relay, &community, &victim).await.unwrap();

        // Low-id roles that WOULD evict the admin from a pre-authorize cap — but they're
        // unauthorized, so the post-authorize cap never sees them.
        for i in 0u8..6 {
            let rid = crate::simd::hex::bytes_to_hex_32(&[i; 32]);
            publish_role(&bed.relay, &community, &attacker.keys, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        }

        let authority = fold_authority(&community, &fetch_control(&bed.relay, &community).await, &load_floors(&community));
        assert!(authority.roles.is_admin(&victim.to_hex()), "the legit admin survives the forged flood");
        assert_eq!(authority.roles.roles.len(), 1, "only the owner's admin role is authorized; every forgery is dropped");
    }

    /// A transport that ACKs publishes but ERRORS every fetch — a relay outage / withhold.
    struct FetchErrors(MemoryRelay);
    #[async_trait::async_trait]
    impl crate::community::transport::Transport for FetchErrors {
        async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.0.publish(e, r).await
        }
        async fn fetch(&self, _q: &Query, _r: &[String]) -> Result<Vec<Event>, String> {
            Err("relay down".to_string())
        }
        async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.0.publish_durable(e, r).await
        }
    }

    #[tokio::test]
    async fn fetch_authority_retains_the_persisted_banlist_on_a_transport_error() {
        let (bed, owner, victim) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "BanRetain", bed.relays.clone(), None).await.unwrap();
        let victim_hex = victim.keys.public_key().to_hex();
        // A ban is persisted locally (as a completed set_banlist + follow leaves it).
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::set_community_banlist(&cid_hex, &[victim_hex.clone()], 1).unwrap();

        // A relay that ERRORS on fetch must degrade FAIL-SAFE: retain the ban, never
        // return an empty banlist (which would silently un-ban on withheld data).
        let down = FetchErrors(MemoryRelay::new());
        let view = fetch_authority(&down, &community).await;
        assert!(view.banned.contains(&victim_hex), "a transport error retains the persisted banlist");
    }

    #[tokio::test]
    async fn follow_control_retains_the_roster_when_a_floored_role_ages_out() {
        let (bed, owner, _m) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Complete", bed.relays.clone(), None).await.unwrap();
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let (a, b) = (Keys::generate().public_key(), Keys::generate().public_key());
        let rid = crate::simd::hex::bytes_to_hex_32(&[0x7c; 32]);

        // Full state on relay1: an Admin role + two grants → both fold + persist as admins.
        publish_role(&bed.relay, &community, &owner.keys, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &a, vec![rid.clone()], 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &b, vec![rid.clone()], 1).await;
        let session = crate::state::SessionGuard::capture();
        follow_control(&bed.relay, &community, &session).await.unwrap();
        assert!(crate::db::community::get_community_roles(&cid_hex).unwrap().is_admin(&a.to_hex()), "seeded");

        // relay2 serves A's grant but NOT the role (aged out of the window): the fold
        // drops both admins yet raises no gap. The completeness gate must RETAIN the
        // stored roster rather than persist the lossy one.
        let relay2 = MemoryRelay::new();
        publish_grant(&relay2, &community, &owner.keys, &a, vec![rid.clone()], 1).await;
        follow_control(&relay2, &community, &session).await.unwrap();
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap();
        assert!(roster.is_admin(&a.to_hex()) && roster.is_admin(&b.to_hex()), "a floored-but-unfetched role retains the stored roster");
    }

    #[tokio::test]
    async fn an_authorized_admin_edits_metadata_but_a_demoted_one_cannot() {
        // CORD-04 §5: an admin holding MANAGE_METADATA renames the community; once the
        // owner revokes the grant, the (now unauthorized) admin's further edit drops
        // and the name holds at the last authorized state.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Base", vec!["wss://r".into()], None).await.unwrap();
        let admin = Keys::generate();
        let rid = "a1".repeat(32);
        publish_role(&relay, &community, &owner, &admin_role(&rid, Permissions::MANAGE_METADATA), 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![rid.clone()], 1).await;
        publish_community_meta(&relay, &community, &admin, "Admin Rename", 2).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("admin edit authorized");
        assert_eq!(updated.name, "Admin Rename", "an admin with MANAGE_METADATA renames");

        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![], 2).await; // revoke
        publish_community_meta(&relay, &community, &admin, "Demoted Rename", 3).await;
        let _ = follow_control(&relay, &community, &session).await.unwrap();
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert_eq!(held.name, "Admin Rename", "a demoted admin's edit is dropped; the name holds");
    }

    #[tokio::test]
    async fn a_roleless_member_cannot_edit_metadata() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Guarded", vec!["wss://r".into()], None).await.unwrap();
        let stranger = Keys::generate();
        publish_community_meta(&relay, &community, &stranger, "Hijacked", 2).await;
        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a roleless member's metadata edit never folds"
        );
    }

    #[tokio::test]
    async fn a_self_signed_grant_is_not_authority() {
        // The self-promotion defense: a member self-signs both a role and a grant of
        // it to themselves. authorize_delegation drops both (their signer never traces
        // to the owner), so their metadata edit stays unauthorized.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "NoSelfPromo", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        let rid = "b2".repeat(32);
        publish_role(&relay, &community, &rogue, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&relay, &community, &rogue, &rogue.public_key(), vec![rid.clone()], 1).await;
        publish_community_meta(&relay, &community, &rogue, "Seized", 2).await;
        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a self-signed grant confers no authority"
        );
    }

    #[tokio::test]
    async fn the_banlist_is_enforced_only_from_a_ban_holder() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Bans", vec!["wss://r".into()], None).await.unwrap();
        let target = "cc".repeat(32);

        // A non-BAN-holder's banlist edition is folded but NOT enforced.
        let rogue = Keys::generate();
        publish_banlist(&relay, &community, &rogue, &[target.clone()], 1).await;
        let floors = load_floors(&community);
        let editions = fetch_control(&relay, &community).await;
        let authority = fold_authority(&community, &editions, &floors);
        assert!(authority.banned.is_empty(), "a non-owner (no BAN) banlist is not enforced");

        // The owner (supreme, holds BAN) bans the target: now enforced.
        publish_banlist(&relay, &community, &owner, &[target.clone()], 2).await;
        let editions = fetch_control(&relay, &community).await;
        let authority = fold_authority(&community, &editions, &floors);
        assert!(authority.banned.contains(&target), "the owner's banlist is enforced");
    }

    #[tokio::test]
    async fn a_banned_admin_loses_all_authority() {
        // CORD-04 §4: a banned npub vanishes — even holding an un-stripped grant, a
        // banned admin's authority is dropped and their edits refused.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "BanAuth", vec!["wss://r".into()], None).await.unwrap();
        let admin = Keys::generate();
        let rid = "e5".repeat(32);
        publish_role(&relay, &community, &owner, &admin_role(&rid, Permissions::MANAGE_METADATA), 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![rid.clone()], 1).await;
        publish_banlist(&relay, &community, &owner, &[admin.public_key().to_hex()], 1).await; // ban, grant left intact
        publish_community_meta(&relay, &community, &admin, "Banned Rename", 2).await;

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a banned admin's edit is dropped even with an unstripped grant"
        );
        let authority = fold_authority(&community, &fetch_control(&relay, &community).await, &load_floors(&community));
        assert!(authority.banned.contains(&admin.public_key().to_hex()));
        assert!(
            !authority.roles.is_authorized(&admin.public_key().to_hex(), Some(&owner.public_key().to_hex()), Permissions::MANAGE_METADATA),
            "a banned admin holds no bit"
        );
    }

    #[tokio::test]
    async fn a_ban_holder_cannot_ban_a_superior_or_the_owner() {
        // CORD-04 §3/§5: BAN needs the bit AND a strict outrank of the target. A mod
        // (pos 2, holds BAN) can ban a lower member but NOT a superior admin (pos 1)
        // and NOT the owner (supreme, unbannable).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Ranks", vec!["wss://r".into()], None).await.unwrap();
        let admin = Keys::generate();
        let moder = Keys::generate();
        let stranger = Keys::generate();
        let (admin_rid, mod_rid) = ("a1".repeat(32), "b2".repeat(32));
        publish_role(&relay, &community, &owner, &Role { role_id: admin_rid.clone(), name: "Admin".into(), position: 1, permissions: Permissions(Permissions::ADMIN_ALL), scope: RoleScope::Server, color: 0 }, 1).await;
        publish_role(&relay, &community, &owner, &Role { role_id: mod_rid.clone(), name: "Mod".into(), position: 2, permissions: Permissions(Permissions::BAN), scope: RoleScope::Server, color: 0 }, 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![admin_rid], 1).await;
        publish_grant(&relay, &community, &owner, &moder.public_key(), vec![mod_rid], 1).await;
        publish_banlist(&relay, &community, &moder, &[admin.public_key().to_hex(), owner.public_key().to_hex(), stranger.public_key().to_hex()], 1).await;

        let authority = fold_authority(&community, &fetch_control(&relay, &community).await, &load_floors(&community));
        assert!(!authority.banned.contains(&admin.public_key().to_hex()), "a mod cannot ban a superior admin");
        assert!(!authority.banned.contains(&owner.public_key().to_hex()), "nobody can ban the owner");
        assert!(authority.banned.contains(&stranger.public_key().to_hex()), "the mod CAN ban a lower-ranked member");
    }

    #[tokio::test]
    async fn an_unauthorized_higher_banlist_cannot_unban() {
        // CORD-04 §4 anti-roster fail-CLOSED: a rogue's higher-version empty banlist
        // must not erase the owner's ban (author-aware head selection + persisted
        // banlist retention).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "NoUnban", vec!["wss://r".into()], None).await.unwrap();
        let target = "cc".repeat(32);
        publish_banlist(&relay, &community, &owner, &[target.clone()], 1).await;
        let session = SessionGuard::capture();
        follow_control(&relay, &community, &session).await.unwrap(); // persists the ban

        let rogue = Keys::generate();
        publish_banlist(&relay, &community, &rogue, &[], 2).await; // unauthorized higher, empty
        let authority = fold_authority(&community, &fetch_control(&relay, &community).await, &load_floors(&community));
        assert!(authority.banned.contains(&target), "an unauthorized higher banlist cannot un-ban");
    }

    #[tokio::test]
    async fn the_community_list_syncs_a_membership_to_a_fresh_device() {
        // CORD-02 §8: create publishes the 13302; a fresh device (community dropped
        // locally, the 13302 + genesis still on the relay) rehydrates it on sync.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let relays = vec!["wss://r".to_string()];
        let community = create_community(&relay, "Synced", relays.clone(), None).await.unwrap();
        crate::db::community::delete_community(&crate::simd::hex::bytes_to_hex_32(&community.id().0)).unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_none());

        let rehydrated = sync_community_list(&relay, &relays).await.unwrap();
        assert_eq!(rehydrated.len(), 1, "the left-behind membership rehydrates");
        assert_eq!(rehydrated[0].id().0, community.id().0);
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_some(), "and is now held locally");
    }

    #[tokio::test]
    async fn a_leave_tombstones_the_membership_so_sync_does_not_rejoin() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let relays = vec!["wss://r".to_string()];
        let community = create_community(&relay, "Left", relays.clone(), None).await.unwrap();
        leave_community(&relay, &community).await.unwrap(); // tombstones the 13302 + deletes

        let rehydrated = sync_community_list(&relay, &relays).await.unwrap();
        assert!(rehydrated.is_empty(), "a tombstoned membership is not rejoined on sync");
    }

    #[tokio::test]
    async fn dissolution_blocks_a_join() {
        // CORD-02 §9: the owner dissolves; a would-be joiner resolves the grave and
        // refuses to join.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Doomed", bed.relays.clone(), None).await.unwrap();
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        dissolve_community(&bed.relay, &community).await.unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().unwrap().dissolved, "the owner's local hold is sealed");

        bed.swap_to(&member);
        let err = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap_err();
        assert!(err.contains("dissolved"), "a join refuses a dissolved community: {err}");
    }

    #[tokio::test]
    async fn only_the_owner_can_dissolve() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Mine", bed.relays.clone(), None).await.unwrap();
        bed.swap_to(&member);
        assert!(dissolve_community(&bed.relay, &community).await.is_err(), "only the owner can dissolve");
        assert!(!is_dissolved(&bed.relay, &community).await, "and no tombstone was published");
    }

    #[tokio::test]
    async fn a_foreign_tombstone_is_not_death() {
        // A non-owner sealing the dissolved plane is noise (verify_dissolved is
        // owner-gated), so the community is not treated as dead.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Safe", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        let rumor = crate::community::v2::dissolution::dissolved_tombstone_rumor(rogue.public_key(), community.id(), 1_000);
        let wrap = crate::community::v2::dissolution::seal_dissolved(&rumor, community.id(), &rogue, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
        assert!(!is_dissolved(&relay, &community).await, "a foreign-signed tombstone is not death");
    }

    #[tokio::test]
    async fn a_public_channel_reads_history_across_a_refounding() {
        // CORD-03 §3: after a Refounding rolls the base root, a Public channel's
        // pre-rotation messages stay readable (the prior epoch's root is archived and
        // the read fans out across held epochs).
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "History", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&relay, &community, &general, "before the refounding").await.unwrap();

        let refounded = refound_community(&relay, &community, &[]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the epoch advanced");
        send_message(&relay, &refounded, &general, "after the refounding").await.unwrap();

        let texts = texts_in(&relay, &refounded, &general).await;
        assert!(texts.contains(&"before the refounding".to_string()), "the epoch-0 message is still readable");
        assert!(texts.contains(&"after the refounding".to_string()), "the epoch-1 message reads too");
    }

    #[tokio::test]
    async fn refounding_aborts_when_control_state_is_withheld() {
        // B1 coverage gate (CORD-06 §3): a relay serving none of the committed control
        // heads must ABORT the Refounding — never silently drop state (e.g. unban a
        // member at the new epoch a fresh joiner bootstraps).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Withheld", vec!["wss://good".into()], None).await.unwrap();
        publish_banlist(&relay, &community, &owner, &["cc".repeat(32)], 1).await;
        let session = SessionGuard::capture();
        follow_control(&relay, &community, &session).await.unwrap(); // seed the banlist floor

        // Re-point the held community to an EMPTY relay + save, so the Refounding (which
        // reloads fresh state) fetches none of the committed heads.
        let mut moved = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        moved.relays = vec!["wss://empty".into()];
        crate::db::community::save_community_v2(&moved).unwrap();

        let err = refound_community(&relay, &moved, &[]).await.unwrap_err();
        assert!(err.contains("was not served"), "a withheld control head aborts the refounding: {err}");
        assert_eq!(
            crate::db::community::load_community_v2(community.id()).unwrap().unwrap().root_epoch,
            Epoch(0),
            "the epoch did NOT advance (zero published state)"
        );
    }

    #[tokio::test]
    async fn refounding_rolls_the_root_and_severs_a_removed_member() {
        // CORD-06 §3: the owner re-founds, removing a member. The base root rolls, the
        // epoch advances, and the removed member's rekey-follow concludes they're cut.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Refound", bed.relays.clone(), None).await.unwrap();
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();

        bed.swap_to(&owner);
        let refounded = refound_community(&bed.relay, &community, &[member.keys.public_key()]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the epoch advanced");
        assert_ne!(refounded.community_root, community.community_root, "the base root rolled");
        // The owner still reads the compacted control plane at the new epoch.
        let session = SessionGuard::capture();
        assert_eq!(
            crate::db::community::load_community_v2(community.id()).unwrap().unwrap().root_epoch,
            Epoch(1),
            "the owner committed the new epoch"
        );

        // The removed member, following rekeys, is severed (no blob in the rotation).
        bed.swap_to(&member);
        let follow = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(follow.self_removed, "the removed member is cut by the re-founding");
    }

    #[tokio::test]
    async fn only_the_owner_re_founds_even_a_ban_holding_admin_cannot() {
        // The receive side (advance_scope) honors ONLY the owner's rotation, so the
        // SEND is owner-only — a non-owner BAN-holder's Refounding would fork onto a
        // root nobody follows. Owner grants a member BAN; they still can't re-found.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Guarded", bed.relays.clone(), None).await.unwrap();
        let rid = "b0".repeat(32);
        publish_role(&bed.relay, &community, &owner.keys, &admin_role(&rid, Permissions::BAN), 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &member.keys.public_key(), vec![rid], 1).await;
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert!(refound_community(&bed.relay, &joined, &[owner.keys.public_key()]).await.is_err(), "a non-owner BAN-holder can't re-found");
    }

    #[tokio::test]
    async fn a_retried_refounding_reuses_the_same_root() {
        // B1 idempotency: minting for the same (scope, epoch) twice yields the SAME
        // root, so a retried Refounding re-delivers one root — never a double-mint fork.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Retry", vec!["wss://r".into()], None).await.unwrap();
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let first = mint_or_reuse_rotation_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap();
        let second = mint_or_reuse_rotation_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap();
        assert_eq!(first, second, "a retry reuses the archived root, never double-mints");
    }

    #[tokio::test]
    async fn minting_a_link_makes_the_community_public_and_revoke_makes_it_private() {
        // CORD-05 §5: the Registry is the Public/Private source of truth. Minting a
        // link publishes it (Public); retiring the last link empties it (Private).
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Invitable", vec!["wss://r".into()], None).await.unwrap();
        assert!(!community_is_public(&relay, &community).await, "a fresh community is Private");

        let minted = mint_public_link(&relay, &community, "https://x", None, None).await.unwrap();
        assert!(community_is_public(&relay, &community).await, "a live link makes it Public");
        let list = fetch_invite_list(&relay, &community.relays).await.expect("the 13303 list was published");
        assert_eq!(list.entries.len(), 1, "the minted link is recorded across devices");

        let token_hex = crate::simd::hex::bytes_to_hex_16(&minted.token);
        revoke_public_link(&relay, &community, &token_hex).await.unwrap();
        assert!(!community_is_public(&relay, &community).await, "retiring the last link makes it Private again");
        let after = fetch_invite_list(&relay, &community.relays).await.unwrap();
        assert!(after.entries.is_empty() && after.tombstones.len() == 1, "the link is tombstoned in the invite list");
    }

    #[tokio::test]
    async fn a_registry_from_a_non_create_invite_holder_does_not_make_it_public() {
        // The CREATE_INVITE gate: a rogue publishing a registry can't fake Public.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Gated", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        // Rogue publishes a registry edition at THEIR coordinate with a fake signer.
        let eid = crate::community::v2::derive::invite_links_locator(community.id(), &rogue.public_key().to_bytes());
        let content = crate::community::v2::invite::build_registry_content(&[Keys::generate().public_key()]);
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let rumor = control::build_edition_rumor(rogue.public_key(), vsk::INVITE_LINKS, &eid, 1, None, &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, &rogue, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
        let _ = owner;
        assert!(!community_is_public(&relay, &community).await, "a non-CREATE_INVITE registry is ignored");
    }

    #[tokio::test]
    async fn full_lifecycle_e2e() {
        // The whole stack end to end across two accounts: create -> Public link ->
        // owner grants an admin -> member joins + reads history -> admin edits metadata
        // (authorized fold) -> owner bans the member (CORD-04 §6: banlist + strip +
        // Refounding) -> the banned member is severed AND stays banned across the new
        // epoch -> pre-ban history still reads -> owner dissolves -> sealed.
        let (bed, owner, member) = TestBed::new();

        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Lifecycle", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "owner: welcome").await.unwrap();

        // Public link → the community reads Public.
        let _minted = mint_public_link(&bed.relay, &community, "https://x", None, None).await.unwrap();
        assert!(community_is_public(&bed.relay, &community).await, "a live link makes it Public");

        // Owner defines + grants an Admin role (MANAGE_METADATA among the bits).
        let rid = "aa".repeat(32);
        publish_role(&bed.relay, &community, &owner.keys, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &member.keys.public_key(), vec![rid], 1).await;

        // Member joins from the bundle + reads the owner's message.
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["owner: welcome"]);
        // The admin renames the community.
        publish_community_meta(&bed.relay, &joined, &member.keys, "Lifecycle Renamed", 2).await;

        // Owner follows: the admin's rename folds (authorized).
        bed.swap_to(&owner);
        let session = SessionGuard::capture();
        let updated = follow_control(&bed.relay, &community, &session).await.unwrap().expect("the admin edit folds");
        assert_eq!(updated.name, "Lifecycle Renamed", "an authorized admin's metadata edit is honored");

        // Ban the member (the three-removal composition, in order).
        set_banlist(&bed.relay, &updated, &[member.keys.public_key().to_hex()]).await.unwrap();
        grant_roles(&bed.relay, &updated, &member.keys.public_key(), vec![]).await.unwrap();
        let refounded = refound_community(&bed.relay, &updated, &[member.keys.public_key()]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the ban rolled the root");
        // The ban survives the Refounding (the banlist head compacted forward).
        let post = fold_authority(&refounded, &fetch_control(&bed.relay, &refounded).await, &load_floors(&refounded));
        assert!(post.banned.contains(&member.keys.public_key().to_hex()), "the ban survives the re-founding");
        // Pre-ban history still reads across the new epoch.
        assert!(
            texts_in(&bed.relay, &refounded, &general).await.contains(&"owner: welcome".to_string()),
            "pre-refounding history stays readable"
        );

        // The banned member's rekey-follow concludes they're severed.
        bed.swap_to(&member);
        let follow = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(follow.self_removed, "the banned member is cryptographically cut");

        // Owner dissolves → sealed.
        bed.swap_to(&owner);
        dissolve_community(&bed.relay, &refounded).await.unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().unwrap().dissolved, "the community is sealed");
    }

    /// The deep two-account e2e the way a real deployment runs: owner (A) + member (B)
    /// over one shared relay, create → channels (public + private) → converse both ways →
    /// persist (get_messages-level) → react/edit/delete → moderate (ban/unban) → dissolve.
    /// Every account, community, channel, and action is LOGGED (run with --nocapture) so it
    /// doubles as a reference transcript and a re-runnable regression.
    #[tokio::test]
    async fn a_forged_edition_cannot_suppress_a_role_across_a_refounding() {
        // A member forges a higher-version role edition at the admin coordinate before a
        // refounding. The compaction must carry the AUTHORIZED floor head, not the
        // author-blind version tip — else the forgery is re-anchored, honest folders drop
        // it, and the admin role vanishes at the new epoch (silent suppression).
        let (bed, owner, member) = TestBed::new();
        let attacker = Keys::generate();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "NoSuppress", bed.relays.clone(), None).await.unwrap();
        let rid = crate::simd::hex::bytes_to_hex_32(&[0xa1; 32]);
        publish_role(&bed.relay, &community, &owner.keys, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &member.keys.public_key(), vec![rid.clone()], 1).await;
        // Owner folds → the authorized role/grant heads are floored.
        let session = SessionGuard::capture();
        follow_control(&bed.relay, &community, &session).await.unwrap();
        assert!(fetch_authority(&bed.relay, &community).await.roles.is_admin(&member.keys.public_key().to_hex()), "member is admin pre-attack");

        // The attacker (a non-owner) forges v2 of the admin role, chaining onto v1.
        publish_role(&bed.relay, &community, &attacker, &Role { role_id: rid.clone(), name: "pwn".into(), position: 1, permissions: Permissions(0), scope: RoleScope::Server, color: 0 }, 2).await;

        // Owner refounds (keeping everyone).
        let refounded = refound_community(&bed.relay, &community, &[]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "root rolled");

        // Post-refound, the admin role SURVIVES (the authorized floor head was carried).
        let post = fold_authority(&refounded, &fetch_control(&bed.relay, &refounded).await, &load_floors(&refounded));
        assert!(post.roles.is_admin(&member.keys.public_key().to_hex()), "the admin role survives the refounding despite the forgery");
    }

    #[tokio::test]
    async fn memberlist_survives_a_refounding_via_the_snapshot() {
        // A silent survivor (didn't re-post at the new epoch) must stay in the memberlist
        // after a refounding — the owner's 3312 snapshot re-seeds them (CORD-02 §5).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Snapshot", bed.relays.clone(), None).await.unwrap();

        // Member joins (a Guestbook Join at epoch 0).
        let bundle = serde_json::to_string(&bundle_of(&community, Some(owner.keys.public_key()), None, None)).unwrap();
        bed.swap_to(&member);
        accept_parked_invite(&bed.relay, &bundle, None).await.unwrap();
        bed.swap_to(&owner);
        assert!(memberlist(&bed.relay, &community).await.unwrap().contains(&member.keys.public_key()), "member present pre-refound");

        // Owner refounds keeping everyone (removed = []); survivors are snapshotted to epoch 1.
        let refounded = refound_community(&bed.relay, &community, &[]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the root rolled");

        // The member is STILL a member at epoch 1 purely via the snapshot (never re-posted).
        let members = memberlist(&bed.relay, &refounded).await.unwrap();
        assert!(members.contains(&member.keys.public_key()), "a silent survivor stays a member after the refounding");
        assert!(members.contains(&owner.keys.public_key()), "owner is always a member");
    }

    #[tokio::test]
    async fn e2e_two_accounts_channels_converse_moderate() {
        use crate::community::v2::inbound::{apply_chat_to_state, persist_chat};
        use nostr_sdk::prelude::ToBech32;
        let (bed, a, b) = TestBed::new();
        let (a_npub, b_npub) = (a.keys.public_key().to_bech32().unwrap(), b.keys.public_key().to_bech32().unwrap());
        let (a_hex, b_hex) = (a.keys.public_key().to_hex(), b.keys.public_key().to_hex());
        println!("\n===== Concord v2 deep e2e =====");
        println!("[acct] A (owner)  = {a_npub}");
        println!("[acct] B (member) = {b_npub}");

        // ── A creates the community + a PRIVATE channel + two extra PUBLIC channels ──
        bed.swap_to(&a);
        let mut community = create_community(&bed.relay, "Deep E2E", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        println!("[create] community {} · #general {}", crate::simd::hex::bytes_to_hex_32(&community.id().0), crate::simd::hex::bytes_to_hex_32(&general.0));

        // A PRIVATE channel via the REAL create path: an independent key minted at
        // channel-epoch 1, delivered over the rekey plane (A is the only member yet),
        // then announced (vsk 2) — later carried to B in the join bundle.
        let priv_id = create_private_channel(&bed.relay, &community, "mods").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        let priv_ch = community.channel(&priv_id).unwrap();
        assert!(priv_ch.private && priv_ch.key.is_some() && priv_ch.epoch == Epoch(1), "born-private: keyed at epoch 1");
        println!("[channel] +private #mods {} (native create: key over the rekey plane)", crate::simd::hex::bytes_to_hex_32(&priv_id.0));

        // Two more PUBLIC channels via the real create path.
        let announcements = create_public_channel(&bed.relay, &community, "announcements").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        let random = create_public_channel(&bed.relay, &community, "random").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        println!("[channel] +public #announcements {} · #random {}", crate::simd::hex::bytes_to_hex_32(&announcements.0), crate::simd::hex::bytes_to_hex_32(&random.0));
        assert_eq!(community.channels.len(), 4, "general + mods + announcements + random");

        // A talks in a few channels.
        let m1 = send_message(&bed.relay, &community, &general, "A: welcome to the deep e2e").await.unwrap();
        send_message(&bed.relay, &community, &announcements, "A: read the rules").await.unwrap();
        send_message(&bed.relay, &community, &priv_id, "A: mods-only channel").await.unwrap();
        println!("[msg] A posted in #general / #announcements / #mods");

        // ── A grants B admin, mints a public link, B joins from the bundle ──
        let admin_rid = crate::simd::hex::bytes_to_hex_32(&[0xa1; 32]);
        publish_role(&bed.relay, &community, &a.keys, &admin_role(&admin_rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&bed.relay, &community, &a.keys, &b.keys.public_key(), vec![admin_rid], 1).await;
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
        assert!(community_is_public(&bed.relay, &community).await, "a live link makes it Public");
        println!("[invite] granted B @admin · minted link {}", link.url);

        let bundle_json = serde_json::to_string(&bundle_of(&community, Some(a.keys.public_key()), None, None)).unwrap();
        bed.swap_to(&b);
        let mut b_view = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        println!("[join] B joined; sees {} channels", b_view.channels.len());
        assert_eq!(b_view.channels.len(), 4, "B receives all four channels (incl. the private one's key) in the bundle");
        assert!(b_view.channels.iter().any(|c| c.id.0 == priv_id.0 && c.private && c.key.is_some()), "B holds the private channel key");
        assert!(texts_in(&bed.relay, &b_view, &general).await.contains(&"A: welcome to the deep e2e".to_string()), "B reads A's #general history");
        assert!(texts_in(&bed.relay, &b_view, &priv_id).await.contains(&"A: mods-only channel".to_string()), "B reads the PRIVATE channel with the bundle key");
        // B folds the control plane (persisting the roster) — the live worker does
        // this right after any join; B's admin standing gates B's channel ops below.
        let session_b = SessionGuard::capture();
        if let Some(fresh) = follow_control(&bed.relay, &b_view, &session_b).await.unwrap() {
            b_view = fresh;
        }
        println!("[follow] B folded control (roster persisted: B is @admin)");

        // ── Conversation both ways + persistence (get_messages-level) ──
        send_message(&bed.relay, &b_view, &general, "B: thanks, glad to be here").await.unwrap();
        send_message(&bed.relay, &b_view, &priv_id, "B: mods checking in").await.unwrap();
        println!("[msg] B replied in #general + #mods");
        // Persist B's own #general view into the shared store (what sync/live ingest does)
        // and confirm it reads back via STATE — get_messages parity.
        let my_pk = b.keys.public_key();
        let gh = crate::simd::hex::bytes_to_hex_32(&general.0);
        for f in fetch_channel(&bed.relay, &b_view, &general, 100).await.unwrap() {
            let outcome = { let mut st = crate::state::STATE.lock().await; apply_chat_to_state(&mut st, &f.event, &gh, &my_pk) };
            if let Some(o) = outcome { persist_chat(&gh, &o).await; }
        }
        assert!(crate::db::events::event_exists(&m1).unwrap(), "A's message persisted into B's shared store (get_messages backfill)");
        println!("[persist] #general history persisted into the shared events store");

        // B (admin) reacts to + the author edits/deletes — the chat-op surface.
        send_reaction(&bed.relay, &b_view, &general, &m1, &a_hex, super::super::kind::MESSAGE, "🔥", None).await.unwrap();
        bed.swap_to(&a);
        let m_edit = send_message(&bed.relay, &community, &general, "A: this will be edited").await.unwrap();
        send_edit(&bed.relay, &community, &general, &m_edit, "A: edited!").await.unwrap();
        let m_del = send_message(&bed.relay, &community, &general, "A: this will be deleted").await.unwrap();
        send_delete(&bed.relay, &community, &general, &m_del, super::super::kind::MESSAGE).await.unwrap();
        println!("[ops] reaction + edit + delete round-tripped");

        // ── B creates a channel as admin, A folds it in ──
        bed.swap_to(&b);
        let bugs = create_public_channel(&bed.relay, &b_view, "bug-reports").await.unwrap();
        println!("[channel] B(admin) +public #bug-reports {}", crate::simd::hex::bytes_to_hex_32(&bugs.0));
        bed.swap_to(&a);
        let session = SessionGuard::capture();
        if let Some(updated) = follow_control(&bed.relay, &community, &session).await.unwrap() {
            community = updated;
        }
        assert!(community.channels.iter().any(|c| c.id.0 == bugs.0), "A folds in B's authorized new channel");
        println!("[follow] A folded in B's #bug-reports (now {} channels)", community.channels.len());

        // ── A creates a SECOND private channel while B is already a member: B is a
        // recipient of the creation delivery, so B keys up from the rekey plane
        // (keyless record → cursor walk → blob) with no bundle involved ──
        let vault = create_private_channel(&bed.relay, &community, "vault").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        send_message(&bed.relay, &community, &vault, "A: vault is open").await.unwrap();
        println!("[channel] +private #vault {} (B is a live member — delivery via rekey plane)", crate::simd::hex::bytes_to_hex_32(&vault.0));
        bed.swap_to(&b);
        let session_b2 = SessionGuard::capture();
        if let Some(fresh) = follow_control(&bed.relay, &b_view, &session_b2).await.unwrap() {
            b_view = fresh;
        }
        let ch = b_view.channel(&vault).expect("B recorded the announced private channel");
        assert!(ch.private && ch.key.is_none() && ch.epoch == Epoch(0), "B's record is keyless at cursor 0");
        let rf = follow_rekeys(&bed.relay, &b_view, &session_b2).await.unwrap();
        b_view = rf.updated.expect("the rekey walk adopts the creation delivery");
        let ch = b_view.channel(&vault).expect("still recorded");
        assert!(ch.key.is_some() && ch.epoch == Epoch(1), "B adopted the epoch-1 key from the creation crate");
        assert!(
            texts_in(&bed.relay, &b_view, &vault).await.contains(&"A: vault is open".to_string()),
            "B reads the private history with the ADOPTED key"
        );
        send_message(&bed.relay, &b_view, &vault, "B: in the vault").await.unwrap();
        bed.swap_to(&a);
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(
            texts_in(&bed.relay, &community, &vault).await.contains(&"B: in the vault".to_string()),
            "A reads B's reply on the natively-created private channel"
        );
        println!("[private] B adopted #vault via rekey plane; two-way private conversation verified");

        // ── Members ──
        let members = memberlist(&bed.relay, &community).await.unwrap();
        let member_hexes: std::collections::BTreeSet<String> = members.iter().map(|m| m.to_hex()).collect();
        assert!(member_hexes.contains(&a_hex) && member_hexes.contains(&b_hex), "A + B both in the memberlist");
        println!("[members] {} members: A + B present", members.len());

        // ── Moderate: ban B (banlist + strip + refound), verify severance + survival ──
        set_banlist(&bed.relay, &community, &[b_hex.clone()]).await.unwrap();
        grant_roles(&bed.relay, &community, &b.keys.public_key(), vec![]).await.unwrap();
        let refounded = refound_community(&bed.relay, &community, &[b.keys.public_key()]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the ban rolled the root");
        let post = fold_authority(&refounded, &fetch_control(&bed.relay, &refounded).await, &load_floors(&refounded));
        assert!(post.banned.contains(&b_hex), "the ban survives the refounding");
        assert!(texts_in(&bed.relay, &refounded, &general).await.iter().any(|t| t == "A: welcome to the deep e2e"), "pre-ban history reads across the new epoch");
        assert!(
            texts_in(&bed.relay, &refounded, &priv_id).await.iter().any(|t| t == "A: mods-only channel"),
            "PRIVATE history reads across the channel's own rotation (per-channel multi-epoch archive)"
        );
        println!("[ban] B banned; root rolled to epoch 1; ban survives; pre-ban history intact (public + private)");
        // B concludes it's severed.
        bed.swap_to(&b);
        let session_b3 = SessionGuard::capture();
        assert!(follow_rekeys(&bed.relay, &b_view, &session_b3).await.unwrap().self_removed, "B is cryptographically cut by the ban-refound");
        println!("[ban] B's rekey-follow: self_removed = true (severed)");

        // ── Unban: A lifts the ban ──
        bed.swap_to(&a);
        set_banlist(&bed.relay, &refounded, &[]).await.unwrap();
        let after_unban = fold_authority(&refounded, &fetch_control(&bed.relay, &refounded).await, &load_floors(&refounded));
        assert!(!after_unban.banned.contains(&b_hex), "the unban clears B from the banlist");
        println!("[unban] B removed from the banlist (re-invitable)");

        // ── Dissolve ──
        dissolve_community(&bed.relay, &refounded).await.unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().unwrap().dissolved, "the community is sealed");
        println!("[dissolve] community sealed (read-only)\n===== e2e PASS =====\n");
    }

    /// The same scenario on a REAL relay with TWO throwaway accounts, off by default. It
    /// LOGS both nsecs (+ every id) so you can inspect the run and RE-RUN against the same
    /// accounts by exporting `VECTOR_E2E_NSEC_A` / `_B`. Set `VECTOR_E2E_LOG=<path>` to also
    /// append the transcript to a file, `VECTOR_E2E_RELAY=<url>` to pick the relay.
    ///   cargo test -p vector-core -- --ignored --nocapture live_e2e_two_accounts
    #[tokio::test]
    #[ignore]
    async fn live_e2e_two_accounts() {
        use crate::community::transport::LiveTransport;
        use nostr_sdk::prelude::{ClientBuilder, RelayOptions, ToBech32};

        let relay = std::env::var("VECTOR_E2E_RELAY").unwrap_or_else(|_| "wss://jskitty.com/nostr".to_string());
        let relays = vec![relay.clone()];
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());

        // Throwaway (or bring-your-own via env for a re-run against the same accounts).
        let a = std::env::var("VECTOR_E2E_NSEC_A").ok().and_then(|n| Keys::parse(&n).ok()).unwrap_or_else(Keys::generate);
        let b = std::env::var("VECTOR_E2E_NSEC_B").ok().and_then(|n| Keys::parse(&n).ok()).unwrap_or_else(Keys::generate);

        let log = |line: String| {
            println!("{line}");
            if let Ok(p) = std::env::var("VECTOR_E2E_LOG") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
                    let _ = writeln!(f, "{line}");
                }
            }
        };
        log(format!("===== LIVE Concord v2 e2e on {relay} ====="));
        log(format!("VECTOR_E2E_NSEC_A={}  ({})", a.secret_key().to_bech32().unwrap(), a.public_key().to_bech32().unwrap()));
        log(format!("VECTOR_E2E_NSEC_B={}  ({})", b.secret_key().to_bech32().unwrap(), b.public_key().to_bech32().unwrap()));

        for k in [&a, &b] {
            let npub = k.public_key().to_bech32().unwrap();
            std::fs::create_dir_all(tmp.path().join(&npub)).unwrap();
            crate::db::set_current_account(npub.clone()).unwrap();
            crate::db::init_database(&npub).unwrap();
        }
        // One relay connection: a v2 wrap is pre-signed (ephemeral p-key) and its seal is
        // signed by MY_SECRET_KEY, so publishing needs no per-account client signer.
        let client = ClientBuilder::new().signer(a.clone()).build();
        client.pool().add_relay(relay.as_str(), RelayOptions::default()).await.ok();
        client.connect().await;
        crate::state::set_nostr_client(client);
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(15));
        let become_acct = |k: &Keys| {
            let npub = k.public_key().to_bech32().unwrap();
            crate::db::set_current_account(npub.clone()).unwrap();
            crate::db::init_database(&npub).unwrap();
            crate::db::clear_id_caches();
            crate::state::MY_SECRET_KEY.store_from_keys(k, &[]);
            crate::state::set_my_public_key(k.public_key());
        };
        let settle = || tokio::time::sleep(std::time::Duration::from_secs(2));

        // A: create + a channel + grant B admin + mint link.
        become_acct(&a);
        let mut community = create_community(&transport, "Live E2E", relays.clone(), None).await.expect("create");
        let general = community.channels[0].id;
        log(format!("[create] community {} · #general {}", crate::simd::hex::bytes_to_hex_32(&community.id().0), crate::simd::hex::bytes_to_hex_32(&general.0)));
        send_message(&transport, &community, &general, "A: live hello").await.expect("send");
        let ann = create_public_channel(&transport, &community, "announcements").await.expect("channel");
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        log(format!("[channel] +public #announcements {}", crate::simd::hex::bytes_to_hex_32(&ann.0)));
        grant_admin(&transport, &community, &b.public_key()).await.expect("grant admin");
        let link = mint_public_link(&transport, &community, "https://vectorapp.io", None, None).await.expect("mint");
        log(format!("[invite] B granted @admin · link {}", link.url));
        let bundle_json = serde_json::to_string(&bundle_of(&community, Some(a.public_key()), None, None)).unwrap();
        settle().await;

        // B: join + read A's history + reply.
        become_acct(&b);
        let b_view = accept_parked_invite(&transport, &bundle_json, None).await.expect("join");
        log(format!("[join] B joined; {} channels", b_view.channels.len()));
        settle().await;
        let seen = texts_in(&transport, &b_view, &general).await;
        log(format!("[read] B sees #general: {seen:?}"));
        assert!(seen.iter().any(|t| t == "A: live hello"), "B reads A's message over the real relay");
        send_message(&transport, &b_view, &general, "B: live reply").await.expect("reply");
        settle().await;

        // A: create a PRIVATE channel while B is already a member — B is a recipient
        // of the creation delivery, so B keys up from the rekey plane over the real
        // relay (no bundle involved), then the two converse on it.
        become_acct(&a);
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        let vault = create_private_channel(&transport, &community, "vault").await.expect("private channel");
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        send_message(&transport, &community, &vault, "A: vault live").await.expect("vault send");
        log(format!("[channel] +private #vault {} (key delivered over the rekey plane)", crate::simd::hex::bytes_to_hex_32(&vault.0)));
        settle().await;

        become_acct(&b);
        let session_b = SessionGuard::capture();
        let mut b_view = crate::db::community::load_community_v2(b_view.id()).unwrap().unwrap();
        if let Some(fresh) = follow_control(&transport, &b_view, &session_b).await.expect("B control follow") {
            b_view = fresh;
        }
        if let Some(fresh) = follow_rekeys(&transport, &b_view, &session_b).await.expect("B rekey follow").updated {
            b_view = fresh;
        }
        let vch = b_view.channel(&vault).expect("B folded the vault");
        assert!(vch.key.is_some() && vch.epoch == Epoch(1), "B adopted the vault key from the live rekey plane");
        let vseen = texts_in(&transport, &b_view, &vault).await;
        log(format!("[read] B sees #vault: {vseen:?}"));
        assert!(vseen.iter().any(|t| t == "A: vault live"), "B reads the private channel with the ADOPTED key");
        send_message(&transport, &b_view, &vault, "B: in the live vault").await.expect("vault reply");
        settle().await;

        become_acct(&a);
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(
            texts_in(&transport, &community, &vault).await.iter().any(|t| t == "B: in the live vault"),
            "A reads B's private reply"
        );
        log("[private] two-way #vault conversation over the live relay".to_string());

        // A: ban B (three-removal) + dissolve.
        set_banlist(&transport, &community, &[b.public_key().to_hex()]).await.expect("banlist");
        grant_roles(&transport, &community, &b.public_key(), vec![]).await.expect("strip");
        let refounded = refound_community(&transport, &community, &[b.public_key()]).await.expect("refound");
        log(format!("[ban] B banned; root → epoch {}", refounded.root_epoch.0));
        settle().await;
        dissolve_community(&transport, &refounded).await.expect("dissolve");
        log("[dissolve] community sealed".to_string());
        log("===== LIVE e2e PASS =====".to_string());
    }

    #[tokio::test]
    async fn an_offline_member_learns_of_a_dissolution_on_catch_up() {
        // The tombstone rides its own public plane, watched live — an OFFLINE
        // member's catch-up must fetch it too, or they follow (and post into) a
        // grave forever.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Doomed", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();

        // The owner dissolves while the member sleeps.
        bed.swap_to(&owner);
        dissolve_community(&bed.relay, &community).await.unwrap();

        // The member's catch-up learns of the death, seals, and refuses to post.
        bed.swap_to(&member);
        let session = SessionGuard::capture();
        let follow = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(follow.dissolved, "the catch-up surfaces the tombstone");
        assert!(!follow.self_removed && follow.updated.is_none());
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&joined.id().0);
        assert!(crate::db::community::get_community_dissolved(&cid_hex).unwrap(), "sealed read-only locally");
        let err = send_message(&bed.relay, &joined, &general, "into the void").await.unwrap_err();
        assert!(err.contains("dissolved"), "sends refuse a grave: {err}");
        // Subsequent follows take the local fast path — still dissolved, no churn.
        let again = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(again.dissolved && again.updated.is_none());
    }

    #[tokio::test]
    async fn a_wide_community_survives_refoundings_and_an_offline_member_converges() {
        // Scale stress: MANY private channels, each rotated on every Refounding.
        // A member offline across two refoundings must converge on all of them
        // (the per-channel rotation fan in refound + the follow's channel×root×step
        // loops stay bounded) with every channel's history readable.
        const PRIV_CHANNELS: usize = 6;
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let mut community = create_community(&bed.relay, "Wide", bed.relays.clone(), None).await.unwrap();
        let mut priv_ids = Vec::new();
        for i in 0..PRIV_CHANNELS {
            let id = create_private_channel(&bed.relay, &community, &format!("priv{i}")).await.unwrap();
            community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
            send_message(&bed.relay, &community, &id, &format!("priv{i} epoch0")).await.unwrap();
            priv_ids.push(id);
        }
        let bundle_json = serde_json::to_string(&bundle_of(&community, Some(owner.keys.public_key()), None, None)).unwrap();

        // Member joins at epoch 0 with all channel keys, then goes offline.
        bed.swap_to(&member);
        let member_view = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert_eq!(member_view.channels.iter().filter(|c| c.private && c.key.is_some()).count(), PRIV_CHANNELS, "joined with all private keys");

        // Two refoundings (each rotates the base + every private channel).
        bed.swap_to(&owner);
        for epoch in 1..=2u64 {
            community = refound_community(&bed.relay, &community, &[]).await.unwrap();
            assert_eq!(community.root_epoch, Epoch(epoch));
            for id in &priv_ids {
                send_message(&bed.relay, &community, id, &format!("{} epoch{epoch}", crate::simd::hex::bytes_to_hex_32(&id.0))).await.unwrap();
            }
        }

        // Member returns: bounded follow to quiescence.
        bed.swap_to(&member);
        let session = SessionGuard::capture();
        let mut passes = 0;
        loop {
            passes += 1;
            assert!(passes <= 8, "a wide catch-up must converge, not churn (pass {passes})");
            let cur = crate::db::community::load_community_v2(member_view.id()).unwrap().unwrap();
            let rk = follow_rekeys(&bed.relay, &cur, &session).await.unwrap();
            assert!(!rk.self_removed);
            let cur = crate::db::community::load_community_v2(member_view.id()).unwrap().unwrap();
            let ctl = follow_control(&bed.relay, &cur, &session).await.unwrap();
            if rk.updated.is_none() && ctl.is_none() {
                break;
            }
        }
        let caught_up = crate::db::community::load_community_v2(member_view.id()).unwrap().unwrap();
        assert_eq!(caught_up.root_epoch, Epoch(2), "walked both refoundings");
        // Every private channel converged to the owner's current key + reads all epochs.
        for id in &priv_ids {
            let mine = caught_up.channel(id).expect("channel survived");
            let theirs = community.channel(id).unwrap();
            assert_eq!(mine.key, theirs.key, "channel {} converged on the owner key", crate::simd::hex::bytes_to_hex_32(&id.0));
            assert_eq!(mine.epoch, theirs.epoch, "…at the same epoch");
            let texts = texts_in(&bed.relay, &caught_up, id).await;
            let id_hex = crate::simd::hex::bytes_to_hex_32(&id.0);
            assert!(texts.iter().any(|t| t.contains("epoch0")), "channel {id_hex} reads epoch-0 history");
            for epoch in 1..=2u64 {
                assert!(texts.iter().any(|t| t.contains(&format!("epoch{epoch}"))), "channel {id_hex} reads epoch-{epoch} history");
            }
        }
    }

    #[tokio::test]
    async fn an_offline_member_catches_up_across_three_refoundings() {
        // The deep offline-online scenario: a member sleeps through THREE
        // Refoundings, per-refound private-channel rotations, a mid-life private
        // channel CREATED while they slept, a public channel, a rename, and a
        // ban — then returns and converges by follow alone (no rejoin).
        use nostr_sdk::prelude::ToBech32;
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let mut community = create_community(&bed.relay, "Sleeper", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        let mods = create_private_channel(&bed.relay, &community, "mods").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        send_message(&bed.relay, &community, &general, "epoch0: hello").await.unwrap();
        send_message(&bed.relay, &community, &mods, "epoch0: mods secret").await.unwrap();
        let bundle_json = serde_json::to_string(&bundle_of(&community, Some(owner.keys.public_key()), None, None)).unwrap();

        // Member joins at epoch 0, then goes OFFLINE.
        bed.swap_to(&member);
        let member_view = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert_eq!(member_view.root_epoch, Epoch(0));

        // While they sleep, the owner reshapes everything across three epochs.
        bed.swap_to(&owner);
        let stranger = Keys::generate();
        for epoch in 1..=3u64 {
            community = refound_community(&bed.relay, &community, &[]).await.unwrap();
            assert_eq!(community.root_epoch, Epoch(epoch));
            send_message(&bed.relay, &community, &general, &format!("epoch{epoch}: general news")).await.unwrap();
            send_message(&bed.relay, &community, &mods, &format!("epoch{epoch}: mods word")).await.unwrap();
        }
        let news = create_public_channel(&bed.relay, &community, "news").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        let vault = create_private_channel(&bed.relay, &community, "vault").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        send_message(&bed.relay, &community, &vault, "epoch3: vault opened").await.unwrap();
        set_banlist(&bed.relay, &community, &[stranger.public_key().to_hex()]).await.unwrap();
        let meta = control::CommunityMetadata { name: "Sleeper Reborn".into(), relays: community.relays.clone(), ..Default::default() };
        edit_community_metadata(&bed.relay, &community, &meta).await.unwrap();

        // The member RETURNS: rekey+control follow to quiescence (the worker's
        // loop, driven explicitly). Bounded — convergence must be fast.
        bed.swap_to(&member);
        let session = SessionGuard::capture();
        let mut passes = 0;
        loop {
            passes += 1;
            assert!(passes <= 6, "catch-up must converge, not churn");
            let cur = crate::db::community::load_community_v2(member_view.id()).unwrap().unwrap();
            let rekeyed = follow_rekeys(&bed.relay, &cur, &session).await.unwrap();
            assert!(!rekeyed.self_removed, "the member was never removed");
            let cur = crate::db::community::load_community_v2(member_view.id()).unwrap().unwrap();
            let controlled = follow_control(&bed.relay, &cur, &session).await.unwrap();
            if rekeyed.updated.is_none() && controlled.is_none() {
                break;
            }
        }
        let caught_up = crate::db::community::load_community_v2(member_view.id()).unwrap().unwrap();

        // Base + name converged.
        assert_eq!(caught_up.root_epoch, Epoch(3), "walked all three refoundings");
        assert_eq!(caught_up.community_root, community.community_root, "landed on the owner's root");
        assert_eq!(caught_up.name, "Sleeper Reborn");
        // Channels: renamed set incl. the mid-sleep public + private ones.
        assert!(caught_up.channels.iter().any(|c| c.id.0 == news.0), "folded the new public channel");
        let m = caught_up.channel(&mods).expect("mods survived");
        let owner_mods = community.channel(&mods).unwrap();
        assert_eq!(m.epoch, owner_mods.epoch, "mods walked every per-refound rotation");
        assert_eq!(m.key, owner_mods.key, "…to the owner's exact key");
        let v = caught_up.channel(&vault).expect("vault folded in");
        assert_eq!(v.key, community.channel(&vault).unwrap().key, "adopted the mid-sleep private channel's key");
        // Banlist survived the compactions.
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&caught_up.id().0);
        let banned = crate::db::community::get_community_banlist(&cid_hex).unwrap();
        assert!(banned.contains(&stranger.public_key().to_hex()), "the ban folded through");
        // History reads across EVERY epoch (public via base-root archive, private
        // via the per-channel archive built during the walk).
        let gen_texts = texts_in(&bed.relay, &caught_up, &general).await;
        for epoch in 0..=3u64 {
            let needle = if epoch == 0 { "epoch0: hello".to_string() } else { format!("epoch{epoch}: general news") };
            assert!(gen_texts.contains(&needle), "general history spans epoch {epoch}: {gen_texts:?}");
        }
        let mods_texts = texts_in(&bed.relay, &caught_up, &mods).await;
        for epoch in 0..=3u64 {
            let needle = if epoch == 0 { "epoch0: mods secret".to_string() } else { format!("epoch{epoch}: mods word") };
            assert!(mods_texts.contains(&needle), "private history spans epoch {epoch}: {mods_texts:?}");
        }
        assert!(texts_in(&bed.relay, &caught_up, &vault).await.contains(&"epoch3: vault opened".to_string()));
        // And the member can still speak.
        send_message(&bed.relay, &caught_up, &general, "member: good morning").await.unwrap();
        bed.swap_to(&owner);
        assert!(
            texts_in(&bed.relay, &community, &general).await.contains(&"member: good morning".to_string()),
            "the caught-up member converses at the new epoch ({})",
            member.keys.public_key().to_bech32().unwrap()
        );
    }

    /// Seal `n` messages onto a community's #general, one per second starting at
    /// `base_secs` (distinct wrap seconds so relay-side `until` paging engages).
    async fn flood_general(relay: &MemoryRelay, community: &CommunityV2, author: &Keys, n: usize, base_secs: u64) {
        let general = community.channels[0].id;
        let group = channel_group_key(&community.community_root, &general, community.root_epoch);
        for i in 0..n {
            let at = base_secs + i as u64;
            let rumor = chat::build_message_rumor(author.public_key(), &general, community.root_epoch, &format!("msg {i}"), None, &[], vec![], at * 1000);
            let (wrap, _) = chat::seal_chat_rumor(&rumor, &group, author, Timestamp::from_secs(at), false).unwrap();
            relay.publish(&wrap, &community.relays).await.unwrap();
        }
    }

    #[tokio::test]
    async fn the_history_walk_pages_past_a_multi_page_burst() {
        // A bot offline through 120 messages must catch ALL of them, not the
        // newest page — the v1 sync-gap class, closed by until-paging.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Burst", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        flood_general(&relay, &community, &owner, 120, 10_000).await;

        let all = fetch_channel_history(&relay, &community, &general, 50, 8, |_| true).await.unwrap();
        assert_eq!(all.len(), 120, "the walk pages the whole burst");
        // Oldest→newest, no duplicates.
        let contents: Vec<String> = all.iter().map(|f| f.event.opened().rumor.content.clone()).collect();
        assert_eq!(contents.first().map(String::as_str), Some("msg 0"));
        assert_eq!(contents.last().map(String::as_str), Some("msg 119"));
        let unique: std::collections::HashSet<&String> = contents.iter().collect();
        assert_eq!(unique.len(), 120, "wrap-id + rumor-id dedup holds across page boundaries");

        // The single-page fetch stays a single page.
        let one = fetch_channel(&relay, &community, &general, 50).await.unwrap();
        assert_eq!(one.len(), 50, "fetch_channel is one newest page");
        assert_eq!(one.last().map(|f| f.event.opened().rumor.content.clone()).as_deref(), Some("msg 119"));
    }

    #[tokio::test]
    async fn the_history_walk_stops_when_the_caller_is_caught_up() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Caught", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        flood_general(&relay, &community, &owner, 120, 10_000).await;

        // The caller says "I hold everything" after the first page — no deeper fetch.
        let mut pages = 0usize;
        let got = fetch_channel_history(&relay, &community, &general, 50, 8, |_| {
            pages += 1;
            false
        })
        .await
        .unwrap();
        assert_eq!(pages, 1, "the early stop is consulted once");
        assert_eq!(got.len(), 50, "only the newest page is fetched");
        assert_eq!(got.last().map(|f| f.event.opened().rumor.content.clone()).as_deref(), Some("msg 119"));
    }

    #[tokio::test]
    async fn a_same_second_history_wall_terminates_instead_of_looping() {
        // 60 messages in ONE second with a 25-wrap page: a second-granular
        // `until` can never page past the wall — the walk must step over it
        // (bounded loss, logged) rather than spin.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Wall", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let group = channel_group_key(&community.community_root, &general, community.root_epoch);
        for i in 0..60usize {
            let rumor = chat::build_message_rumor(owner.public_key(), &general, community.root_epoch, &format!("burst {i}"), None, &[], vec![], 5_000_000 + i as u64);
            let (wrap, _) = chat::seal_chat_rumor(&rumor, &group, &owner, Timestamp::from_secs(5_000), false).unwrap();
            relay.publish(&wrap, &community.relays).await.unwrap();
        }
        let got = fetch_channel_history(&relay, &community, &general, 25, 8, |_| true).await.unwrap();
        assert!(got.len() >= 25, "at least the relay page is read");
        assert!(got.len() <= 60, "sane bound");
        // Termination is the assertion: reaching here means the wall didn't loop.
    }

    #[tokio::test]
    async fn a_grant_revoke_survives_a_withholding_relay() {
        // Floor persistence on the delegation plane: after the owner revokes an admin,
        // a relay serving only the OLD (still owner-signed) grant can't resurrect it.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Revoke", vec!["wss://good".into()], None).await.unwrap();
        let admin = Keys::generate();
        let rid = "d4".repeat(32);
        publish_role(&relay, &community, &owner, &admin_role(&rid, Permissions::MANAGE_METADATA), 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![rid.clone()], 1).await;
        let session = SessionGuard::capture();
        follow_control(&relay, &community, &session).await.unwrap(); // seed floors incl. the grant at v1
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![], 2).await; // revoke → grant floor v2
        follow_control(&relay, &community, &session).await.unwrap();

        // A stale relay serves only the grant prefix (v1, the live grant).
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await;
        let mut stale = community.clone();
        stale.relays = vec!["wss://stale".into()];
        let floors = load_floors(&community);
        let editions = fetch_control(&relay, &stale).await;
        let authority = fold_authority(&stale, &editions, &floors);
        assert!(
            !authority.roles.is_authorized(&admin.public_key().to_hex(), Some(&owner.public_key().to_hex()), Permissions::MANAGE_METADATA),
            "the persisted grant floor refuses the rolled-back (re-granted) view"
        );
    }

    /// Load the current-epoch floors for a community (test mirror of follow_control).
    fn load_floors(community: &CommunityV2) -> Floors {
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::get_all_edition_heads_full(&cid_hex)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, f)| f.0 == community.root_epoch.0)
            .map(|(e, f)| (e, (f.1, f.2, f.3)))
            .collect()
    }

    /// Fetch + open every control edition at a community's control plane (test helper).
    async fn fetch_control(relay: &MemoryRelay, community: &CommunityV2) -> Vec<ParsedEdition> {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let q = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
        relay
            .fetch(&q, &community.relays)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|w| control::open_control_edition(w, &group).ok().map(|(ed, _)| ed))
            .collect()
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

    /// Re-inject only the OLD prefix (every edition at/below `max_version`) of a
    /// community's control plane onto a second relay URL — the withholding-relay
    /// simulation: everything it serves is genuinely owner-signed, just stale.
    async fn inject_stale_prefix(relay: &MemoryRelay, community: &CommunityV2, max_version: u64, stale_relay: &str) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let query = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
        let wraps = relay.fetch(&query, &community.relays).await.unwrap();
        for w in &wraps {
            if let Ok((ed, _)) = control::open_control_edition(w, &group) {
                if ed.version <= max_version {
                    relay.inject(w, &[stale_relay.to_string()]);
                }
            }
        }
    }

    #[tokio::test]
    async fn a_withholding_relay_cannot_roll_back_a_rename() {
        // W2 persisted floor: after adopting the owner's v2 rename, a relay serving
        // only the (owner-signed) v1 genesis must not revert the held name.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Original", vec!["wss://good".into()], None).await.unwrap();
        publish_community_meta(&relay, &community, &owner, "Renamed", 2).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("rename adopted");
        assert_eq!(updated.name, "Renamed");

        // The stale relay holds only the genesis prefix; point the follow at it.
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await;
        let mut stale_view = updated.clone();
        stale_view.relays = vec!["wss://stale".into()];
        assert!(
            follow_control(&relay, &stale_view, &session).await.unwrap().is_none(),
            "a stale-only relay must not change the held view"
        );
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert_eq!(held.name, "Renamed", "the persisted floor refuses the rollback");
    }

    #[tokio::test]
    async fn a_withholding_relay_cannot_resurrect_a_deleted_channel() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prune2", vec!["wss://good".into()], None).await.unwrap();
        let extra = ChannelId([0x44; 32]);
        let session = SessionGuard::capture();

        // A same-content metadata edit: no visible change (None), but the floor must
        // still advance to v2 (so the genesis metadata can't re-present below).
        publish_community_meta(&relay, &community, &owner, "Prune2", 2).await;
        assert!(follow_control(&relay, &community, &session).await.unwrap().is_none());

        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 1, false).await;
        let with_extra = follow_control(&relay, &community, &session).await.unwrap().expect("added");
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 2, true).await;
        let pruned = follow_control(&relay, &with_extra, &session).await.unwrap().expect("removed");
        assert!(pruned.channel(&extra).is_none());

        // The stale relay serves the add (v1) but withholds the delete (v2).
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await;
        let mut stale_view = pruned.clone();
        stale_view.relays = vec!["wss://stale".into()];
        assert!(
            follow_control(&relay, &stale_view, &session).await.unwrap().is_none(),
            "the withheld delete must not resurrect the channel"
        );
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(held.channel(&extra).is_none(), "the deleted channel stays deleted");
    }

    #[tokio::test]
    async fn a_new_epoch_bootstraps_past_an_old_epoch_floor() {
        // The Armada-convergence carve-out: a Refounding compacts the chain and
        // re-wraps a detached head at the NEW epoch's control plane. The old epoch's
        // floor must not block it — epoch-filtering makes the entity bootstrap.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Before", vec!["wss://good".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        publish_community_meta(&relay, &community, &owner, "Edited", 2).await;
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("edit adopted");
        assert_eq!(updated.name, "Edited");

        // Refounding lands (epoch bump saved by the rekey path); the compacted head
        // arrives DETACHED (high version, no prev) on the new epoch's plane.
        let mut refounded = updated.clone();
        refounded.root_epoch = crate::community::Epoch(1);
        crate::db::community::save_community_v2(&refounded).unwrap();
        publish_community_meta(&relay, &refounded, &owner, "Compacted", 5).await;

        let adopted = follow_control(&relay, &refounded, &session).await.unwrap().expect("compacted head adopted");
        assert_eq!(adopted.name, "Compacted", "a fresh epoch bootstraps despite the dangling prev");
        // The persisted floor is stamped with the epoch the FOLD ran under.
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let heads = crate::db::community::get_all_edition_heads_epoched(&cid_hex).unwrap();
        assert!(
            heads.get(&cid_hex).is_some_and(|(e, v, _)| *e == 1 && *v == 5),
            "the adopted head carries the fold's epoch + version"
        );
    }

    #[tokio::test]
    async fn a_same_version_owner_fork_at_the_floor_converges_to_the_deterministic_winner() {
        // Two owner-signed editions at the SAME version (publish retry / two owner
        // devices): every client must land on the lower-inner-id winner. A hash-strict
        // floor would wedge here forever while Armada converges — the floor must
        // CONVERGE instead (the v1 decide() rule).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Fork", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let genesis_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        publish_community_meta(&relay, &community, &owner, "Ours", 2).await;
        let ours = follow_control(&relay, &community, &session).await.unwrap().expect("ours adopted");
        assert_eq!(ours.name, "Ours");

        // Our committed v2 edition's tiebreak id.
        let our_inner = {
            let q = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
            let wraps = relay.fetch(&q, &community.relays).await.unwrap();
            wraps
                .iter()
                .find_map(|w| {
                    control::open_control_edition(w, &group)
                        .ok()
                        .filter(|(ed, _)| ed.version == 2 && ed.vsk == vsk::COMMUNITY_METADATA)
                        .map(|(ed, _)| ed.inner_id)
                })
                .unwrap()
        };

        // Craft the concurrent fork so it WINS the deterministic tiebreak (vary the
        // authored timestamp until its inner id is lower).
        let meta = control::CommunityMetadata { name: "Theirs".into(), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let mut ts = 2_000u64;
        let fork_wrap = loop {
            let rumor = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 2, Some(&genesis_hash), &content, ts, None);
            let inner = rumor.id.unwrap().to_bytes();
            if inner < our_inner {
                break control::seal_control_edition(&rumor, &group, &owner, Timestamp::from_secs(ts)).unwrap().0;
            }
            ts += 1;
        };
        relay.publish(&fork_wrap, &community.relays).await.unwrap();

        let converged = follow_control(&relay, &ours, &session).await.unwrap().expect("fork winner adopted");
        assert_eq!(converged.name, "Theirs", "the floor converges to the lower-inner-id winner");
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let held = crate::db::community::get_edition_head_inner_id(&cid_hex, &cid_hex).unwrap();
        assert!(held.is_some_and(|h| h < our_inner), "the persisted floor's tiebreak key moved to the winner");
    }

    #[tokio::test]
    async fn an_anchored_prefix_applies_while_a_gap_above_awaits_the_missing_link() {
        // v2 chains to the floor; v4 arrives but its v3 link is withheld. The
        // chain-verified prefix (v2) applies NOW — refuse-downgrade holds for it —
        // while the detached v4 waits. When v3 lands, the chain heals to v4.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prefix", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);

        publish_community_meta(&relay, &community, &owner, "Two", 2).await;
        let v2_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        // Craft v3 (held back) and v4 (published, chained to the withheld v3).
        let c3 = serde_json::to_string(&control::CommunityMetadata { name: "Three".into(), ..Default::default() }).unwrap();
        let r3 = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 3, Some(&v2_hash), &c3, 3_000, None);
        let (w3, _) = control::seal_control_edition(&r3, &group, &owner, Timestamp::from_secs(3_000)).unwrap();
        let (ed3, _) = control::open_control_edition(&w3, &group).unwrap();
        let c4 = serde_json::to_string(&control::CommunityMetadata { name: "Four".into(), ..Default::default() }).unwrap();
        let r4 = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 4, Some(&ed3.self_hash), &c4, 4_000, None);
        let (w4, _) = control::seal_control_edition(&r4, &group, &owner, Timestamp::from_secs(4_000)).unwrap();
        relay.publish(&w4, &community.relays).await.unwrap();

        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("the verified prefix applies");
        assert_eq!(updated.name, "Two", "the anchored prefix lands; the detached v4 does not");

        relay.publish(&w3, &community.relays).await.unwrap();
        let healed = follow_control(&relay, &updated, &session).await.unwrap().expect("the chain heals");
        assert_eq!(healed.name, "Four", "once the link arrives, the head advances past the prefix");
    }

    #[tokio::test]
    async fn paging_rescues_a_floor_link_evicted_from_the_newest_window() {
        // The held floor is v2; the owner publishes v3, then a flood of foreign junk
        // wraps fills the newest window, then v4. Page 1 sees only v4 (detached →
        // gapped); paging older must recover v3 (and the floor link) and heal to v4.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Paged", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);

        publish_community_meta(&relay, &community, &owner, "Two", 2).await;
        let base = follow_control(&relay, &community, &session).await.unwrap().expect("floor at v2");
        publish_community_meta(&relay, &base, &owner, "Three", 3).await; // ts 1_000 (old)
        let v3_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        // Rogue flood occupying the newest window (sealed to the control plane, but
        // non-owner — the authority gate drops them; they only crowd the page).
        let rogue = Keys::generate();
        for i in 0..(FOLLOW_PAGE as u64 - 1) {
            let rumor = control::build_edition_rumor(rogue.public_key(), vsk::CHANNEL_METADATA, &[0xCC; 32], 1, None, "{\"name\":\"junk\",\"private\":false}", 4_000 + i, None);
            let (w, _) = control::seal_control_edition(&rumor, &group, &rogue, Timestamp::from_secs(4_000 + i)).unwrap();
            relay.publish(&w, &community.relays).await.unwrap();
        }
        // v4 chained to the real v3 (crafted directly: the flood also blinds the
        // helper's own newest-window head lookup), timestamped newest of all.
        let c4 = serde_json::to_string(&control::CommunityMetadata { name: "Four".into(), ..Default::default() }).unwrap();
        let r4 = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 4, Some(&v3_hash), &c4, 10_000, None);
        let (w4, _) = control::seal_control_edition(&r4, &group, &owner, Timestamp::from_secs(10_000)).unwrap();
        relay.publish(&w4, &community.relays).await.unwrap();

        let healed = follow_control(&relay, &base, &session).await.unwrap().expect("paging recovered the chain");
        assert_eq!(healed.name, "Four", "the gap paged past the flood to the floor link");
    }

    #[tokio::test]
    async fn a_follow_after_delete_does_not_resurrect_the_community() {
        // A leave/delete racing an in-flight follow: the follow must not re-insert
        // the community row or floor rows past delete_community's wipe.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Gone", vec!["wss://r".into()], None).await.unwrap();
        publish_community_meta(&relay, &community, &owner, "Edited", 2).await;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::delete_community(&cid_hex).unwrap();

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a follow racing a delete is a no-op"
        );
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_none(), "the community stays deleted");
        assert!(crate::db::community::edition_head_entity_ids(&cid_hex).unwrap().is_empty(), "no orphan floor rows");
    }

    #[tokio::test]
    async fn a_rekey_follow_after_delete_does_not_resurrect_the_community() {
        // The rekey sibling of the follow_control guard: an owner rotation adopted
        // mid-race must not upsert the community row back after a leave/delete.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "GoneKeys", vec!["wss://r".into()], None).await.unwrap();
        let new_root = [0xB2; 32];
        publish_base_rotation(&relay, &community, &owner, &[owner.public_key()], &new_root, &community.community_root).await;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::delete_community(&cid_hex).unwrap();

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "a rekey follow racing a delete adopts nothing");
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_none(), "the community stays deleted");
    }

    #[tokio::test]
    async fn a_joiner_bootstraps_the_highest_head_across_a_lost_middle_edition() {
        // {v1, v3} on the relays with v2 lost at publish time (a rate-limiting relay
        // that still ACKed): the genesis anchors, so an anchored-prefix-first fold
        // would take v1 and SEED the joiner's floor there — pinning them below the
        // head Armada shows, forever. A joiner (floor 0) must bootstrap v3.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Skip", bed.relays.clone(), None).await.unwrap();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let genesis_hash = head_hash_on_relay(&bed.relay, &community, &community.id().0).await.unwrap();

        // v2 is crafted but NEVER published; v3 chains to it and is published.
        let c2 = serde_json::to_string(&control::CommunityMetadata { name: "Two".into(), ..Default::default() }).unwrap();
        let r2 = control::build_edition_rumor(owner.keys.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 2, Some(&genesis_hash), &c2, 2_000, None);
        let (w2, _) = control::seal_control_edition(&r2, &group, &owner.keys, Timestamp::from_secs(2_000)).unwrap();
        let (ed2, _) = control::open_control_edition(&w2, &group).unwrap();
        let c3 = serde_json::to_string(&control::CommunityMetadata { name: "Three".into(), ..Default::default() }).unwrap();
        let r3 = control::build_edition_rumor(owner.keys.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 3, Some(&ed2.self_hash), &c3, 3_000, None);
        let (w3, _) = control::seal_control_edition(&r3, &group, &owner.keys, Timestamp::from_secs(3_000)).unwrap();
        bed.relay.publish(&w3, &community.relays).await.unwrap();

        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert_eq!(joined.name, "Three", "the joiner bootstraps the highest signed head, not the anchored stale prefix");
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&joined.id().0);
        let head = crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap();
        assert!(head.is_some_and(|(v, _)| v == 3), "the seeded floor is the bootstrap head");
    }

    #[tokio::test]
    async fn a_losing_same_version_fork_cannot_replace_the_held_floor() {
        // The refusal half of fork convergence: a relay withholding OUR committed
        // floor edition while serving only a same-version fork with a HIGHER inner
        // id must be treated as withholding — held state and floor unchanged.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Fork2", vec!["wss://good".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let genesis_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        publish_community_meta(&relay, &community, &owner, "Ours", 2).await;
        let ours = follow_control(&relay, &community, &session).await.unwrap().expect("ours adopted");
        assert_eq!(ours.name, "Ours");
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let held_before = crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap().unwrap();
        let our_inner = crate::db::community::get_edition_head_inner_id(&cid_hex, &cid_hex).unwrap().unwrap();

        // Grind the fork to LOSE the tiebreak (higher inner id), then serve it —
        // with the genesis but WITHOUT our v2 — from a withholding relay.
        let meta = control::CommunityMetadata { name: "Theirs".into(), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let mut ts = 5_000u64;
        let fork_wrap = loop {
            let rumor = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 2, Some(&genesis_hash), &content, ts, None);
            if rumor.id.unwrap().to_bytes() > our_inner {
                break control::seal_control_edition(&rumor, &group, &owner, Timestamp::from_secs(ts)).unwrap().0;
            }
            ts += 1;
        };
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await; // genesis only
        relay.inject(&fork_wrap, &["wss://stale".to_string()]);
        let mut stale_view = ours.clone();
        stale_view.relays = vec!["wss://stale".into()];

        assert!(
            follow_control(&relay, &stale_view, &session).await.unwrap().is_none(),
            "a losing fork served without our floor edition changes nothing"
        );
        let held_after = crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap().unwrap();
        assert_eq!(held_after, held_before, "the floor row is untouched");
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert_eq!(held.name, "Ours", "the held state is untouched");
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
    async fn follow_control_records_a_new_private_channel_keyless_and_unreadable() {
        // A Private channel's key rides the rekey plane, not the control edition —
        // control-follow records it KEYLESS (epoch 0, the rekey-scan cursor), and
        // every read/send path refuses it until the key lands (never the root plane).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Priv", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0x33; 32]);
        publish_channel_edition(&relay, &community, &owner, &priv_id, "mods", true, 1, false).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session)
            .await
            .unwrap()
            .expect("the keyless record is a change");
        let ch = updated.channel(&priv_id).expect("the private channel is recorded");
        assert!(ch.private && ch.key.is_none(), "recorded keyless");
        assert_eq!(ch.epoch, Epoch(0), "epoch 0 = the root generation (scan cursor)");
        assert!(updated.channel_read_coords(ch).is_empty(), "unreadable until keyed");
        assert!(
            fetch_channel(&relay, &updated, &priv_id, 50).await.unwrap().is_empty(),
            "a keyless fetch returns empty (and never queries the root plane)"
        );
        assert!(
            send_message(&relay, &updated, &priv_id, "nope").await.is_err(),
            "a keyless send refuses"
        );
        // The keyless record round-trips (the stored placeholder never surfaces
        // as a real key).
        let reloaded = crate::db::community::load_community_v2(updated.id()).unwrap().unwrap();
        let rch = reloaded.channel(&priv_id).unwrap();
        assert!(rch.private && rch.key.is_none() && rch.epoch == Epoch(0), "keyless survives reload");
        // And a bundle minted while keyless never carries the placeholder.
        let bundle = bundle_of(&reloaded, None, None, None);
        assert!(
            !bundle.channels.iter().any(|c| c.id == crate::simd::hex::bytes_to_hex_32(&priv_id.0)),
            "an ungrantable keyless channel stays out of invite bundles"
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

    #[tokio::test]
    async fn follow_rekeys_finds_a_channel_rekey_under_an_archived_prior_root() {
        // PROTO-B2 regression: a Refounding's channel rekeys ride the PRIOR root
        // (CORD-06 §3). A follower who adopted the BASE first (the live window:
        // the base crate landed and was walked before the channel crates) must
        // still find them — the lookup fans across the archived roots, not just
        // the current one.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let mut community = create_community(&relay, "Strand", vec!["wss://r".into()], None).await.unwrap();
        let root0 = community.community_root;
        let priv_id = ChannelId([0x33; 32]);
        let key1 = [0x44; 32];
        add_private_channel(&mut community, priv_id, key1, Epoch(1));

        // The refounder's channel rekey (1 → 2), sealed + addressed under the PRIOR
        // root (root0), delivering the fresh key to me.
        let key2 = [0x55; 32];
        let prev_commit = super::super::derive::epoch_key_commitment(Epoch(1), &key1);
        let group = channel_rekey_group_key(&root0, &priv_id, Epoch(2));
        let blob = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &owner.public_key(), RekeyScope::Channel(priv_id), Epoch(2), &key2).unwrap();
        for e in rekey::build_rekey_chunks_local(&owner, &group, RekeyScope::Channel(priv_id), Epoch(2), Epoch(1), &prev_commit, &[blob], 2_000).unwrap() {
            relay.publish(&e, &community.relays).await.unwrap();
        }

        // Simulate the base having ALREADY advanced (the stranding order): the head
        // moved to a fresh root while root0 sits in the epoch-key archive (where
        // genesis put it).
        community.community_root = [0xB7; 32];
        community.root_epoch = Epoch(1);
        crate::db::community::save_community_v2(&community).unwrap();

        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("the prior-root crate is found");
        let ch = updated.channel(&priv_id).unwrap();
        assert_eq!(ch.epoch, Epoch(2), "the channel advanced despite the moved base");
        assert_eq!(ch.key, Some(key2), "adopted the key delivered under the prior root");
    }

    #[tokio::test]
    async fn follow_rekeys_keyless_cursor_walks_past_an_excluding_rotation_then_adopts() {
        // A keyless private channel (announced by vsk-2, key not yet held) has no
        // chain, so its epoch is a scan cursor: a complete rotation that excludes
        // us advances the cursor (never a removal — we were never in); a later
        // rotation that includes us is the entry point.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let mut community = create_community(&relay, "Cursor", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0x66; 32]);
        community.channels.push(ChannelV2 { id: priv_id, name: "vault".into(), private: true, key: None, epoch: Epoch(0) });
        crate::db::community::save_community_v2(&community).unwrap();
        let community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(community.channel(&priv_id).unwrap().key.is_none(), "keyless survives the round-trip");

        // Epoch 1: the creation delivery went to a stranger only (pre-dates us).
        let stranger = Keys::generate();
        let key1 = [0x71; 32];
        let pc1 = super::super::derive::epoch_key_commitment(Epoch(0), &community.community_root);
        let g1 = channel_rekey_group_key(&community.community_root, &priv_id, Epoch(1));
        let b1 = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &stranger.public_key(), RekeyScope::Channel(priv_id), Epoch(1), &key1).unwrap();
        for e in rekey::build_rekey_chunks_local(&owner, &g1, RekeyScope::Channel(priv_id), Epoch(1), Epoch(0), &pc1, &[b1], 2_000).unwrap() {
            relay.publish(&e, &community.relays).await.unwrap();
        }
        // Epoch 2: a later rotation includes ME (e.g. a removal-forced re-mint whose
        // recipient set is the CURRENT members).
        let key2 = [0x72; 32];
        let pc2 = super::super::derive::epoch_key_commitment(Epoch(1), &key1);
        let g2 = channel_rekey_group_key(&community.community_root, &priv_id, Epoch(2));
        let b2 = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &owner.public_key(), RekeyScope::Channel(priv_id), Epoch(2), &key2).unwrap();
        for e in rekey::build_rekey_chunks_local(&owner, &g2, RekeyScope::Channel(priv_id), Epoch(2), Epoch(1), &pc2, &[b2], 2_100).unwrap() {
            relay.publish(&e, &community.relays).await.unwrap();
        }

        // ONE follow: the cursor walks 0→1 (excluded, still keyless) and 1→2 (my
        // blob — adopt), because each real step re-loops.
        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("the walk lands on the included epoch");
        let ch = updated.channel(&priv_id).unwrap();
        assert_eq!(ch.epoch, Epoch(2), "cursor walked through the excluding epoch to the included one");
        assert_eq!(ch.key, Some(key2), "adopted the delivery that includes us");
    }

    #[tokio::test]
    async fn follow_rekeys_honors_an_admin_channel_rotation_but_never_a_strangers() {
        // CORD-06 §Authority: a CHANNEL rekey is honored from the owner or a
        // MANAGE_CHANNELS holder under the persisted roster — so an admin-run
        // rotation keys members up; a mere keyholder's forgery never does.
        use crate::community::roles::{CommunityRoles, MemberGrant, Role};
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let mut community = create_community(&relay, "AdminRot", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0x88; 32]);
        let key1 = [0x91; 32];
        add_private_channel(&mut community, priv_id, key1, Epoch(1));

        // Persist a roster granting `admin` the Admin role (MANAGE_CHANNELS ⊂ ADMIN_ALL).
        let admin = Keys::generate();
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let role = Role::admin("aa".repeat(32));
        let roster = CommunityRoles {
            roles: vec![role.clone()],
            grants: vec![MemberGrant { member: admin.public_key().to_hex(), role_ids: vec![role.role_id.clone()] }],
        };
        crate::db::community::set_community_roles(&cid_hex, &roster, 1_000).unwrap();

        // The ADMIN rotates the channel 1 → 2, delivering to me: adopted.
        let key2 = [0x92; 32];
        let pc = super::super::derive::epoch_key_commitment(Epoch(1), &key1);
        let g2 = channel_rekey_group_key(&community.community_root, &priv_id, Epoch(2));
        let me_pk = crate::state::MY_SECRET_KEY.to_keys().unwrap().public_key();
        let blob = rekey::build_blob_local(admin.secret_key(), &admin.public_key().to_bytes(), &me_pk, RekeyScope::Channel(priv_id), Epoch(2), &key2).unwrap();
        for e in rekey::build_rekey_chunks_local(&admin, &g2, RekeyScope::Channel(priv_id), Epoch(2), Epoch(1), &pc, &[blob], 2_000).unwrap() {
            relay.publish(&e, &community.relays).await.unwrap();
        }
        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("an admin rotation is honored");
        assert_eq!(updated.channel(&priv_id).unwrap().key, Some(key2), "adopted the admin's key");

        // A STRANGER (keyholder, no roster standing) rotates 2 → 3: refused.
        let rogue = Keys::generate();
        let key3 = [0x93; 32];
        let pc3 = super::super::derive::epoch_key_commitment(Epoch(2), &key2);
        let g3 = channel_rekey_group_key(&updated.community_root, &priv_id, Epoch(3));
        let rb = rekey::build_blob_local(rogue.secret_key(), &rogue.public_key().to_bytes(), &me_pk, RekeyScope::Channel(priv_id), Epoch(3), &key3).unwrap();
        for e in rekey::build_rekey_chunks_local(&rogue, &g3, RekeyScope::Channel(priv_id), Epoch(3), Epoch(2), &pc3, &[rb], 2_100).unwrap() {
            relay.publish(&e, &updated.relays).await.unwrap();
        }
        let follow = follow_rekeys(&relay, &updated, &session).await.unwrap();
        assert!(follow.updated.is_none(), "a stranger's channel rotation is never adopted");
    }

    #[tokio::test]
    async fn a_non_outranking_admins_rotation_never_concludes_my_removal() {
        // CORD-06 §Authority: the Rotator must strictly OUTRANK every removed
        // target. An equal-rank bit-holder's complete rotation that skips my blob
        // must read Stay (my record survives); the OWNER's reads Removed. Needs a
        // two-account bed: the follower must be a NON-owner admin.
        use crate::community::roles::{CommunityRoles, MemberGrant, Role};
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Outrank", bed.relays.clone(), None).await.unwrap();

        // The MEMBER's device: holds the community + the private channel, with a
        // persisted roster granting the member AND a peer the same Admin role.
        bed.swap_to(&member);
        let mut held = community.clone();
        let priv_id = ChannelId([0xAB; 32]);
        let key1 = [0xA1; 32];
        add_private_channel(&mut held, priv_id, key1, Epoch(1));
        let peer = Keys::generate();
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let role = Role::admin("bb".repeat(32));
        let roster = CommunityRoles {
            roles: vec![role.clone()],
            grants: vec![
                MemberGrant { member: peer.public_key().to_hex(), role_ids: vec![role.role_id.clone()] },
                MemberGrant { member: member.keys.public_key().to_hex(), role_ids: vec![role.role_id.clone()] },
            ],
        };
        crate::db::community::set_community_roles(&cid_hex, &roster, 1_000).unwrap();

        // The equal-rank PEER rotates 1 → 2 delivering only to themselves.
        let key2 = [0xA2; 32];
        let pc = super::super::derive::epoch_key_commitment(Epoch(1), &key1);
        let g2 = channel_rekey_group_key(&held.community_root, &priv_id, Epoch(2));
        let pb = rekey::build_blob_local(peer.secret_key(), &peer.public_key().to_bytes(), &peer.public_key(), RekeyScope::Channel(priv_id), Epoch(2), &key2).unwrap();
        for e in rekey::build_rekey_chunks_local(&peer, &g2, RekeyScope::Channel(priv_id), Epoch(2), Epoch(1), &pc, &[pb], 2_000).unwrap() {
            bed.relay.publish(&e, &held.relays).await.unwrap();
        }
        let session = SessionGuard::capture();
        let follow = follow_rekeys(&bed.relay, &held, &session).await.unwrap();
        assert!(follow.updated.is_none(), "an equal-rank rotation excluding me is Stay, never my removal");
        let reloaded = crate::db::community::load_community_v2(held.id()).unwrap().unwrap();
        assert!(reloaded.channel(&priv_id).is_some(), "my channel record survives the peer's rotation");

        // The OWNER's rotation excluding me IS a removal (owner outranks everyone).
        let key3 = [0xA3; 32];
        let stranger = Keys::generate();
        let ob = rekey::build_blob_local(owner.keys.secret_key(), &owner.keys.public_key().to_bytes(), &stranger.public_key(), RekeyScope::Channel(priv_id), Epoch(2), &key3).unwrap();
        for e in rekey::build_rekey_chunks_local(&owner.keys, &g2, RekeyScope::Channel(priv_id), Epoch(2), Epoch(1), &pc, &[ob], 2_100).unwrap() {
            bed.relay.publish(&e, &held.relays).await.unwrap();
        }
        let follow = follow_rekeys(&bed.relay, &held, &session).await.unwrap();
        let updated = follow.updated.expect("the owner's removal folds");
        assert!(updated.channel(&priv_id).is_none(), "the owner's exclusion cuts my channel record");
    }

    #[tokio::test]
    async fn converting_a_public_channel_to_private_is_refused() {
        // The conversion (CORD-03 §2) is a key rotation this build doesn't mint yet:
        // the producer refuses the flag flip, so no reader is left unkeyable.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "NoConvert", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let meta = control::ChannelMetadata { name: "general".into(), private: true, voice: None, deleted: None, custom: None, extra: Default::default() };
        let err = edit_channel_metadata(&relay, &community, &general, &meta).await.unwrap_err();
        assert!(err.contains("not supported"), "conversion is refused at the producer: {err}");
        // A rename of the same public channel still works.
        let meta = control::ChannelMetadata { name: "lobby".into(), private: false, voice: None, deleted: None, custom: None, extra: Default::default() };
        edit_channel_metadata(&relay, &community, &general, &meta).await.unwrap();
    }

    #[tokio::test]
    async fn a_rekey_plane_flood_cannot_bury_a_genuine_rotation() {
        // An insider floods the next-epoch rekey address (community_root-derived,
        // so any member can seal there) with >200 junk 3303s to push the owner's
        // genuine rotation out of a single fetch window. The paginated fetch must
        // still recover it and adopt.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Flooded", vec!["wss://r".into()], None).await.unwrap();
        let new_root = [0xD9; 32];
        let new_epoch = Epoch(1);
        let group = base_rekey_group_key(&community.community_root, community.id(), new_epoch);

        // The GENUINE owner rotation lands first (oldest).
        publish_base_rotation(&relay, &community, &owner, &[owner.public_key()], &new_root, &community.community_root).await;

        // Then a member floods 260 well-formed-but-unauthorized junk chunks ON TOP
        // (newer), burying the genuine one past the 200 newest.
        let rogue = Keys::generate();
        let prev_commit = super::super::derive::epoch_key_commitment(Epoch(0), &community.community_root);
        for i in 0..260u64 {
            let blob = rekey::build_blob_local(rogue.secret_key(), &rogue.public_key().to_bytes(), &rogue.public_key(), RekeyScope::Root, new_epoch, &[0xEE; 32]).unwrap();
            let rumor = rekey::build_rekey_rumor(rogue.public_key(), RekeyScope::Root, new_epoch, Epoch(0), &prev_commit, &[blob], 1, 1, 3_000 + i).unwrap();
            let (wrap, _) = rekey::seal_rekey_chunk(&rumor, &group, &rogue, Timestamp::from_secs(3_000 + i)).unwrap();
            relay.publish(&wrap, &community.relays).await.unwrap();
        }

        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("the genuine rotation is recovered past the flood");
        assert_eq!(updated.root_epoch, Epoch(1));
        assert_eq!(updated.community_root, new_root, "adopted the owner's root, not a junk one");
    }

    #[tokio::test]
    async fn a_swap_during_create_private_channel_aborts_without_a_write() {
        // create_private_channel straddles a memberlist fetch (seconds long) then
        // whole-row-saves. A swap in that window must abort — never mint a channel
        // into the swapped-in account, and never leave a half-published key crate
        // adopted locally.
        let (bed, owner, _member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "SwapCreate", bed.relays.clone(), None).await.unwrap();
        let before = crate::db::community::load_community_v2(community.id()).unwrap().unwrap().channels.len();

        // The memberlist fetch inside create bumps the generation mid-flight.
        let swap_relay = SwapMidFetch { inner: MemoryRelay::new() };
        let err = create_private_channel(&swap_relay, &community, "ghost").await.unwrap_err();
        assert!(err.contains("account changed"), "a swap mid-create aborts: {err}");
        let after = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert_eq!(after.channels.len(), before, "no channel row was written");
        assert!(!after.channels.iter().any(|c| c.name == "ghost"), "the ghost channel never persisted");
    }

    #[tokio::test]
    async fn two_admins_racing_a_channel_rotation_converge_on_one_key() {
        // CORD-06 §Failure-and-races: two DISTINCT authorized rotators mint the
        // same channel epoch concurrently (reachable — both hold MANAGE_CHANNELS).
        // Every follower must converge on the SAME key (the lexicographically
        // lowest), so the community never permanently forks. (Retaining the losing
        // fork's key for its race-window messages needs a multi-key-per-epoch
        // archive — a deferred refinement shared with v1; convergence, the
        // security-critical property, is what this pins.)
        use crate::community::roles::{CommunityRoles, MemberGrant, Role};
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let mut community = create_community(&relay, "Race", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0xC0; 32]);
        let key1 = [0xC1; 32];
        add_private_channel(&mut community, priv_id, key1, Epoch(1));

        // Two admins (a, b) both hold the Admin role; I hold the channel key.
        let (a, b) = (Keys::generate(), Keys::generate());
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let role = Role::admin("ce".repeat(32));
        let roster = CommunityRoles {
            roles: vec![role.clone()],
            grants: [&a, &b].iter().map(|k| MemberGrant { member: k.public_key().to_hex(), role_ids: vec![role.role_id.clone()] }).collect(),
        };
        crate::db::community::set_community_roles(&cid_hex, &roster, 1_000).unwrap();

        // Both rotate 1 → 2, each delivering their OWN fresh key to me, off the
        // same prevcommit — a genuine same-epoch fork.
        let me_pk = crate::state::MY_SECRET_KEY.to_keys().unwrap().public_key();
        let pc = super::super::derive::epoch_key_commitment(Epoch(1), &key1);
        let group = channel_rekey_group_key(&community.community_root, &priv_id, Epoch(2));
        let key_a = [0x0A; 32];
        let key_b = [0xFB; 32]; // higher — a's must win regardless of publish order
        for (signer, k) in [(&a, &key_a), (&b, &key_b)] {
            let blob = rekey::build_blob_local(signer.secret_key(), &signer.public_key().to_bytes(), &me_pk, RekeyScope::Channel(priv_id), Epoch(2), k).unwrap();
            for e in rekey::build_rekey_chunks_local(signer, &group, RekeyScope::Channel(priv_id), Epoch(2), Epoch(1), &pc, &[blob], 2_000).unwrap() {
                relay.publish(&e, &community.relays).await.unwrap();
            }
        }

        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("adopts a winner");
        let adopted = updated.channel(&priv_id).unwrap().key.unwrap();
        assert_eq!(adopted, key_a, "converges on the lexicographically lowest key (deterministic across clients)");

        // A SECOND follower (fresh, holding the same epoch-1 key) converges identically.
        let mut peer = community.clone();
        if let Some(c) = peer.channels.iter_mut().find(|c| c.id.0 == priv_id.0) {
            c.key = Some(key1);
            c.epoch = Epoch(1);
        }
        // Re-run the same fold from the peer's identical starting point → same winner.
        let updated2 = follow_rekeys(&relay, &peer, &session).await.unwrap().updated.expect("peer adopts");
        assert_eq!(updated2.channel(&priv_id).unwrap().key.unwrap(), key_a, "every follower lands on the identical key");
    }

    #[tokio::test]
    async fn create_private_channel_refuses_a_member_without_manage_channels() {
        // The local mirror of the reader's gate: an unauthorized member is refused
        // BEFORE any publish (no floor pollution, no orphan key crate).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Gate", bed.relays.clone(), None).await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();
        let err = create_private_channel(&bed.relay, &joined, "sneaky").await.unwrap_err();
        assert!(err.contains("MANAGE_CHANNELS"), "refused with the permission it lacks: {err}");
        let err = create_public_channel(&bed.relay, &joined, "sneaky-too").await.unwrap_err();
        assert!(err.contains("MANAGE_CHANNELS"), "public creation gates identically: {err}");
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
    async fn a_channel_owned_by_another_community_is_skipped_not_clobbered() {
        // channel_id is the sole DB primary key, so a bundle/replay reusing another
        // community's channel_id must NOT overwrite that row. It's skipped (not an
        // error — erroring would wedge all of this community's control persistence).
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let a = create_community(&relay, "A", vec!["wss://r".into()], None).await.unwrap();
        let a_channel = a.channels[0].id;
        let mut b = create_community(&relay, "B", vec!["wss://r".into()], None).await.unwrap();
        let b_channel = b.channels[0].id;
        // B's set includes a phantom whose id collides with A's channel.
        b.channels.push(ChannelV2 { id: a_channel, name: "phantom".into(), private: false, key: None, epoch: b.root_epoch });

        crate::db::community::save_community_v2(&b).expect("save succeeds, the phantom is skipped");
        // A's channel row is untouched.
        let a_reloaded = crate::db::community::load_community_v2(a.id()).unwrap().unwrap();
        assert!(!a_reloaded.channels.iter().any(|c| c.private), "A's channel is untouched");
        assert_eq!(a_reloaded.channels[0].id.0, a_channel.0);
        // B keeps its own channel but never acquired a row for the foreign id.
        let b_reloaded = crate::db::community::load_community_v2(b.id()).unwrap().unwrap();
        assert!(b_reloaded.channel(&b_channel).is_some(), "B's own channel persists");
        assert!(b_reloaded.channel(&a_channel).is_none(), "the foreign-owned channel is skipped, not stolen");
    }

    /// A single relay that CAPS every query below the page size (modelling a real
    /// relay's maxFilterLimit) and honors `until` — so the join-verify walk MUST
    /// paginate to reach an old genesis. MemoryRelay can't model this (it unions then
    /// truncates the whole set), which is why a MemoryRelay flood test gives false
    /// confidence about the production `LiveTransport` behaviour.
    struct CappedRelay {
        events: Vec<Event>,
        cap: usize,
    }
    #[async_trait::async_trait]
    impl Transport for CappedRelay {
        async fn publish(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn publish_durable(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn fetch(&self, q: &Query, _r: &[String]) -> Result<Vec<Event>, String> {
            let mut m: Vec<Event> = self
                .events
                .iter()
                .filter(|e| q.authors.is_empty() || q.authors.contains(&e.pubkey.to_hex()))
                .filter(|e| q.until.is_none_or(|u| e.created_at.as_secs() <= u))
                .cloned()
                .collect();
            m.sort_by(|a, b| b.created_at.cmp(&a.created_at)); // newest first
            m.truncate(self.cap.min(q.limit.unwrap_or(usize::MAX)));
            Ok(m)
        }
    }

    #[tokio::test]
    async fn verify_pages_a_capped_relay_past_a_flood_to_the_genesis() {
        // The join-verify DoS mitigation, tested against a relay that caps below PAGE
        // (production behaviour MemoryRelay hides): a rogue root-holder buries the
        // genesis under junk, and the `until`-walk must page past it. Uses fixed OLD
        // timestamps so `until = now` includes everything and the walk is deterministic.
        let (_tmp, _guard, owner) = init_test_db();
        let meta = control::CommunityMetadata { name: "Capped".into(), relays: vec!["wss://r".into()], ..Default::default() };
        let g = control::genesis(&owner, meta, 1_000).unwrap();
        let community = CommunityV2::from_genesis(&g, "Capped", None, vec!["wss://r".into()], 1_000);

        let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let rogue = Keys::generate();
        let mut events: Vec<Event> = g.wraps.to_vec();
        for i in 0..250u64 {
            let rumor = control::build_edition_rumor(rogue.public_key(), vsk::CHANNEL_METADATA, &[0xAB; 32], 1, None, "{\"name\":\"junk\",\"private\":false}", 1_001 + i, None);
            let (wrap, _) = control::seal_control_edition(&rumor, &control, &rogue, Timestamp::from_secs(1_001 + i)).unwrap();
            events.push(wrap);
        }
        // Cap 100/query forces the walk across ~3 pages down to the genesis at ts 1000.
        let relay = CappedRelay { events, cap: 100 };
        let verified = verify_owner_root_and_reconcile(&relay, community.clone()).await;
        assert!(verified.is_ok(), "the until-walk pages a capped relay past the flood to the genesis: {:?}", verified.err());
    }

    #[tokio::test]
    async fn accept_parked_invite_joins_from_the_stored_bundle() {
        // The 3313 receive path: an invite is parked as its bundle JSON, then accepted
        // from the stored bundle (re-verifying the owner root over the network).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Parked", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "owner: hi").await.unwrap();
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        let inviter_hex = owner.keys.public_key().to_hex();

        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, Some(&inviter_hex)).await.unwrap();
        assert_eq!(joined.id().0, community.id().0, "joined the community from the parked bundle");
        assert!(joined.identity.verify());
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["owner: hi"]);
        // The join seeded the verified fold as the member's initial floor, so their
        // first follow can't roll below the state the join just showed.
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&joined.id().0);
        assert!(
            crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap().is_some(),
            "the joiner's control floor is seeded from the join-time fold"
        );

        // The Guestbook memberlist now folds both participants.
        bed.swap_to(&owner);
        let members = memberlist(&bed.relay, &community).await.unwrap();
        assert!(members.contains(&member.keys.public_key()), "the parked-invite joiner is a member");
    }

    #[tokio::test]
    async fn accept_parked_invite_rejects_a_forged_root() {
        // A forged-root parked bundle (real identity triple, attacker-chosen root) fails
        // accept — the shared accept path re-verifies the owner root, so a parked invite
        // gets the same eclipse protection as a live one.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Real", vec!["wss://r".into()], None).await.unwrap();
        let mut forged = bundle_of(&community, None, None, None);
        let fake = crate::simd::hex::bytes_to_hex_32(&[0xEE; 32]);
        forged.community_root = fake.clone();
        for ch in &mut forged.channels {
            ch.key = fake.clone();
        }
        let bundle_json = serde_json::to_string(&forged).unwrap();

        let err = accept_parked_invite(&relay, &bundle_json, None).await.unwrap_err();
        assert!(err.contains("could not verify"), "a forged-root parked bundle fails definitively: {err}");
    }

    #[test]
    fn v2_and_v1_bundles_are_distinguishable_by_parse() {
        // The protocol discriminator the facade list/accept relies on: a v2 bundle
        // (self-certifying: owner + owner_salt + community_root) parses; a v1-shaped
        // one does not, so a parked invite routes to the right accept path.
        let owner = Keys::generate();
        let identity = super::super::control::CommunityIdentity::mint(&owner.public_key());
        let hex = crate::simd::hex::bytes_to_hex_32;
        let v2 = invite::CommunityInvite {
            community_id: hex(&identity.community_id.0),
            owner: hex(&identity.owner_xonly),
            owner_salt: hex(&identity.owner_salt),
            community_root: hex(&[0x11; 32]),
            root_epoch: 0,
            channels: vec![],
            relays: vec!["wss://r".into()],
            name: "V2".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };
        let v2_json = serde_json::to_string(&v2).unwrap();
        assert!(invite::CommunityInvite::from_bundle_json(&v2_json).is_ok(), "a real v2 bundle parses");
        let v1_like = r#"{"community_id":"aa","name":"X","relays":[]}"#;
        assert!(invite::CommunityInvite::from_bundle_json(v1_like).is_err(), "a v1 bundle is not a v2 bundle");
    }

    #[tokio::test]
    async fn verify_rejects_a_cross_community_owner_edition_replay() {
        // The eclipse-via-replay: an owner-signed edition from community X (eid == X.id)
        // rewrapped onto a FORGED community T's fake control plane must NOT authenticate
        // T. T's genesis has eid == T.id, so X's edition — a genuine owner signature but
        // a different eid — is not a valid proof of T's root. This is why "any owner
        // edition" is unsound and the eid==community_id genesis pin is required.
        let (_tmp, _guard, owner) = init_test_db();

        // Community X (real), owned by `owner`.
        let gx = control::genesis(&owner, control::CommunityMetadata { name: "X".into(), ..Default::default() }, 1_000).unwrap();
        let x_control = control_group_key(&gx.community_root, &gx.identity.community_id, Epoch(0));
        let (_ed, opened) = control::open_control_edition(&gx.wraps[0], &x_control).unwrap();

        // Forged community T: the real owner triple but an ATTACKER-chosen root.
        let t_identity = control::CommunityIdentity::mint(&owner.public_key());
        let fake_root = [0xEE; 32];
        let t = CommunityV2 {
            identity: t_identity,
            community_root: fake_root,
            root_epoch: Epoch(0),
            name: "T".into(),
            description: None,
            relays: vec!["wss://r".into()],
            channels: vec![],
            dissolved: false,
            created_at_ms: 0,
        };
        // Rewrap X's owner-signed genesis onto T's fake control plane (the attacker
        // controls the fake root, so they can derive its control group key).
        let t_control = control_group_key(&fake_root, t.id(), t.root_epoch);
        let (replayed, _) = stream::rewrap_seal(&opened.seal, &t_control, Timestamp::from_secs(1_000)).unwrap();
        let relay = MemoryRelay::new();
        relay.publish(&replayed, &t.relays).await.unwrap();

        let verified = verify_owner_root_and_reconcile(&relay, t.clone()).await;
        assert!(verified.is_err(), "a cross-community owner-edition replay must not authenticate a forged root");
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

    #[tokio::test]
    async fn chat_ops_react_edit_delete_round_trip() {
        let (bed, owner, _member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Ops", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        let me_hex = owner.keys.public_key().to_hex();

        let msg_id = send_message(&bed.relay, &community, &general, "original").await.unwrap();
        send_reaction(&bed.relay, &community, &general, &msg_id, &me_hex, super::super::kind::MESSAGE, ":fire:", Some(("fire", "https://e/f.png")))
            .await
            .unwrap();
        send_edit(&bed.relay, &community, &general, &msg_id, "edited").await.unwrap();
        send_delete(&bed.relay, &community, &general, &msg_id, super::super::kind::MESSAGE).await.unwrap();

        let page = fetch_channel(&bed.relay, &community, &general, 50).await.unwrap();
        let target = crate::simd::hex::hex_to_bytes_32(&msg_id);
        let mut saw = (false, false, false);
        for f in &page {
            match &f.event {
                ChatEvent::Reaction { target: t, emoji, emoji_url, .. } if *t == target => {
                    assert_eq!(emoji, ":fire:");
                    assert_eq!(emoji_url.as_deref(), Some("https://e/f.png"));
                    saw.0 = true;
                }
                ChatEvent::Edit { target: t, new_content, .. } if *t == target => {
                    assert_eq!(new_content, "edited");
                    saw.1 = true;
                }
                ChatEvent::Delete { target: t, .. } if *t == target => saw.2 = true,
                _ => {}
            }
        }
        assert!(saw.0 && saw.1 && saw.2, "reaction/edit/delete all round-trip: {saw:?}");
    }

    #[tokio::test]
    async fn a_typing_signal_rides_the_ephemeral_wrap_and_is_never_stored() {
        let (bed, owner, _member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Typ", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        let group = channel_group_key(&community.community_root, &general, community.root_epoch);

        // A live subscriber sees the 21059 wrap and it opens as Typing…
        let mut sub = bed.relay.subscribe(Query {
            kinds: vec![stream::KIND_WRAP_EPHEMERAL],
            authors: vec![group.pk_hex()],
            ..Default::default()
        });
        send_typing(&bed.relay, &community, &general).await.unwrap();
        let wrap = sub.try_recv().expect("the typing wrap streams to a live subscriber");
        assert!(
            matches!(chat::open_chat_event(&wrap, &group, &general, community.root_epoch), Ok(ChatEvent::Typing { .. })),
            "the ephemeral wrap opens as a Typing event"
        );

        // …while nothing durable is stored (relays never keep the ephemeral tier),
        // so channel history stays free of typing noise.
        let page = fetch_channel(&bed.relay, &community, &general, 50).await.unwrap();
        assert!(page.iter().all(|f| !matches!(f.event, ChatEvent::Typing { .. })));
    }

    #[tokio::test]
    async fn send_chat_message_threads_the_reply_and_extra_tags() {
        let (bed, owner, _member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Re", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        let me_hex = owner.keys.public_key().to_hex();

        let parent_id = send_message(&bed.relay, &community, &general, "parent").await.unwrap();
        let imeta = nostr_sdk::prelude::Tag::custom(
            nostr_sdk::prelude::TagKind::custom("imeta"),
            ["url https://e/blob".to_string(), "m image/png".to_string()],
        );
        let child_id = send_chat_message(
            &bed.relay, &community, &general, "child",
            Some((parent_id.as_str(), me_hex.as_str())), &[], vec![imeta],
        )
        .await
        .unwrap();

        let page = fetch_channel(&bed.relay, &community, &general, 50).await.unwrap();
        let child = page
            .iter()
            .find_map(|f| match &f.event {
                ChatEvent::Message { opened, reply_to, .. } if opened.rumor_id.to_hex() == child_id => Some((opened, reply_to)),
                _ => None,
            })
            .expect("the reply message round-trips");
        let reply = child.1.as_ref().expect("the reply reference is carried");
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&reply.id), parent_id);
        assert_eq!(reply.author, Some(owner.keys.public_key()));
        assert!(
            child.0.rumor.tags.iter().any(|t| t.kind() == nostr_sdk::prelude::TagKind::custom("imeta")),
            "the imeta attachment tag rides the rumor verbatim"
        );
    }

    #[tokio::test]
    async fn a_kick_needs_kick_authority_and_removes_the_target() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Kick", bed.relays.clone(), None).await.unwrap();

        // The target announces a Join (as an accepted invite would).
        let gb = super::super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
        let join = guestbook::build_join_rumor(member.keys.public_key(), None, 2_000);
        let (wrap, _) = guestbook::seal_guestbook_rumor(&join, &gb, &member.keys, Timestamp::from_secs(2)).unwrap();
        bed.relay.publish(&wrap, &bed.relays).await.unwrap();
        let before = memberlist(&bed.relay, &community).await.unwrap();
        assert!(before.contains(&member.keys.public_key()), "the join lands first");

        // An unprivileged member's kick of the owner is refused locally…
        bed.swap_to(&member);
        let err = kick_member(&bed.relay, &community, &owner.keys.public_key()).await.unwrap_err();
        assert!(err.contains("not authorized"), "unprivileged kick refused: {err}");

        // …and the owner (supreme, no grant needed) kicks the member out.
        bed.swap_to(&owner);
        kick_member(&bed.relay, &community, &member.keys.public_key()).await.unwrap();
        let after = memberlist(&bed.relay, &community).await.unwrap();
        assert!(!after.contains(&member.keys.public_key()), "the kicked member leaves the fold");
        assert!(after.contains(&owner.keys.public_key()), "the owner remains");
    }

    #[tokio::test]
    async fn grant_admin_mints_one_deterministic_role_and_revoke_strips_it() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Adm", bed.relays.clone(), None).await.unwrap();
        let member_pk = member.keys.public_key();
        let member_hex = member_pk.to_hex();
        let owner_hex = owner.keys.public_key().to_hex();

        grant_admin(&bed.relay, &community, &member_pk).await.unwrap();
        let view = fetch_authority(&bed.relay, &community).await;
        assert!(view.roles.is_admin(&member_hex), "the grant folds as admin");
        assert!(view.roles.is_authorized(&member_hex, Some(&owner_hex), Permissions::MANAGE_ROLES));

        // A second grant (any device) converges on the SAME role entity — and a
        // repeat is a no-op, not a version bump.
        let second = Keys::generate().public_key();
        grant_admin(&bed.relay, &community, &second).await.unwrap();
        grant_admin(&bed.relay, &community, &member_pk).await.unwrap();
        let view = fetch_authority(&bed.relay, &community).await;
        assert_eq!(view.roles.roles.len(), 1, "one Admin role, never a fork");
        assert!(view.roles.is_admin(&member_hex) && view.roles.is_admin(&second.to_hex()));
        let grant = view.roles.grants.iter().find(|g| g.member == member_hex).unwrap();
        assert_eq!(grant.role_ids.len(), 1, "no duplicate role id in the grant");

        // Revoke strips ONLY the admin role and de-authorizes.
        revoke_admin(&bed.relay, &community, &member_pk).await.unwrap();
        let view = fetch_authority(&bed.relay, &community).await;
        assert!(!view.roles.is_admin(&member_hex), "revoked");
        assert!(view.roles.is_admin(&second.to_hex()), "the other admin is untouched");
        assert!(!view.roles.is_authorized(&member_hex, Some(&owner_hex), Permissions::KICK));
    }

    #[tokio::test]
    async fn follow_control_persists_the_roster_for_sync_local_reads() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Persist", bed.relays.clone(), None).await.unwrap();
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let member_hex = member.keys.public_key().to_hex();
        grant_admin(&bed.relay, &community, &member.keys.public_key()).await.unwrap();

        // The passive follow folds + persists; the read is then LOCAL (v1 parity).
        let session = crate::state::SessionGuard::capture();
        follow_control(&bed.relay, &community, &session).await.unwrap();
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap();
        assert!(roster.is_admin(&member_hex), "the persisted roster reads back without a fetch");

        // A withholding relay serves nothing — an empty fold raises no gap flag, and
        // the stored roster must be RETAINED, never wiped.
        let withholding = MemoryRelay::new();
        let _ = follow_control(&withholding, &community, &session).await;
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap();
        assert!(roster.is_admin(&member_hex), "withholding never shrinks standing");

        // A real revocation (a NEWER grant edition) does replace it.
        revoke_admin(&bed.relay, &community, &member.keys.public_key()).await.unwrap();
        follow_control(&bed.relay, &community, &session).await.unwrap();
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap();
        assert!(!roster.is_admin(&member_hex), "the revoke folds + persists");
    }

    #[tokio::test]
    async fn grant_admin_is_refused_for_a_non_owner_and_publishes_nothing() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "NoSquat", bed.relays.clone(), None).await.unwrap();

        bed.swap_to(&member);
        let err = grant_admin(&bed.relay, &community, &member.keys.public_key()).await.unwrap_err();
        assert!(err.contains("owner"), "refused before any publish: {err}");

        // The deterministic admin-role entity stays unsquatted — the owner's later
        // legitimate mint is version 1 and folds cleanly.
        bed.swap_to(&owner);
        let view = fetch_authority(&bed.relay, &community).await;
        assert!(view.roles.roles.is_empty(), "no role edition landed");
        grant_admin(&bed.relay, &community, &member.keys.public_key()).await.unwrap();
        let view = fetch_authority(&bed.relay, &community).await;
        assert!(view.roles.is_admin(&member.keys.public_key().to_hex()));
    }

    #[tokio::test]
    async fn grant_admin_merges_other_roles_and_refuses_a_withheld_grant() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Merge", bed.relays.clone(), None).await.unwrap();
        let member_pk = member.keys.public_key();

        // The member already holds a Mod role, granted through the real send path
        // (so this device's floors track both entities).
        let mod_rid = crate::simd::hex::bytes_to_hex_32(&[0x66; 32]);
        set_role(&bed.relay, &community, &admin_role(&mod_rid, Permissions::BAN)).await.unwrap();
        grant_roles(&bed.relay, &community, &member_pk, vec![mod_rid.clone()]).await.unwrap();

        // A relay that withholds the control plane must refuse the merge — a blind
        // push would erase the Mod role at a higher version.
        let withholding = MemoryRelay::new();
        let err = grant_admin(&withholding, &community, &member_pk).await.unwrap_err();
        assert!(err.contains("could not be fetched"), "withheld grant refused: {err}");

        // Against the full relay the merge preserves the Mod role.
        grant_admin(&bed.relay, &community, &member_pk).await.unwrap();
        let view = fetch_authority(&bed.relay, &community).await;
        let grant = view.roles.grants.iter().find(|g| g.member == member_pk.to_hex()).unwrap();
        assert_eq!(grant.role_ids.len(), 2, "admin ADDED to the existing grant, not replacing it");
        assert!(grant.role_ids.contains(&mod_rid));
    }

    #[tokio::test]
    async fn fetch_authority_reflects_a_granted_admin() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Auth", bed.relays.clone(), None).await.unwrap();
        let rid = crate::simd::hex::bytes_to_hex_32(&[0x5a; 32]);
        publish_role(&bed.relay, &community, &owner.keys, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &member.keys.public_key(), vec![rid], 1).await;

        let view = fetch_authority(&bed.relay, &community).await;
        let member_hex = member.keys.public_key().to_hex();
        assert!(view.roles.is_admin(&member_hex), "the granted member folds as admin");
        assert!(
            view.roles.is_authorized(&member_hex, Some(&owner.keys.public_key().to_hex()), Permissions::KICK),
            "an ADMIN_ALL grant carries KICK"
        );
        assert!(view.banned.is_empty());
    }
}
