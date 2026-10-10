//! Message actions beyond send: reactions, edits, deletes, retries, read state, link previews, attachment actions.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use nostr_sdk::prelude::{EventId, PublicKey};
use serde_json::{json, Value};
use vector_core::sending::{SendCallback, SendConfig};
use vector_core::{db, ChatType, Message, SiteMetadata, VectorCore, STATE};

use crate::commands::{core, to_value, Args};
use crate::community::{self as cm, ControlOp};
use crate::messaging::WebSendCallback;

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move { run(cmd, a).await.transpose() })
}

async fn run(cmd: &str, a: &Args) -> Result<Option<Value>, String> {
    Ok(Some(match cmd {
        "react_to_message" => react_to_message(a).await?,
        "edit_message" => edit_message(a).await?,
        "delete_own_message" => delete_own_message(a.str("messageId")?).await?,
        "delete_failed_message" => {
            delete_failed_message(a.str("messageId")?).await?;
            Value::Null
        }
        "retry_failed_dm" => Value::Bool(retry_failed_dm(a.str("receiver")?, a.str("messageId")?).await?),
        "revoke_reaction" => {
            db::scoped(revoke_reaction(a.str("reactionId")?)).await?;
            Value::Null
        }
        "get_message_delete_options" => {
            let id = a.str("messageId")?;
            to_value(resolve_delete_options(std::slice::from_ref(&id)).await.remove(&id).unwrap_or_default())?
        }
        "get_message_delete_meta_bulk" => {
            let resolved = resolve_delete_options(&a.de::<Vec<String>>("messageIds")?).await;
            let meta: HashMap<String, Value> = resolved
                .into_iter()
                .map(|(id, o)| (id, json!({ "has_retained_keys": o.has_retained_keys, "can_admin_hide": o.can_admin_hide })))
                .collect();
            to_value(meta)?
        }
        "mark_as_unread" => db::scoped(mark_as_unread(a.str("chatId")?)).await.map_or(Value::Null, Value::String),
        "mark_unread_from" => json!(db::scoped(mark_unread_from(a.str("chatId")?, a.str("messageId")?)).await?),
        "set_self_destruct_timer" => {
            let (chat_id, secs) = (a.str("chatId")?, a.de::<Option<u64>>("secs")?);
            db::scoped(async move { vector_core::self_destruct::set_chat_duration_secs(&chat_id, secs) }).await?;
            Value::Null
        }
        "get_messages_around_id" => {
            to_value(get_messages_around_id(&a.str("chatId")?, &a.str("targetMessageId")?, a.usize("contextBefore")?).await?)?
        }
        "fetch_msg_metadata" => json!(fetch_msg_metadata(a.str("chatId")?, a.str("msgId")?).await),
        "cancel_download" => json!(crate::attachments::cancel_download(&a.str("attachmentId")?)),
        "verify_remote_media" => verify_remote_media(a.de("urls")?).await,
        "bump_emoji_usage" => {
            if db::get_current_account().is_ok() {
                let url = a.opt_str("url");
                vector_core::emoji_usage::bump(&a.str("kind")?, &a.str("id")?, url.as_deref())?;
            }
            Value::Null
        }
        "gif_api" => Value::String(gif_api(&a.str("query")?).await?),
        "cache_gif_preview" => cache_gif_preview(&a.str("url")?).await?.map_or(Value::Null, Value::String),
        "fetch_nostr_embed" => to_value(vector_core::nostr_embed::fetch(&a.str("reference")?).await?)?,
        "cache_embed_video" => {
            let fallbacks: Option<Vec<String>> = a.de("fallbacks")?;
            let sha256 = a.opt_str("sha256");
            Value::String(cache_embed_video(&a.str("url")?, sha256.as_deref(), fallbacks.unwrap_or_default()).await?)
        }
        "cancel_embed_video" => json!(false),
        "embed_video_preview" => Value::String(embed_video_preview(&a.str("url")?, a.opt_str("sha256").as_deref()).await?),
        _ => return Ok(None),
    }))
}

fn is_dm(chat_id: &str) -> bool {
    chat_id.starts_with("npub1")
}

fn emit_removed(id: &str, chat_id: &str) {
    vector_core::emit_event("message_removed", &json!({ "id": id, "chat_id": chat_id, "reason": "deleted" }));
}

