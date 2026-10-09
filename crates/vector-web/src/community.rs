//! Concord communities (v2) over the desktop command surface.
//!
//! v1 communities are read-only history on the web: new v1 joins ended at the
//! protocol's timelock, and its desktop glue is platform-heavy.

use std::time::Duration;

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::community::transport::LiveTransport;
use vector_core::community::v2::community::CommunityV2;
use vector_core::community::{ChannelId, CommunityId, ConcordProtocol};
use vector_core::sending::SendCallback;
use vector_core::simd::hex::bytes_to_hex_32;
use vector_core::{db, Message, VectorCore, STATE};

use crate::messaging::WebSendCallback;

pub(crate) fn id32(hex: &str) -> Result<[u8; 32], String> {
    let bytes = vector_core::simd::hex::hex_to_bytes_32(hex);
    if hex.len() != 64 {
        return Err(format!("invalid id: {hex}"));
    }
    Ok(bytes)
}

pub(crate) fn is_v2(community_id: &str) -> bool {
    id32(community_id).is_ok_and(|b| {
        matches!(db::community::community_protocol(&CommunityId(b)).ok().flatten(), Some(ConcordProtocol::V2))
    })
}

pub(crate) fn load_v2(community_id: &str) -> Result<CommunityV2, String> {
    db::community::load_community_v2(&CommunityId(id32(community_id)?))?.ok_or_else(|| "Community not found".to_string())
}

pub(crate) fn summarize_v2(c: &CommunityV2) -> Value {
    let me = vector_core::my_public_key();
    let owner = c.owner().ok();
    json!({
        "community_id": bytes_to_hex_32(&c.identity.community_id.0),
        "name": c.name,
        "description": c.description,
        "is_owner": matches!((me, owner), (Some(m), Some(o)) if m == o),
        "has_icon": c.icon.is_some(),
        "channels": c.channels.iter().map(|ch| json!({
            "channel_id": bytes_to_hex_32(&ch.id.0),
            "name": ch.name,
            "private": ch.private,
            "readable": !ch.private || ch.key.is_some(),
        })).collect::<Vec<_>>(),
        "primary_channel": c.primary_channel().map(|ch| bytes_to_hex_32(&ch.id.0)),
        "owner_npub": owner.and_then(|pk| pk.to_bech32().ok()),
        "dissolved": c.dissolved,
        "preloaded": false,
        "proto_version": ConcordProtocol::V2.as_i64(),
        "relays": c.relays,
    })
}

fn summarize_any(id: &CommunityId) -> Option<Value> {
    match db::community::community_protocol(id).ok().flatten() {
        Some(ConcordProtocol::V2) => db::community::load_community_v2(id).ok().flatten().map(|c| summarize_v2(&c)),
        _ => db::community::load_community(id).ok().flatten().map(|c| {
            json!({
                "community_id": c.id.to_hex(),
                "name": c.name,
                "description": c.description,
                "is_owner": false,
                "has_icon": c.icon.is_some(),
                "channels": c.channels.iter().map(|ch| json!({
                    "channel_id": ch.id.to_hex(), "name": ch.name, "private": false, "readable": true,
                })).collect::<Vec<_>>(),
                "primary_channel": c.channels.first().map(|ch| ch.id.to_hex()),
                "owner_npub": null,
                "dissolved": c.dissolved,
                "preloaded": false,
                "proto_version": ConcordProtocol::V1.as_i64(),
                "relays": c.relays,
            })
        }),
    }
}

pub fn list_communities() -> Result<Value, String> {
    let ids = db::community::list_community_ids()?;
    Ok(Value::Array(ids.iter().filter_map(summarize_any).collect()))
}

pub fn get_community(community_id: &str) -> Result<Value, String> {
    summarize_any(&CommunityId(id32(community_id)?)).ok_or_else(|| "Community not found".to_string())
}

pub fn get_community_admins(community_id: &str) -> Result<Value, String> {
    if !is_v2(community_id) {
        return Ok(json!([]));
    }
    let roles = VectorCore.community_roles(community_id).map_err(|e| e.to_string())?;
    Ok(roles.get("admins").cloned().unwrap_or_else(|| json!([])))
}

pub async fn get_community_members(community_id: &str) -> Value {
    let members = VectorCore.get_community_members(community_id).await;
    Value::Array(
        members
            .iter()
            .filter_map(|m| m.get("npub").and_then(Value::as_str))
            .map(|npub| json!({ "npub": npub, "last_active": 0 }))
            .collect(),
    )
}

