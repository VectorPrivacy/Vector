//! Concord Communities: summaries, rosters, channel history and sends, and the catch-up that
//! pages every held channel behind the live subscription.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::community::transport::LiveTransport;
use vector_core::community::v2::community::CommunityV2;
use vector_core::community::v2::realtime;
use vector_core::community::v2::volley::{self, PaintTarget};
use vector_core::community::{ChannelId, CommunityId};
use vector_core::sending::SendCallback;
use vector_core::simd::hex::bytes_to_hex_32;
use vector_core::{db, Message, VectorCore, STATE};

use super::messaging::Callback;

fn id32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!("invalid id: {hex}"));
    }
    Ok(vector_core::simd::hex::hex_to_bytes_32(hex))
}

fn is_held(community_id: &str) -> bool {
    id32(community_id).is_ok_and(|b| db::community::community_held(&CommunityId(b)).unwrap_or(false))
}

fn load(community_id: &str) -> Result<CommunityV2, String> {
    db::community::load_community_v2(&CommunityId(id32(community_id)?))?.ok_or_else(|| "Community not found".to_string())
}

/// `list_communities`' shape, which `data::Community` reads.
pub fn summarize(c: &CommunityV2) -> Value {
    let me = vector_core::my_public_key();
    let mine = VectorCore::my_channel_permissions(c);
    let owner = c.owner().ok();
    json!({
        "community_id": bytes_to_hex_32(&c.identity.community_id.0),
        "name": c.name,
        "description": c.description.clone().unwrap_or_default(),
        "is_owner": matches!((me, owner), (Some(m), Some(o)) if m == o),
        "has_icon": c.icon.is_some(),
        "channels": c.layout().1.into_iter().map(|(ch, cat)| json!({
            "channel_id": bytes_to_hex_32(&ch.id.0),
            "name": ch.name,
            "private": ch.private,
            "readable": !ch.private || ch.key.is_some(),
            "category_id": cat.map(|k| bytes_to_hex_32(&k.id)),
            "topic": ch.topic(),
            "view": ch.view().as_str(),
            "my_permissions": mine.get(&ch.id.0).map(|p| p.to_string()),
            "restricted": ch.restricted(),
            "can_post": vector_core::community::v2::service::can_post(c, &ch.id),
        })).collect::<Vec<_>>(),
        "categories": VectorCore::categories_json(c),
        "primary_channel": c.primary_channel().map(|ch| bytes_to_hex_32(&ch.id.0)),
        "owner_npub": owner.and_then(|pk| pk.to_bech32().ok()),
        "dissolved": c.dissolved,
        "relays": c.relays,
        "voice_relays": c.voice_relays(),
    })
}

pub fn list() -> Vec<Value> {
    db::community::list_community_ids()
        .unwrap_or_default()
        .iter()
        .filter_map(|id| db::community::load_community_v2(id).ok().flatten())
        .map(|c| summarize(&c))
        .collect()
}