async fn react_to_message(a: &Args) -> Result<Value, String> {
    let (reference, chat_id, emoji) = (a.str("referenceId")?, a.str("chatId")?, a.str("emoji")?);
    if !is_dm(&chat_id) {
        return Err("Reactions in Community channels are not yet supported".into());
    }
    let url = a.opt_str("emojiUrl");
    core(VectorCore.send_reaction(&chat_id, &reference, &emoji, url.as_deref()).await.map(|_| true))
}

async fn edit_message(a: &Args) -> Result<Value, String> {
    let (message_id, chat_id, content) = (a.str("messageId")?, a.str("chatId")?, a.str("newContent")?);
    if !is_dm(&chat_id) {
        cm::community_control(&chat_id, &message_id, ControlOp::Edit(&content)).await?;
        return Ok(json!(""));
    }
    core(VectorCore.edit_dm(&chat_id, &message_id, &content).await)
}

async fn reconcile_unread(chat_id: &str) {
    let count = db::events::unread_count_for_chat(chat_id).await.unwrap_or(0);
    let mut state = STATE.lock().await;
    if state.unread_seeded {
        state.unread_set(chat_id, count);
    }
}

async fn delete_own_message(message_id: String) -> Result<Value, String> {
    let (chat_id, msg) = VectorCore.get_message(&message_id).await.ok_or_else(|| format!("Message not found (id: {message_id})"))?;
    if !msg.mine {
        return Err("Cannot delete a message that isn't yours".into());
    }
    if !is_dm(&chat_id) {
        return Err("Community channel messages are deleted via the Community service, not this path".into());
    }
    let rumor_id = EventId::from_hex(&message_id).map_err(|e| format!("Invalid message id: {e}"))?;
    let outcome = vector_core::delete_own_dm(&rumor_id).await?;

    STATE.lock().await.remove_message(&message_id);
    // Before the row delete: an unflushed self-echo consults this and drops itself.
    vector_core::state::note_message_deleted(&message_id);
    let _ = db::events::delete_event(&message_id).await;
    emit_removed(&message_id, &chat_id);
    reconcile_unread(&chat_id).await;
    to_value(outcome)
}

async fn delete_failed_message(message_id: String) -> Result<(), String> {
    let removed = {
        let mut state = STATE.lock().await;
        match state.find_message(&message_id) {
            Some((_, m)) if m.failed => state.remove_message(&message_id),
            Some(_) => return Err("Message is not failed or does not exist".into()),
            None => None,
        }
    };
    let (chat_id, msg) = match removed {
        Some(found) => found,
        None => match VectorCore.get_message(&message_id).await {
            Some((c, m)) if m.failed => (c, m),
            _ => return Err("Message is not failed or does not exist".into()),
        },
    };

    // Web blobs are content-addressed and shared, so only an unshared copy in the download dir goes.
    let download_dir = db::get_download_dir();
    for att in &msg.attachments {
        if att.path.is_empty() || !Path::new(&att.path).starts_with(&download_dir) {
            continue;
        }
        if db::attachments::hash_referenced_elsewhere(&att.id, &message_id) == Ok(false) {
            vector_core::webfiles::remove(Path::new(&att.path)).await;
        }
    }
    let urls: Vec<String> = msg.attachments.iter().flat_map(|a| a.all_urls().map(str::to_string)).collect();
    let urls = db::attachments::urls_unreferenced_elsewhere(&urls, &message_id).unwrap_or_default();
    if !urls.is_empty() {
        if let Ok(signer) = vector_core::signer::active_signer() {
            vector_core::blossom::delete_blobs_best_effort(signer, urls);
        }
    }
    let _ = db::events::delete_event(&message_id).await;
    emit_removed(&message_id, &chat_id);
    Ok(())
}

async fn retry_failed_dm(receiver: String, message_id: String) -> Result<bool, String> {
    let failed = VectorCore.get_message(&message_id).await.is_some_and(|(_, m)| m.failed);
    if !failed {
        return Err("Message is not failed or does not exist".into());
    }
    let callback: Arc<dyn SendCallback> = Arc::new(WebSendCallback);
    vector_core::sending::resend_failed_dm(&receiver, &message_id, &SendConfig::gui(), callback).await
}