pub fn get_community_banlist(community_id: &str) -> Value {
    json!(VectorCore
        .get_community_banned(community_id)
        .into_iter()
        .filter_map(|h| PublicKey::from_hex(&h).ok().and_then(|pk| pk.to_bech32().ok()).or(Some(h)))
        .collect::<Vec<_>>())
}

pub fn get_community_invite_summary(community_id: &str) -> Result<Value, String> {
    let is_public = !db::community::get_community_invite_registry(community_id)?.is_empty();
    let creators: Vec<Value> = db::community::get_invite_link_sets(community_id)?
        .into_iter()
        .filter(|s| !s.locators.is_empty())
        .filter_map(|s| {
            let npub = PublicKey::from_hex(&s.creator_hex).ok()?.to_bech32().ok()?;
            Some(json!({ "npub": npub, "count": s.locators.len() }))
        })
        .collect();
    Ok(json!({ "is_public": is_public, "creators": creators }))
}

pub async fn sync_community_channel(channel_id: &str, before_ms: Option<u64>, reset_cursor: bool) -> Result<Value, String> {
    use vector_core::community::cache;
    let is_older = before_ms.is_some();
    let key = format!("{channel_id}:{}", if is_older { "older" } else { "latest" });
    let empty = json!({ "new_messages": 0, "reached_start": false, "oldest_ms": null });
    if !cache::try_begin_page_fetch(&key) {
        return Ok(empty);
    }
    struct Claim(String);
    impl Drop for Claim {
        fn drop(&mut self) {
            vector_core::community::cache::end_page_fetch(&self.0);
        }
    }
    let _claim = Claim(key);

    if reset_cursor {
        cache::clear_channel_floors(channel_id);
    }
    if is_older && cache::is_at_history_start(channel_id) {
        return Ok(json!({ "new_messages": 0, "reached_start": true, "oldest_ms": null }));
    }
    let community_id = db::community::community_id_for_channel(channel_id)?.ok_or("Unknown Community channel")?;
    if !is_v2(&community_id) {
        return Ok(json!({ "new_messages": 0, "reached_start": true, "oldest_ms": null }));
    }

    const LIMIT: usize = 50;
    const SINCE_LOOKBACK_SECS: u64 = 120;
    let since = if is_older { None } else { cache::newest_cursor(channel_id).map(|s| s.saturating_sub(SINCE_LOOKBACK_SECS)) };
    let before = if is_older { before_ms.map(|m| m / 1000) } else { None };
    let count = db::scoped(VectorCore.sync_community_channel_page(channel_id, LIMIT, before, since))
        .await
        .map(|(c, _)| c)
        .unwrap_or_default();
    let reached_start = if is_older { count.fetched == 0 } else { count.new_messages < LIMIT };
    Ok(json!({ "new_messages": count.new_messages, "reached_start": reached_start, "oldest_ms": null }))
}

/// Send with an optimistic bubble whose id is the real one: the rumor is built
/// first, so the echo and the pending row are the same message.
pub async fn send_community_message(channel_id: String, content: String, replied_to: Option<String>, bot: Option<String>) -> Result<(), String> {
    db::scoped(async move {
        let reply = replied_to.filter(|r| !r.is_empty());
        let (content, color_spans) = vector_core::text_color::extract(&content);
        let bot_pk = bot.as_deref().filter(|b| !b.is_empty()).and_then(|b| PublicKey::parse(b).ok());
        let mut extra_tags: Vec<Tag> = bot_pk.map(|pk| vec![vector_core::bot_interface::bot_tag(&pk)]).unwrap_or_default();
        extra_tags.extend(vector_core::text_color::to_nostr_tags(&color_spans));
        let addressed_bots: Vec<String> = bot_pk.and_then(|pk| pk.to_bech32().ok()).into_iter().collect();

        let author = vector_core::my_public_key().ok_or("Public key not set")?;
        let community_id = db::community::community_id_for_channel(&channel_id)?.ok_or("Unknown Community channel")?;
        if !is_v2(&community_id) {
            return Err("Legacy communities are read-only on Vector Web".into());
        }
        let community = load_v2(&community_id)?;
        let ch = ChannelId(id32(&channel_id)?);
        let channel = community.channel(&ch).ok_or("Channel not found in Community")?;
        let (_, epoch) = community.channel_secret(channel);
        let ms = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

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
        let rumor = vector_core::community::v2::chat::build_message_rumor(
            author, &ch, epoch, &content, reply_ref, &emoji_pairs, extra_tags.clone(), ms,
        );
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
            addressed_bots,
            color_spans,
            expiration: expiry,
            ..Default::default()
        };
        let _ = db::events::populate_reply_context(&mut pending).await;
        STATE.lock().await.add_message_to_chat(&channel_id, &pending);
        let callback = WebSendCallback;
        callback.on_pending(&channel_id, &pending);

        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let sent = vector_core::community::v2::service::send_chat_message_at(
            &transport, &community, &ch, &content, reply_ref, &emoji_pairs, extra_tags, ms,
        )
        .await;
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
                    callback.on_sent(&channel_id, &message_id, &msg);
                    callback.on_persist(&channel_id, &msg);
                }
                Ok(())
            }
            Err(e) => {
                let failed = STATE.lock().await.update_message(&message_id, |m| {
                    m.set_failed(true);
                    m.set_pending(false);
                });
                if let Some((_, msg)) = failed {
                    callback.on_failed(&channel_id, &message_id, &msg);
                }
                Err(e)
            }
        }
    })
    .await
}

