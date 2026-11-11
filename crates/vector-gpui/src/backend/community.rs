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
        "description": c.description,
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