async fn revoke_reaction(reaction_id: String) -> Result<(), String> {
    let found = STATE.lock().await.find_reaction(&reaction_id);
    let (chat_id, message_id, author, is_community) = found.ok_or_else(|| format!("Reaction not found (id: {reaction_id})"))?;
    if author != db::get_current_account()? {
        return Err("Cannot revoke a reaction that isn't yours".into());
    }

    let updated = STATE.lock().await.remove_reaction_from_message(&message_id, &reaction_id);
    let _ = db::events::delete_event(&reaction_id).await;
    if let Some((_, message)) = updated {
        vector_core::emit_event("message_update", &json!({ "old_id": message_id, "message": message, "chat_id": chat_id }));
    }

    if is_community {
        if db::community::get_message_key(&reaction_id).is_ok_and(|k| k.is_some()) {
            let transport = vector_core::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            if let Err(e) = vector_core::community::service::delete_message(&transport, &reaction_id).await {
                vector_core::log_warn!("[Web] community reaction relay delete failed, tombstone only: {e}");
            }
        }
        cm::community_control(&chat_id, &reaction_id, ControlOp::Delete).await
    } else {
        let rid = EventId::from_hex(&reaction_id).map_err(|e| format!("Invalid reaction id: {e}"))?;
        let recipient = PublicKey::parse(&chat_id).map_err(|e| format!("Invalid DM counterpart: {e}"))?;
        vector_core::deletion::delete_own_reaction(&rid, recipient).await.map(|_| ())
    }
}

#[derive(serde::Serialize, Default)]
struct MessageDeleteOptions {
    mine: bool,
    has_retained_keys: bool,
    has_attachments: bool,
    can_admin_hide: bool,
}

type ModerationContext = (Option<String>, vector_core::community::roles::CommunityRoles);

fn moderation_context_for_channel(chat_id: &str) -> Result<Option<ModerationContext>, ()> {
    let Some(cid) = db::community::community_id_for_channel(chat_id).map_err(|_| ())? else {
        return Ok(None);
    };
    let roster = db::community::get_community_roles(&cid).map_err(|_| ())?;
    Ok(Some((vector_core::community::moderation::owner_hex(&cid), roster)))
}

/// Ids that can't be resolved confidently are omitted, so the caller re-probes
/// instead of caching a false verdict.
async fn resolve_delete_options(message_ids: &[String]) -> HashMap<String, MessageDeleteOptions> {
    struct Ctx {
        chat_type: Option<ChatType>,
        chat_id: String,
        mine: bool,
        author: Option<String>,
        has_attachments: bool,
    }
    let mut ctxs: HashMap<String, Ctx> = {
        let state = STATE.lock().await;
        message_ids
            .iter()
            .filter_map(|id| {
                state.find_message(id).map(|(chat, msg)| {
                    let ctx = Ctx {
                        chat_type: Some(chat.chat_type.clone()),
                        chat_id: chat.id.clone(),
                        mine: msg.mine,
                        author: msg.npub.clone(),
                        has_attachments: msg.attachments.iter().any(|a| !a.url.is_empty()),
                    };
                    (id.clone(), ctx)
                })
            })
            .collect()
    };
    for id in message_ids {
        if ctxs.contains_key(id) {
            continue;
        }
        if let Ok(Some((chat_id, mine, author))) = db::events::event_delete_context(id) {
            ctxs.insert(id.clone(), Ctx { chat_type: None, chat_id, mine, author, has_attachments: false });
        }
    }

    let me = vector_core::state::my_public_key();
    let mut communities: HashMap<String, Result<Option<ModerationContext>, ()>> = HashMap::new();
    let mut out = HashMap::with_capacity(ctxs.len());
    for (id, ctx) in ctxs {
        let maybe_community = !matches!(ctx.chat_type, Some(ChatType::DirectMessage));
        let can_admin_hide = match (ctx.mine, maybe_community, ctx.author.as_deref(), &me) {
            (false, true, Some(author), Some(me)) => {
                match communities.entry(ctx.chat_id.clone()).or_insert_with(|| moderation_context_for_channel(&ctx.chat_id)) {
                    Err(()) => continue,
                    Ok(None) => false,
                    Ok(Some((owner, roster))) => {
                        vector_core::community::moderation::can_hide(owner.as_deref(), roster, &me.to_hex(), author)
                    }
                }
            }
            _ => false,
        };

        let has_retained_keys = if ctx.mine {
            let dm_keys = || match EventId::from_hex(&id) {
                Ok(rid) => db::nip17_keys::has_wrap_keys_for_rumor(&rid),
                Err(_) => Ok(false),
            };
            let community_keys = || db::community::get_message_key(&id).map(|k| k.is_some());
            let checked = match ctx.chat_type {
                Some(ChatType::DirectMessage) => dm_keys(),
                Some(ChatType::Community) => community_keys(),
                None => match (community_keys(), dm_keys()) {
                    (Ok(true), _) | (_, Ok(true)) => Ok(true),
                    (Ok(false), Ok(false)) => Ok(false),
                    (Err(e), _) | (_, Err(e)) => Err(e),
                },
            };
            match checked {
                Ok(b) => b,
                Err(_) => continue,
            }
        } else {
            false
        };

        out.insert(id, MessageDeleteOptions { mine: ctx.mine, has_retained_keys, has_attachments: ctx.has_attachments, can_admin_hide });
    }
    out
}