/// Reactions, edits and deletes. The facade's echo is silent, so the UI gets its update here.
pub async fn community_control(channel_id: &str, target: &str, op: ControlOp<'_>) -> Result<(), String> {
    let community_id = db::community::community_id_for_channel(channel_id)?.ok_or("Unknown Community channel")?;
    if !is_v2(&community_id) {
        return Err("Legacy communities are read-only on Vector Web".into());
    }
    match op {
        ControlOp::React { emoji, url } => {
            vector_core::badges::check_new_reaction_allowance(target, emoji).await?;
            VectorCore.send_community_reaction(channel_id, target, emoji, url).await.map_err(|e| e.to_string())?;
        }
        ControlOp::Edit(content) => {
            VectorCore.edit_community_message(channel_id, target, content).await.map_err(|e| e.to_string())?;
        }
        ControlOp::Delete => {
            VectorCore.delete_community_message_in(channel_id, target).await.map_err(|e| e.to_string())?;
            vector_core::emit_event("message_removed", &json!({ "id": target, "chat_id": channel_id, "reason": "deleted" }));
            return Ok(());
        }
    }
    if let Some((_, msg)) = STATE.lock().await.find_message(target) {
        vector_core::emit_event("message_update", &json!({ "old_id": target, "message": msg, "chat_id": channel_id }));
    }
    Ok(())
}

pub enum ControlOp<'a> {
    React { emoji: &'a str, url: Option<&'a str> },
    Edit(&'a str),
    Delete,
}

pub async fn create_community(name: String, channel_name: Option<String>, relays: Option<Vec<String>>) -> Result<Value, String> {
    let relays = match relays {
        Some(r) if !r.is_empty() => r,
        _ => vector_core::state::active_trusted_relays().await.iter().map(|s| s.to_string()).collect(),
    };
    if relays.is_empty() {
        return Err("No relays available to host the Community".into());
    }
    let channel_name = channel_name.unwrap_or_else(|| "general".into());
    let session = db::current_session();
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let mut community = vector_core::community::v2::service::create_community(&transport, &name, relays, None).await?;
    if channel_name != "general" {
        if let Some(ch) = community.channels.first().cloned() {
            let mut meta = ch.metadata();
            meta.name = channel_name.clone();
            vector_core::community::v2::service::edit_channel_metadata(&transport, &community, &ch.id, &meta).await?;
            if let Ok(Some(fresh)) = db::community::load_community_v2(community.id()) {
                community = fresh;
            }
        }
    }
    let channel_id = community.channels.first().map(|ch| bytes_to_hex_32(&ch.id.0)).ok_or("created community has no channel")?;
    VectorCore.register_v2_chats(&community, &session).await;
    if let Some(client) = vector_core::state::nostr_client() {
        vector_core::community::v2::realtime::refresh_subscription(&client).await;
    }
    Ok(json!({
        "community_id": bytes_to_hex_32(&community.id().0),
        "channel_id": channel_id,
        "owner_npub": community.owner().ok().and_then(|pk| pk.to_bech32().ok()),
        "channel_name": channel_name,
        "proto_version": ConcordProtocol::V2.as_i64(),
    }))
}

pub async fn create_community_channel(community_id: String, name: String, private: bool) -> Result<Value, String> {
    db::scoped_result(async move {
        if !is_v2(&community_id) {
            return Err("Only Concord v2 communities support extra channels".to_string());
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("A channel needs a name".into());
        }
        let community = load_v2(&community_id)?;
        let transport = LiveTransport::with_timeout(Duration::from_secs(20));
        let channel_id = if private {
            vector_core::community::v2::service::create_private_channel(&transport, &community, &name).await?
        } else {
            vector_core::community::v2::service::create_public_channel(&transport, &community, &name).await?
        };
        if let Ok(c) = load_v2(&community_id) {
            VectorCore.register_v2_chats(&c, &db::current_session()).await;
        }
        Ok(json!(bytes_to_hex_32(&channel_id.0)))
    })
    .await
}