/// The roster as npubs, and which of them hold the admin role.
pub async fn members(community_id: String) -> (Vec<String>, Vec<String>) {
    let members = VectorCore
        .get_community_members(&community_id)
        .await
        .iter()
        .filter(|m| m.get("unlisted").is_none())
        .filter_map(|m| m.get("npub")?.as_str().map(str::to_string))
        .collect();
    let admins = if is_held(&community_id) {
        VectorCore
            .community_roles(&community_id)
            .ok()
            .and_then(|r| r.get("admins").cloned())
            .and_then(|a| serde_json::from_value::<Vec<String>>(a).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    (members, admins)
}

/// A channel's newest page, or the page before `before_ms`; whether its start is reached.
pub async fn sync_channel(channel_id: String, before_ms: Option<u64>) -> Result<bool, String> {
    use vector_core::community::cache;
    let is_older = before_ms.is_some();
    let Some(_claim) = cache::claim_page_fetch(&channel_id, is_older) else { return Ok(false) };
    let community_id = db::community::community_id_for_channel(&channel_id)?.ok_or("Unknown Community channel")?;
    if !is_held(&community_id) {
        return Ok(true);
    }
    const LIMIT: usize = 50;
    let before = before_ms.map(|m| m / 1000);
    let count = db::scoped(VectorCore.sync_community_channel_page(&channel_id, LIMIT, before, None)).await.map(|(c, _)| c).unwrap_or_default();
    Ok(if is_older { vector_core::community::reached_history_start(&channel_id, before, &count) } else { count.new_messages < LIMIT })
}

/// The optimistic bubble carries the real id: the rumor is built before it shows.
pub async fn send(channel_id: String, content: String, replied_to: String) -> Result<(), String> {
    db::scoped(async move {
        let reply = (!replied_to.is_empty()).then_some(replied_to);
        let community_id = db::community::community_id_for_channel(&channel_id)?.ok_or("Unknown Community channel")?;
        let (content, role_mentions) = vector_core::community::mentions::resolve_in(&community_id, &content);
        let (content, text_spans) = vector_core::text_spans::extract_own(&content);
        let mut extra_tags: Vec<Tag> = vector_core::text_spans::to_nostr_tags(&text_spans).into_iter().collect();
        extra_tags.extend(vector_core::community::mentions::to_tags(&role_mentions));

        let author = vector_core::my_public_key().ok_or("Public key not set")?;
        let community = load(&community_id)?;
        let ch = ChannelId(id32(&channel_id)?);
        let channel = community.channel(&ch).ok_or("Channel not found in Community")?;
        let (_, epoch) = community.channel_secret(channel);
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

        let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&content);
        let emoji_pairs: Vec<(&str, &str)> = emoji_tags.iter().map(|t| (t.shortcode.as_str(), t.url.as_str())).collect();
        let reply_owned = match reply.as_deref() {
            Some(parent) => {
                let author_hex = STATE
                    .lock()
                    .await
                    .find_message(parent)
                    .and_then(|(_, m)| m.npub.as_deref().and_then(|n| PublicKey::parse(n).ok()))
                    .map(|pk| pk.to_hex())
                    .unwrap_or_default();
                Some((parent.to_string(), author_hex))
            }
            None => None,
        };
        let reply_ref = reply_owned.as_ref().map(|(id, a)| (id.as_str(), a.as_str()));
        let expiry = vector_core::self_destruct::resolve_send_expiry(&channel_id);
        if let Some(exp) = expiry {
            extra_tags.push(Tag::expiration(Timestamp::from_secs(exp)));
        }
        let rumor = vector_core::community::v2::chat::build_message_rumor(author, &ch, epoch, &content, reply_ref, &emoji_pairs, extra_tags.clone(), ms);
        let rumor = vector_core::community::v2::service::outbound_rumor(&community, &ch, rumor);
        let message_id = rumor.id.ok_or("inner rumor has no id")?.to_hex();

        let mut pending = Message {
            id: message_id.clone(),
            content: content.clone(),
            at: ms,
            pending: true,
            mine: true,
            npub: author.to_bech32().ok(),
            replied_to: reply.clone().unwrap_or_default(),
            emoji_tags: emoji_tags.clone(),
            text_spans,
            role_mentions,
            expiration: expiry,
            ..Default::default()
        };
        let _ = db::events::populate_reply_context(&mut pending).await;
        STATE.lock().await.add_message_to_chat(&channel_id, &pending);
        Callback.on_pending(&channel_id, &pending);

        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let sent = vector_core::community::v2::service::send_chat_message_at(&transport, &community, &ch, &content, reply_ref, &emoji_pairs, extra_tags, ms).await;
        match sent {
            Ok(sent_id) => {
                let row = {
                    let mut state = STATE.lock().await;
                    if sent_id != message_id {
                        state.remove_message(&message_id);
                        state.find_message(&sent_id).map(|(_, m)| m)
                    } else {
                        state.update_message(&message_id, |m| m.set_pending(false)).map(|(_, m)| m)
                    }
                };
                if let Some(msg) = row {
                    Callback.on_sent(&channel_id, &message_id, &msg);
                    Callback.on_persist(&channel_id, &msg);
                }
                Ok(())
            }
            Err(e) => {
                let failed = STATE.lock().await.update_message(&message_id, |m| {
                    m.set_failed(true);
                    m.set_pending(false);
                });
                if let Some((_, msg)) = failed {
                    Callback.on_failed(&channel_id, &message_id, &msg);
                }
                Err(e)
            }
        }
    })
    .await
}

pub enum Control<'a> {
    React(&'a str),
    Edit(&'a str),
    Delete,
}

/// Reactions, edits and deletes. The service's echo is silent, so the UI hears it here.
pub async fn control(channel_id: &str, target: &str, op: Control<'_>) -> Result<(), String> {
    match op {
        Control::React(emoji) => {
            vector_core::badges::check_new_reaction_allowance(target, emoji).await?;
            VectorCore.send_community_reaction(channel_id, target, emoji, None).await.map_err(|e| e.to_string())?;
        }
        Control::Edit(content) => {
            VectorCore.edit_community_message(channel_id, target, content).await.map_err(|e| e.to_string())?;
        }
        Control::Delete => {
            VectorCore.delete_community_message_in(channel_id, target).await.map_err(|e| e.to_string())?;
            vector_core::emit_event("message_removed", &json!({ "id": target, "chat_id": channel_id, "reason": "deleted" }));
            return Ok(());
        }
    }
    if let Some((_, msg)) = STATE.lock().await.find_message(target) {
        vector_core::traits::emit_message_replaced(channel_id, target, &msg);
    }
    Ok(())
}

// ── Catch-up ────────────────────────────────────────────────────────────────

/// Channels paged at once.
const WINDOW: usize = 6;
/// The paint asks from a little before each channel's newest held message.
const SINCE_SLACK_SECS: u64 = 3600;
const COOLDOWN_MS: u64 = 20_000;

static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static RERUN: AtomicBool = AtomicBool::new(false);
static RECONNECT_ARMED: AtomicBool = AtomicBool::new(false);
static LAST_END_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Sweep every held channel, or queue one more behind a sweep already going: a relay that came
/// up mid-run may hold what that run never asked it for.
pub fn request() {
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        RERUN.store(true, Ordering::SeqCst);
        return;
    }
    db::spawn_bound(run());
}

fn rerun() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(run())
}