async fn mark_as_unread(chat_id: String) -> Option<String> {
    use db::events::{compute_unread_anchor, UnreadMark};
    let (last_read, last_read_hex, is_clear) = match compute_unread_anchor(&chat_id).await {
        Ok(UnreadMark::Anchor(id)) => (vector_core::compact::encode_message_id(&id), id, false),
        Ok(UnreadMark::Clear) => ([0u8; 32], String::new(), true),
        Ok(UnreadMark::NoOp) | Err(_) => return None,
    };
    let slim = {
        let mut state = STATE.lock().await;
        let idx = state.chats.iter().position(|c| c.id == chat_id)?;
        state.chats[idx].mark_read_at(last_read);
        db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner)
    };
    let _ = db::chats::save_slim_chat(&slim);
    // The upsert refuses empty markers, so a clear takes its own path.
    if is_clear {
        let _ = db::chats::clear_chat_last_read(&chat_id);
    }
    reconcile_unread(&chat_id).await;
    Some(last_read_hex)
}

/// Unread from `message_id` on, past our own replies, until the chat is read again; the new count.
async fn mark_unread_from(chat_id: String, message_id: String) -> Result<u32, String> {
    let slim = {
        let mut state = STATE.lock().await;
        let idx = state.chats.iter().position(|c| c.id == chat_id).ok_or("Chat not found")?;
        state.chats[idx].unread_from = vector_core::compact::encode_message_id(&message_id);
        db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner)
    };
    db::chats::save_slim_chat(&slim)?;
    reconcile_unread(&chat_id).await;
    db::events::unread_count_for_chat(&chat_id).await
}

/// `context_before` messages older than the target, the target, and everything newer; newest first.
async fn get_messages_around_id(chat_id: &str, target: &str, context_before: usize) -> Result<Vec<Message>, String> {
    let chat = db::id_cache::get_chat_id_by_identifier(chat_id)?;
    let newer = db::events::get_chat_message_count(chat)?;
    let mut window = db::events::get_messages_around(chat, target, context_before.saturating_add(1), newer).await?;
    window.reverse();
    Ok(window)
}

const INVITE_PREFIXES: [&str; 2] = ["https://vectorapp.io/invite", "https://www.vectorapp.io/invite"];

fn extract_https_urls(text: &str) -> Vec<String> {
    const DELIMITERS: &[u8] = b" \t\n\r\"<>)]}|";
    let mut urls = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("https://") {
        let start = from + i;
        let rest = text[start..].as_bytes();
        let mut end = rest.iter().position(|b| DELIMITERS.contains(b)).unwrap_or(rest.len());
        while end > 0 && matches!(rest[end - 1], b'.' | b',' | b':' | b';') {
            end -= 1;
        }
        if end > "https://".len() {
            urls.push(text[start..start + end].to_string());
        }
        from = start + 1;
    }
    urls
}

/// Only through a Magnitude server: a direct fetch would hand the page this device's address.
async fn site_metadata(url: &str) -> Result<SiteMetadata, String> {
    use vector_core::proxy;
    if !proxy::enabled() {
        return Err("link previews need the privacy proxy on Vector Web".into());
    }
    let server = proxy::server_offering(proxy::UNFURL_EXT).await.ok_or("no configured server offers link previews")?;
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(20))?;
    let response = client
        .get(format!("{server}/unfurl"))
        .query(&[("url", url)])
        .send()
        .await
        .map_err(|e| format!("unfurl request failed: {e}"))?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|e| format!("unfurl answer unreadable: {e}"))?;
    if !status.is_success() {
        return Err(format!("unfurl refused: {status}"));
    }
    let proxy_server = proxy::server_offering(proxy::PROXY_EXT).await;

    let p = body.get("preview").cloned().unwrap_or(Value::Null);
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
    let final_url = body.get("url").and_then(Value::as_str).unwrap_or(url).to_string();
    let domain = reqwest::Url::parse(&final_url)
        .map(|u| format!("{}/", u.origin().ascii_serialization()))
        .unwrap_or_else(|_| final_url.clone());
    // No proxy: drop the picture rather than load it from this device.
    let via_proxy = |u: Option<String>| -> Option<String> {
        let u = u?;
        match proxy_server.as_deref() {
            Some(srv) if proxy::wants_proxy(&u) => Some(proxy::proxy_url(srv, &u)),
            Some(_) => Some(u),
            None => None,
        }
    };
    let image = p.get("image").and_then(|i| i.get("url")).and_then(Value::as_str).map(str::to_string);
    Ok(SiteMetadata {
        domain,
        og_title: s("title"),
        og_description: s("description"),
        og_image: via_proxy(image),
        og_url: s("canonical").or(Some(final_url)),
        og_type: s("kind"),
        title: s("title"),
        description: s("description"),
        favicon: via_proxy(s("favicon")),
    })
}