pub async fn preview_public_invite(url: &str) -> Result<Value, String> {
    if vector_core::community::v2::invite::parse_invite_link(url).is_err() {
        return Err("Only Concord v2 invite links can be opened on Vector Web".into());
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let bundle = vector_core::community::v2::service::fetch_public_bundle(&transport, url).await?;
    let community_id = bundle.community_id.clone();
    if let Ok(local) = load_v2(&community_id) {
        return Ok(json!({
            "name": local.name, "description": local.description,
            "icon": local.icon.as_ref().map(|i| i.to_community_image()),
            "community_id": community_id,
        }));
    }
    let folded = vector_core::community::v2::service::preview_bundle(&transport, &bundle).await?;
    Ok(json!({
        "name": folded.name, "description": folded.description,
        "icon": folded.icon.as_ref().map(|i| i.to_community_image()),
        "community_id": community_id,
    }))
}

pub async fn accept_public_invite(url: String) -> Result<Value, String> {
    db::scoped(async move {
        if vector_core::community::v2::invite::parse_invite_link(&url).is_err() {
            return Err("Only Concord v2 invite links can be opened on Vector Web".to_string());
        }
        let joined = VectorCore.join_community(&url).await.map_err(|e| e.to_string())?;
        let cid = joined
            .get("community_id")
            .or_else(|| joined.get("id"))
            .and_then(Value::as_str)
            .ok_or("join returned no community id")?;
        get_community(cid)
    })
    .await
}

pub async fn accept_community_invite(community_id: &str) -> Result<Value, String> {
    VectorCore.accept_pending_invite(community_id).await.map_err(|e| e.to_string())?;
    get_community(community_id)
}

/// Join/leave lines in a channel, saved once and painted live.
pub async fn apply_presence(
    channel_id: &str,
    npub: &str,
    joined: bool,
    event_id: &str,
    created_at: u64,
    invited_by: Option<&str>,
    invited_label: Option<&str>,
) {
    use vector_core::stored_event::SystemEventType;
    let et = if joined { SystemEventType::MemberJoined } else { SystemEventType::MemberLeft };
    let note = invited_by.map(|by| match invited_label {
        Some(l) if !l.is_empty() => format!("{by}|{l}"),
        _ => by.to_string(),
    });
    let inserted = db::events::save_system_event_at(event_id, channel_id, et, npub, note.as_deref(), created_at, invited_by, invited_label)
        .await
        .unwrap_or(false);
    if !inserted {
        return;
    }
    let nameless = STATE
        .lock()
        .await
        .get_profile(npub)
        .is_none_or(|p| p.nickname().is_empty() && p.display_name.is_empty() && p.name.is_empty());
    if nameless {
        vector_core::profile::sync::queue_profile_sync(npub.to_string(), vector_core::profile::sync::SyncPriority::High, false);
    }
    vector_core::emit_event(
        "system_event",
        &json!({
            "conversation_id": channel_id, "event_id": event_id, "event_type": et.as_u8(),
            "member_pubkey": npub, "member_name": null,
            "invited_by": invited_by, "invited_label": invited_label,
            "created_at_ms": created_at.saturating_mul(1000),
        }),
    );
}

/// One attachment into a channel: local copy, optimistic bubble, seal, upload,
/// mirror, then the channel message naming it.
pub async fn send_community_file(
    channel_id: String,
    replied_to: String,
    bytes: std::sync::Arc<Vec<u8>>,
    name: String,
    extension: String,
    img_meta: Option<vector_core::types::ImageMetadata>,
) -> Result<Value, String> {
    use vector_core::sending::{FileSource, Sealed};
    db::scoped(async move {
        let reply = Some(replied_to).filter(|r| !r.is_empty());
        let community_id = db::community::community_id_for_channel(&channel_id)?.ok_or("Unknown Community channel")?;
        if !is_v2(&community_id) {
            return Err("Legacy communities are read-only on Vector Web".to_string());
        }
        let community = load_v2(&community_id)?;
        let ch = ChannelId(id32(&channel_id)?);
        community.channel(&ch).ok_or("Channel not found in Community")?;
        let author = vector_core::my_public_key().ok_or("Public key not set")?;

        let hash = vector_core::crypto::sha256_hex(&bytes);
        let local = db::get_download_dir().join(format!("{hash}.{extension}"));
        vector_core::webfiles::write(&local, &bytes).await?;
        let params = vector_core::crypto::generate_encryption_params();
        let mut attachment = vector_core::types::Attachment {
            id: hash.clone(), key: params.key, nonce: params.nonce,
            extension: extension.clone(), name, url: String::new(),
            path: local.to_string_lossy().to_string(), size: bytes.len() as u64 + 16,
            img_meta, downloading: false, downloaded: true,
            ..Default::default()
        };

        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default();
        let pending_id = format!("pending-{}", now.as_nanos());
        let callback = WebSendCallback;
        let mut pending = Message {
            id: pending_id.clone(), at: now.as_millis() as u64, pending: true, mine: true,
            npub: author.to_bech32().ok(), replied_to: reply.clone().unwrap_or_default(),
            attachments: vec![attachment.clone()], ..Default::default()
        };
        let _ = db::events::populate_reply_context(&mut pending).await;
        STATE.lock().await.add_message_to_chat(&channel_id, &pending);
        callback.on_pending(&channel_id, &pending);

        let failed = |e: String| async {
            let row = STATE.lock().await.update_message(&pending_id, |m| {
                m.set_failed(true);
                m.set_pending(false);
            });
            if let Some((_, msg)) = row {
                callback.on_failed(&channel_id, &pending_id, &msg);
            }
            Err::<Value, String>(e)
        };

        let signer = match vector_core::signer::active_signer() {
            Ok(s) => s,
            Err(e) => return failed(format!("Signer unavailable: {e}")).await,
        };
        let servers = vector_core::state::get_blossom_servers();
        let cancel = callback.cancel_token(&pending_id);
        let sealed = vector_core::sending::seal_or_reuse(
            FileSource::Bytes(bytes), &attachment.id, &attachment.key, &attachment.nonce,
            &pending_id, std::sync::Arc::new(callback), cancel.clone(),
        )
        .await;
        match sealed {
            Err(e) => return failed(e).await,
            Ok(Sealed::Reused(r)) => {
                (attachment.key, attachment.nonce, attachment.url, attachment.size) = (r.key, r.nonce, r.url, r.size);
            }
            Ok(Sealed::Body(body, _guard)) => {
                let pid = pending_id.clone();
                let progress: vector_core::blossom::ProgressCallback = std::sync::Arc::new(move |pct, sent| {
                    callback.on_upload_progress(&pid, pct.unwrap_or(0), sent.unwrap_or(0))
                });
                let mime = vector_core::crypto::mime_from_extension(&extension);
                let accepted = match vector_core::blossom::upload_body_with_progress_and_failover(
                    signer.clone(), servers.clone(), body, Some(mime), true, progress,
                    Some(3), Some(Duration::from_secs(2)), cancel,
                )
                .await
                {
                    Ok(a) => a,
                    Err(e) => return failed(format!("Upload failed: {e}")).await,
                };
                attachment.url = accepted.url.clone();
                attachment.fallback_urls = vector_core::blossom::mirror_blob_to_servers(
                    signer, &accepted.url, servers, 2, Duration::from_secs(5), std::slice::from_ref(&accepted.server),
                )
                .await;
            }
        }
        callback.on_upload_complete(&channel_id, &pending_id, &attachment.id, &attachment.url);

        let mut tags = vec![vector_core::community::attachments::attachment_to_imeta(&attachment)];
        if let Some(exp) = vector_core::self_destruct::chat_duration_secs(&channel_id).and_then(vector_core::self_destruct::expiry_after) {
            tags.push(Tag::expiration(Timestamp::from_secs(exp)));
        }
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
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let sent = vector_core::community::v2::service::send_chat_message(&transport, &community, &ch, "", reply_ref, &[], tags).await;
        let real_id = match sent {
            Ok(id) => id,
            Err(e) => return failed(e).await,
        };
        let echoed = {
            let mut state = STATE.lock().await;
            state.remove_message(&pending_id);
            let path = attachment.path.clone();
            state.update_attachment(&channel_id, &real_id, &attachment.id, |a| {
                a.set_downloaded(true);
                a.set_downloading(false);
                a.path = path.clone().into_boxed_str();
            });
            state.find_message(&real_id).map(|(_, m)| m)
        };
        if let Some(msg) = echoed {
            callback.on_sent(&channel_id, &pending_id, &msg);
            callback.on_persist(&channel_id, &msg);
        }
        Ok(json!({ "message_id": real_id, "webxdc_topic": null }))
    })
    .await
}