/// A burst of reconnects becomes one sweep once it settles.
pub fn after_reconnect() {
    if !realtime::subscription_ready() || RECONNECT_ARMED.swap(true, Ordering::SeqCst) {
        return;
    }
    db::spawn_bound(async {
        let cooling = (LAST_END_MS.load(Ordering::SeqCst) + COOLDOWN_MS).saturating_sub(now_ms());
        tokio::time::sleep(Duration::from_millis(cooling.max(1500))).await;
        RECONNECT_ARMED.store(false, Ordering::SeqCst);
        request();
    });
}

async fn run() {
    RERUN.store(false, Ordering::SeqCst);
    sweep().await;
    LAST_END_MS.store(now_ms(), Ordering::SeqCst);
    IN_FLIGHT.store(false, Ordering::SeqCst);
    if RERUN.swap(false, Ordering::SeqCst) && !db::session_stopped() {
        IN_FLIGHT.store(true, Ordering::SeqCst);
        db::spawn_bound(rerun());
    }
}

async fn sweep() {
    db::spawn_bound(ingest_list());
    let _ = db::community::purge_expired_pending_invites();
    let Ok(ids) = db::community::list_community_ids() else { return };
    let last = db::events::get_all_chats_last_messages().await.unwrap_or_default();
    let activity = |cid: &str| last.get(cid).and_then(|v| v.first().map(|m| m.at)).unwrap_or(0);

    let session = db::current_session();
    let mut held: Vec<CommunityId> = Vec::new();
    let mut targets: Vec<(PaintTarget, u64)> = Vec::new();
    for id in ids {
        let Ok(Some(c)) = db::community::load_community_v2(&id) else { continue };
        VectorCore.register_v2_chats(&c, &session).await;
        if c.dissolved {
            continue;
        }
        held.push(CommunityId(id.0));
        for ch in &c.channels {
            let hex = bytes_to_hex_32(&ch.id.0);
            let at = activity(&hex);
            let since = (at > 0).then(|| (at / 1000).saturating_sub(SINCE_SLACK_SECS).max(vector_core::history_gaps::proven(&hex).unwrap_or(0)));
            targets.push((PaintTarget { community_id: CommunityId(id.0), channel_hex: hex, since }, at));
        }
    }
    super::emit("communities_changed", ());
    targets.sort_by_key(|t| std::cmp::Reverse(t.1));
    let channels: Vec<String> = targets.iter().map(|(t, _)| t.channel_hex.clone()).collect();
    volley::paint_all(targets.into_iter().map(|(t, _)| t).collect()).await;
    for id in &held {
        realtime::enqueue_follow(id);
    }
    stream::iter(channels)
        .map(|cid| async move {
            if !db::session_stopped() {
                let _ = sync_channel(cid, None).await;
            }
        })
        .buffer_unordered(WINDOW)
        .collect::<Vec<()>>()
        .await;
    if let Some(client) = vector_core::state::nostr_client() {
        realtime::force_refresh_subscription(&client).await;
    }
}