async fn fetch_msg_metadata(chat_id: String, msg_id: String) -> bool {
    let Some((_, message)) = VectorCore.get_message(&msg_id).await else { return false };
    let urls = extract_https_urls(&vector_core::net::strip_md_link_claims(&message.content));
    for url in urls.into_iter().take(3) {
        if INVITE_PREFIXES.iter().any(|p| url.starts_with(p)) || vector_core::golink::is_go_url(&url) {
            continue;
        }
        // A link to a Nostr post, article or video gets its own card from relays.
        if vector_core::nostr_embed::EmbedRef::from_url(&url).is_some() {
            continue;
        }
        let Ok(metadata) = site_metadata(&url).await else { continue };
        let has_content = metadata.og_title.is_some()
            || metadata.og_description.is_some()
            || metadata.og_image.is_some()
            || metadata.title.is_some()
            || metadata.description.is_some();
        if !has_content {
            continue;
        }
        let resident = STATE.lock().await.update_message_in_chat(&chat_id, &msg_id, |m| {
            m.set_preview_metadata(Some(metadata.clone()));
        });
        let msg = resident.unwrap_or_else(|| Message { preview_metadata: Some(metadata), ..message.clone() });
        vector_core::emit_event("message_update", &json!({ "old_id": msg_id, "message": msg, "chat_id": chat_id }));
        let _ = db::events::save_message(&chat_id, &msg).await;
        return true;
    }
    false
}

/// `gone` only on a definitive 404/410: a transient failure must never flag healthy media.
async fn verify_remote_media(urls: Vec<String>) -> Value {
    use futures_util::StreamExt;
    let mut deduped: Vec<String> = Vec::new();
    for u in urls {
        if deduped.len() >= 64 {
            break;
        }
        if !deduped.contains(&u) {
            deduped.push(u);
        }
    }
    let results: Vec<Value> = futures_util::stream::iter(deduped.into_iter().map(|url| async move {
        let gone = vector_core::net::validate_url_not_private(&url).is_ok()
            && matches!(vector_core::net::remote_status(&url, std::time::Duration::from_secs(10)).await, Some(404 | 410));
        json!({ "url": url, "gone": gone })
    }))
    .buffer_unordered(6)
    .collect()
    .await;
    Value::Array(results)
}

const GIF_SERVICE: &str = "https://gifverse.net";

/// `None` on a non-success status or a body past `cap`.
/// `fresh` skips the browser's HTTP cache, for answers the caller keeps itself.
async fn fetch_capped(url: &str, cap: usize, fresh: bool) -> Result<Option<Vec<u8>>, String> {
    use futures_util::StreamExt;
    let client = vector_core::net::shared_http_client();
    let mut req = vector_core::net::proxied_request(&client, reqwest::Method::GET, url).await;
    if fresh {
        req = req.fetch_cache_no_store();
    }
    let resp = req
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        if out.len() > cap {
            return Ok(None);
        }
    }
    Ok(Some(out))
}

/// Held in the worker's memory while it downloads, so smaller than the desktop's cap.
const MAX_EMBED_VIDEO_BYTES: usize = 128 * 1024 * 1024;

/// An embedded Nostr video as a file in OPFS, kept only if its bytes are a video and,
/// when the event names one, its hash matches.
async fn cache_embed_video(url: &str, sha256: Option<&str>, fallbacks: Vec<String>) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("Not a web video".into());
    }
    // Reused only for the same link and the same promised hash.
    let stem = &vector_core::crypto::sha256_hex(format!("{url}\n{}", sha256.unwrap_or("").to_ascii_lowercase()).as_bytes())[..32];
    for ext in ["mp4", "webm", "mov"] {
        let path = format!("/cache/embed_videos/{stem}.{ext}");
        if vector_core::webfiles::exists(Path::new(&path)).await {
            return Ok(path);
        }
    }
    vector_core::emit_event("embed_video_progress", &json!({ "url": url, "progress": -1 }));
    let mut last_err = "Failed to download".to_string();
    let mirrors = fallbacks.into_iter().filter(|u| u.starts_with("https://")).take(3);
    for source in std::iter::once(url.to_string()).chain(mirrors) {
        if vector_core::net::validate_url_not_private(&source).is_err() {
            continue;
        }
        let body = match fetch_capped(&source, MAX_EMBED_VIDEO_BYTES, true).await {
            Ok(Some(body)) => body,
            Ok(None) => {
                last_err = "This video is too large or unavailable".into();
                continue;
            }
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let ext = if body.len() > 12 && &body[4..8] == b"ftyp" {
            if &body[8..12] == b"qt  " { "mov" } else { "mp4" }
        } else if body.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
            "webm"
        } else {
            return Err("Not a video".into());
        };
        if sha256.is_some_and(|h| !vector_core::crypto::sha256_hex(&body).eq_ignore_ascii_case(h)) {
            return Err("The video doesn't match the post's fingerprint".into());
        }
        let path = format!("/cache/embed_videos/{stem}.{ext}");
        vector_core::webfiles::write(Path::new(&path), &body).await?;
        vector_core::emit_event("embed_video_progress", &json!({ "url": url, "progress": 100 }));
        return Ok(path);
    }
    Err(last_err)
}

/// The opening of an embedded video, enough to show its first frame, fetched as byte ranges.
/// The whole file, when it is already here, does as well.
async fn embed_video_preview(url: &str, sha256: Option<&str>) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("Not a web video".into());
    }
    let stem = &vector_core::crypto::sha256_hex(format!("{url}\n{}", sha256.unwrap_or("").to_ascii_lowercase()).as_bytes())[..32];
    for name in [stem.to_string(), format!("{stem}-poster")] {
        for ext in ["mp4", "webm", "mov"] {
            let path = format!("/cache/embed_videos/{name}.{ext}");
            if vector_core::webfiles::exists(Path::new(&path)).await {
                return Ok(path);
            }
        }
    }
    let poster = vector_core::video_poster::fetch(url).await?;
    let path = format!("/cache/embed_videos/{stem}-poster.{}", poster.ext);
    vector_core::webfiles::write(Path::new(&path), &poster.bytes).await?;
    Ok(path)
}

/// The path is fixed here; the caller only picks the query.
async fn gif_api(query: &str) -> Result<String, String> {
    let (path, _) = query.split_once('?').unwrap_or((query, ""));
    if !matches!(path, "trending" | "search") || query.contains('/') || query.contains("..") || query.contains('#') {
        return Err("unknown GIF query".into());
    }
    let body = fetch_capped(&format!("{GIF_SERVICE}/api/v1/{query}"), 1024 * 1024, false)
        .await
        .map_err(|e| format!("gif service: {e}"))?
        .ok_or("gif service: no usable answer")?;
    String::from_utf8(body).map_err(|_| "gif service: answer is not text".into())
}

async fn cache_gif_preview(url: &str) -> Result<Option<String>, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "bad preview url")?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("gifverse.net") || !parsed.path().starts_with("/media/") {
        return Err("not a GIF preview".into());
    }
    let ext = match parsed.path().rsplit('.').next() {
        Some("av1" | "mp4") => "mp4",
        Some("webm") => "webm",
        Some("gif") => "gif",
        _ => return Err("not a GIF preview".into()),
    };
    let path = format!("/cache/gif_previews/{}.{ext}", vector_core::crypto::sha256_hex(url.as_bytes()));
    if vector_core::webfiles::exists(Path::new(&path)).await {
        return Ok(Some(path));
    }
    // Kept in OPFS, so the browser's cache adds nothing, and an entry of it stored
    // without CORS headers would refuse this cross-origin read.
    let Some(body) = fetch_capped(url, 8 * 1024 * 1024, true).await.map_err(|e| format!("preview: {e}"))? else {
        return Ok(None);
    };
    // Judged by its bytes, never by what the service said.
    let looks_right = match ext {
        "mp4" => body.len() > 12 && &body[4..8] == b"ftyp",
        "webm" => body.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]),
        _ => body.starts_with(b"GIF8"),
    };
    if !looks_right {
        return Ok(None);
    }
    vector_core::webfiles::write(Path::new(&path), &body).await?;
    Ok(Some(path))
}