/// Memberships joined or left on another device.
async fn ingest_list() {
    use vector_core::community::v2::service as v2;
    let Some(client) = vector_core::state::nostr_client() else { return };
    let bootstrap: Vec<String> = client.relays().await.keys().map(|r| r.to_string()).collect();
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let Ok(outcome) = v2::sync_community_list(&transport, &bootstrap).await else { return };
    let session = db::current_session();
    for c in &outcome.joined {
        VectorCore.register_v2_chats(c, &session).await;
        realtime::enqueue_follow(c.id());
    }
    if !outcome.joined.is_empty() || !outcome.removed.is_empty() {
        super::emit("communities_changed", ());
    }
    realtime::refresh_subscription(&client).await;
}

// ── Lifecycle and layout ────────────────────────────────────────────────────

fn channel_ids(community_id: &str) -> Vec<String> {
    load(community_id).map(|c| c.channels.iter().map(|ch| bytes_to_hex_32(&ch.id.0)).collect()).unwrap_or_default()
}

async fn reregister(community_id: &str) {
    if let Ok(c) = load(community_id) {
        VectorCore.register_v2_chats(&c, &db::current_session()).await;
    }
    super::emit("communities_changed", ());
}

/// The new channel's id.
pub async fn create_channel(community_id: String, name: String, private: bool, category_id: Option<String>, voice: bool) -> Result<String, String> {
    db::scoped_result(async move {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("A channel needs a name".into());
        }
        let community = load(&community_id)?;
        let transport = LiveTransport::with_timeout(Duration::from_secs(20));
        let category = category_id.as_deref().map(id32).transpose()?;
        let channel_id = vector_core::community::v2::service::create_channel_in(&transport, &community, &name, private, category).await?;
        let hex = bytes_to_hex_32(&channel_id.0);
        if voice {
            VectorCore.edit_channel_details(&community_id, &hex, &name, None, "voice").await.map_err(|e| e.to_string())?;
        }
        reregister(&community_id).await;
        Ok(hex)
    })
    .await
}

pub async fn create_category(community_id: String, name: String) -> Result<(), String> {
    db::scoped(async move {
        VectorCore.create_category(&community_id, name.trim()).await.map_err(|e| e.to_string())?;
        reregister(&community_id).await;
        Ok(())
    })
    .await
}

async fn teardown_local(id: &str, channels: &[String], relays: &[String]) {
    let _ = db::community::delete_community_retain_keys(id);
    if let Some(client) = vector_core::state::nostr_client() {
        realtime::refresh_subscription(&client).await;
    }
    STATE.lock().await.chats.retain(|c| !channels.contains(&c.id));
    for ch in channels {
        let _ = db::chats::delete_chat(ch);
    }
    vector_core::community::transport::prune_unneeded_community_relays(relays).await;
}

/// Leave as a member, or dissolve it as its owner.
pub async fn leave_or_delete(community_id: String, owner: bool) -> Result<(), String> {
    db::scoped(async move {
        let channels = channel_ids(&community_id);
        let relays = load(&community_id).map(|c| c.relays).unwrap_or_default();
        if owner {
            if is_held(&community_id) && !db::community::get_community_dissolved(&community_id).unwrap_or(false) {
                VectorCore.dissolve_community(&community_id).await.map_err(|e| e.to_string())?;
            }
        } else {
            VectorCore.leave_community_now(&community_id).await.map_err(|e| e.to_string())?;
        }
        teardown_local(&community_id, &channels, &relays).await;
        super::emit("communities_changed", ());
        Ok(())
    })
    .await
}

/// Every channel read to its newest message.
pub async fn mark_read(community_id: String) {
    for ch in channel_ids(&community_id) {
        super::messaging::mark_as_read(ch.clone(), None).await;
        STATE.lock().await.unread_clear(&ch);
    }
    vector_core::emit_event("unread_counts_stale", &json!({ "chats": [] }));
}

/// A new single-channel Community on the account's relays: its id and its channel's.
pub async fn create_community(name: String) -> Result<(String, String), String> {
    db::scoped_result(async move {
        let relays: Vec<String> = vector_core::state::active_trusted_relays().await.iter().map(|s| s.to_string()).collect();
        if relays.is_empty() {
            return Err("No relays available to host the Community".into());
        }
        let session = db::current_session();
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let community = vector_core::community::v2::service::create_community_on_roles_v2(&transport, name.trim(), relays, None).await?;
        let channel_id = community.channels.first().map(|ch| bytes_to_hex_32(&ch.id.0)).ok_or("created community has no channel")?;
        VectorCore.register_v2_chats(&community, &session).await;
        if let Some(client) = vector_core::state::nostr_client() {
            realtime::refresh_subscription(&client).await;
        }
        super::emit("communities_changed", ());
        Ok((bytes_to_hex_32(&community.id().0), channel_id))
    })
    .await
}

/// A shareable link anyone can join by.
pub async fn create_public_invite(community_id: String, label: Option<String>) -> Result<String, String> {
    db::scoped(async move { VectorCore.create_public_invite(&community_id, None, label).await.map_err(|e| e.to_string()) }).await
}

pub struct InviteLinks {
    pub mine: Vec<db::community::PublicInviteRecord>,
    /// Anyone's link keeps the Community joinable, not only ours.
    pub public: bool,
    /// Other creators and how many live links each holds.
    pub others: Vec<(String, usize)>,
}

pub async fn invite_links(community_id: String) -> InviteLinks {
    db::scoped(async move {
        let mine = db::community::list_public_invites(&community_id).unwrap_or_default();
        let public = db::community::get_community_invite_registry(&community_id).map(|r| !r.is_empty()).unwrap_or(!mine.is_empty());
        let me = vector_core::my_public_key().map(|k| k.to_hex());
        let others = db::community::get_invite_link_sets(&community_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.locators.is_empty() && Some(&s.creator_hex) != me.as_ref())
            .filter_map(|s| Some((PublicKey::from_hex(&s.creator_hex).ok()?.to_bech32().ok()?, s.locators.len())))
            .collect();
        InviteLinks { mine, public, others }
    })
    .await
}

pub async fn revoke_invite(community_id: String, token: String) -> Result<(), String> {
    db::scoped(async move { VectorCore.revoke_public_invite(&community_id, &token).await.map_err(|e| e.to_string()) }).await
}

/// Who can't be invited: the banned (as npubs).
pub fn banned(community_id: &str) -> Vec<String> {
    VectorCore
        .get_community_banned(community_id)
        .into_iter()
        .map(|h| PublicKey::from_hex(&h).ok().and_then(|pk| pk.to_bech32().ok()).unwrap_or(h))
        .collect()
}

pub async fn invite(community_id: String, npub: String) -> Result<(), String> {
    db::scoped(async move { VectorCore.invite_to_community(&community_id, &npub).await.map(|_| ()).map_err(|e| e.to_string()) }).await
}

/// Invites parked for consent, newest first.
pub async fn pending_invites() -> Vec<crate::data::RawInvite> {
    db::scoped(async move {
        db::community::list_pending_invites()
            .unwrap_or_default()
            .into_iter()
            .map(|i| crate::data::RawInvite {
                community_id: i.community_id,
                bundle_json: i.bundle_json,
                inviter_npub: i.inviter_npub,
                received_at: i.received_at,
                sent_at: i.sent_at,
                ends_at: i.ends_at,
            })
            .collect()
    })
    .await
}

pub async fn accept_invite(community_id: String) -> Result<(), String> {
    db::scoped(async move {
        VectorCore.accept_pending_invite(&community_id).await.map_err(|e| e.to_string())?;
        super::emit("communities_changed", ());
        Ok(())
    })
    .await
}

pub async fn decline_invite(community_id: String) -> Result<(), String> {
    db::scoped(async move { VectorCore.decline_community_invite(&community_id).await.map(|_| ()).map_err(|e| e.to_string()) }).await
}

#[derive(Clone, Debug)]
pub struct InvitePreview {
    pub community_id: String,
    pub name: String,
    pub description: String,
}

/// What an invite link leads to, read off its bundle (or our copy, once joined).
pub async fn preview_invite(url: String) -> Result<InvitePreview, String> {
    db::scoped(async move {
        vector_core::community::v2::invite::parse_invite_link(&url).map_err(|e| e.to_string())?;
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let bundle = vector_core::community::v2::service::fetch_public_bundle(&transport, &url).await?;
        let community_id = bundle.community_id.clone();
        if let Ok(local) = load(&community_id) {
            return Ok(InvitePreview { community_id, name: local.name.clone(), description: local.description.clone().unwrap_or_default() });
        }
        let folded = vector_core::community::v2::service::preview_bundle(&transport, &bundle).await?;
        Ok(InvitePreview { community_id, name: folded.name.clone(), description: folded.description.clone().unwrap_or_default() })
    })
    .await
}

/// Joins by link: the Community's id.
pub async fn join_by_link(url: String) -> Result<String, String> {
    db::scoped(async move {
        let joined = VectorCore.join_community(&url).await.map_err(|e| e.to_string())?;
        let id = joined.get("community_id").or_else(|| joined.get("id")).and_then(Value::as_str).ok_or("join returned no community id")?.to_string();
        if let Ok(c) = load(&id) {
            VectorCore.register_v2_chats(&c, &db::current_session()).await;
        }
        super::emit("communities_changed", ());
        Ok(id)
    })
    .await
}

/// What this account may do here: `get_community_capabilities`' flags.
pub fn capabilities(community_id: &str) -> std::collections::HashSet<String> {
    VectorCore
        .community_capabilities(community_id)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .map(|o| o.into_iter().filter(|(_, v)| v.as_bool() == Some(true)).map(|(k, _)| k).collect())
        .unwrap_or_default()
}

pub async fn edit_metadata(community_id: String, name: Option<String>, description: Option<String>) -> Result<(), String> {
    db::scoped(async move {
        VectorCore.edit_community_metadata(&community_id, name.as_deref(), description.as_deref()).await.map_err(|e| e.to_string())?;
        super::emit("communities_changed", ());
        Ok(())
    })
    .await
}

pub async fn set_voice_relays(community_id: String, relays: Vec<String>) -> Result<Vec<String>, String> {
    db::scoped(async move { VectorCore.set_voice_relays(&community_id, &relays).await.map_err(|e| e.to_string()) }).await
}

/// A relay that can't host the Community, by name and why; else the moved list and its gaps.
pub async fn move_relays(community_id: String, relays: Vec<String>) -> Result<(Vec<String>, Vec<String>), String> {
    db::scoped(async move {
        let checks = VectorCore.check_community_relays(&community_id, &relays).await.map_err(|e| e.to_string())?;
        if let Some(bad) = checks.iter().find(|c| !c.ok) {
            return Err(bad.reason.clone().unwrap_or_else(|| format!("{} can't hold this community.", bad.relay)));
        }
        let moved = VectorCore.move_community_relays(&community_id, &relays).await.map_err(|e| e.to_string())?;
        super::emit("communities_changed", ());
        Ok((moved.relays, moved.warnings))
    })
    .await
}

pub async fn set_banned(community_id: String, npubs: Vec<String>, banned: bool) -> Result<(), String> {
    db::scoped(async move {
        let refs: Vec<&str> = npubs.iter().map(String::as_str).collect();
        VectorCore.set_members_banned(&community_id, &refs, banned).await.map_err(|e| e.to_string())?;
        super::emit("community_refreshed", community_id);
        Ok(())
    })
    .await
}

/// `get_community_audit_log`: newest first, as JSON the Audit Log reads.
pub async fn audit_log(community_id: String) -> Result<serde_json::Value, String> {
    db::scoped(async move {
        let log = VectorCore.community_audit_log(&community_id).await.map_err(|e| e.to_string())?;
        serde_json::to_value(log).map_err(|e| e.to_string())
    })
    .await
}
