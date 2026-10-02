//! Community administration: moderation, roles, channel and community lifecycle, images.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::community::transport::LiveTransport;
use vector_core::community::{ChannelId, CommunityId, CommunityImage};
use vector_core::simd::hex::bytes_to_hex_32;
use vector_core::{db, VectorCore, STATE};

use crate::commands::{core, Args};
use crate::community::{id32, is_v2, load_v2};

const LEGACY: &str = "Legacy communities are read-only on Vector Web";

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        Some(match cmd {
            // --- Moderation ---
            "hide_community_message" => hide_message(a).await,
            "kick_community_member" => kick_member(a).await,
            "kick_community_members" => kick_members(a).await,
            "ban_community_member" | "unban_community_member" => ban_member(a, cmd == "ban_community_member").await,
            "ban_community_members" | "unban_community_members" => ban_members(a, cmd == "ban_community_members").await,
            "refound_community" => refound(a).await,
            "revoke_all_public_invites" => revoke_all_invites(a).await,
            "get_moderation_intel" => with_id(a, |id| core(VectorCore.community_moderation_intel(id))),
            "policy_presets" => core(VectorCore.policy_presets()),
            "builtin_policy_json" => core(VectorCore.builtin_policy_json()),
            "list_community_policies" => with_id(a, |id| core(VectorCore.list_community_policies(id))),
            "policy_side_by_side" => with_id(a, |id| core(VectorCore.policy_side_by_side(id))),
            "preview_community_policy" => {
                a.str("bytes").and_then(|bytes| with_id(a, |id| core(VectorCore.preview_community_policy(id, &bytes))))
            }
            "set_community_policy" => set_policy(a),
            "delete_community_policy" => {
                a.str("policyId").and_then(|pid| with_id(a, |id| core(VectorCore.delete_community_policy(id, &pid))))
            }

            // --- Lifecycle ---
            "delete_community" => delete_community(a).await,
            "delete_community_channel" => delete_channel(a).await,
            "migration_status" => migration_status(a),
            "migrate_community" => migrate(a).await,

            // --- Roles ---
            "create_community_role" => create_role(a).await,
            "edit_community_role" => edit_role(a).await,

            // --- Images & pins ---
            "set_community_image" => set_community_image(a).await,
            "fetch_pinned_attachment" => fetch_pinned_attachment(a).await,
            _ => return None,
        })
    })
}

fn with_id(a: &Args, f: impl FnOnce(&str) -> Result<Value, String>) -> Result<Value, String> {
    f(&a.str("communityId")?)
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn channel_ids(community_id: &str) -> Vec<String> {
    load_v2(community_id).map(|c| c.channels.iter().map(|ch| bytes_to_hex_32(&ch.id.0)).collect()).unwrap_or_default()
}

async fn refresh_subscription() {
    if let Some(client) = vector_core::state::nostr_client() {
        vector_core::community::v2::realtime::refresh_subscription(&client).await;
    }
}

/// A rotation moves every plane pseudonym; the live sub must follow it.
async fn refresh_if_live() {
    if db::session_is_live() {
        refresh_subscription().await;
    }
}

/// A ban hides messages retroactively, which the incremental unread cache can't derive.
async fn reconcile_unread(community_id: &str) {
    for ch in channel_ids(community_id) {
        let count = db::events::unread_count_for_chat(&ch).await.unwrap_or(0);
        let mut state = STATE.lock().await;
        if state.unread_seeded {
            state.unread_set(&ch, count);
        }
    }
}

fn purge_progress(community_id: String) -> impl Fn(usize, usize) + Sync {
    move |done, total| {
        vector_core::emit_event("community_purge_progress", &json!({ "community_id": community_id, "done": done, "total": total }));
    }
}

async fn hide_message(a: &Args) -> Result<Value, String> {
    let (channel, message) = (a.str("channelId")?, a.str("messageId")?);
    VectorCore.hide_community_message(&channel, &message).await.map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

async fn kick_member(a: &Args) -> Result<Value, String> {
    let (id, npub) = (a.str("communityId")?, a.str("npub")?);
    if !is_v2(&id) {
        return Err(LEGACY.into());
    }
    db::scoped(async move { VectorCore.kick_member(&id, &npub).await.map_err(|e| e.to_string()) }).await?;
    Ok(Value::Null)
}

async fn kick_members(a: &Args) -> Result<Value, String> {
    let (id, npubs) = (a.str("communityId")?, a.de::<Vec<String>>("npubs")?);
    db::scoped(async move {
        let refs: Vec<&str> = npubs.iter().map(String::as_str).collect();
        let summary = VectorCore
            .kick_community_members(&id, &refs, &purge_progress(id.clone()))
            .await
            .map_err(|e| e.to_string())?;
        VectorCore::invalidate_raid_report(&id);
        Ok(summary)
    })
    .await
}

async fn ban_member(a: &Args, banned: bool) -> Result<Value, String> {
    let (id, npub) = (a.str("communityId")?, a.str("npub")?);
    if !is_v2(&id) {
        return Err(LEGACY.into());
    }
    db::scoped(async move {
        VectorCore.set_member_banned(&id, &npub, banned).await.map_err(|e| e.to_string())?;
        reconcile_unread(&id).await;
        Ok(Value::Null)
    })
    .await
}

/// One banlist edition for the whole batch: per-member bans would each mint a rotation.
async fn ban_members(a: &Args, banned: bool) -> Result<Value, String> {
    let (id, npubs) = (a.str("communityId")?, a.de::<Vec<String>>("npubs")?);
    db::scoped(async move {
        let refs: Vec<&str> = npubs.iter().map(String::as_str).collect();
        VectorCore.set_members_banned(&id, &refs, banned).await.map_err(|e| e.to_string())?;
        if banned {
            VectorCore::invalidate_raid_report(&id);
        }
        reconcile_unread(&id).await;
        refresh_if_live().await;
        Ok(Value::Null)
    })
    .await
}

async fn refound(a: &Args) -> Result<Value, String> {
    let (id, retain) = (a.str("communityId")?, a.de::<Vec<String>>("retain")?);
    db::scoped(async move {
        let refs: Vec<&str> = retain.iter().map(String::as_str).collect();
        let summary = VectorCore
            .purge_and_refound(&id, &refs, &purge_progress(id.clone()))
            .await
            .map_err(|e| e.to_string())?;
        VectorCore::invalidate_raid_report(&id);
        refresh_if_live().await;
        Ok(summary)
    })
    .await
}

async fn revoke_all_invites(a: &Args) -> Result<Value, String> {
    let id = a.str("communityId")?;
    db::scoped(async move {
        let (revoked, failed) = VectorCore.revoke_all_public_invites(&id).await.map_err(|e| e.to_string())?;
        refresh_if_live().await;
        Ok(json!({ "revoked": revoked, "failed": failed }))
    })
    .await
}

fn set_policy(a: &Args) -> Result<Value, String> {
    let (pid, bytes, enabled) = (a.str("policyId")?, a.str("bytes")?, a.bool("enabled").unwrap_or(true));
    with_id(a, |id| core(VectorCore.set_community_policy(id, &pid, &bytes, enabled)))
}

/// Owner dissolution: publish the tombstone, then forget the community entirely.
async fn delete_community(a: &Args) -> Result<Value, String> {
    let id = a.str("communityId")?;
    db::scoped(async move {
        if !is_v2(&id) {
            return Err(LEGACY.to_string());
        }
        let channels = channel_ids(&id);
        let relays = load_v2(&id).map(|c| c.relays).unwrap_or_default();
        if !db::community::get_community_dissolved(&id).unwrap_or(false) {
            VectorCore.dissolve_community(&id).await.map_err(|e| e.to_string())?;
        }
        teardown_local(&id, &channels, &relays).await;
        Ok(Value::Null)
    })
    .await
}

/// Forget a community on this device, keeping its keys for any rejoin.
pub(crate) async fn teardown_local(id: &str, channels: &[String], relays: &[String]) {
    let _ = db::community::delete_community_retain_keys(id);
    refresh_subscription().await;
    STATE.lock().await.chats.retain(|c| !channels.contains(&c.id));
    for ch in channels {
        let _ = db::chats::delete_chat(ch);
        vector_core::community::cache::clear_channel_sync_state(ch);
    }
    vector_core::community::cache::abort_preload(id);
    vector_core::community::transport::prune_unneeded_community_relays(relays).await;
}

async fn delete_channel(a: &Args) -> Result<Value, String> {
    let (id, channel) = (a.str("communityId")?, a.str("channelId")?);
    db::scoped_result(async move {
        if !is_v2(&id) {
            return Err("Only Concord v2 communities support channel deletion".to_string());
        }
        let ch = ChannelId(id32(&channel)?);
        let community = load_v2(&id)?;
        if community.primary_channel().is_some_and(|p| p.id.0 == ch.0) {
            return Err("The community's first channel can't be deleted".to_string());
        }
        let name = community.channel(&ch).map(|c| c.name.clone()).ok_or("Channel not found in Community")?;
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        vector_core::community::v2::service::delete_channel(&transport, &community, &ch, &name).await?;
        if let Ok(c) = load_v2(&id) {
            VectorCore.register_v2_chats(&c, &db::current_session()).await;
        }
        Ok(Value::Null)
    })
    .await
}

fn migration_status(a: &Args) -> Result<Value, String> {
    use vector_core::community::migration;
    let id = a.str("communityId")?;
    let unlocked = migration::wizard_unlocked(now_secs());
    if is_v2(&id) {
        return Ok(json!({
            "unlock_at": migration::MIGRATION_UNLOCK_AT, "unlocked": unlocked,
            "eligible": false, "state": "not_applicable",
        }));
    }
    let community = db::community::load_community(&CommunityId(id32(&id)?))?;
    let is_owner = community.as_ref().is_some_and(vector_core::community::service::is_proven_owner);
    let migrated_to = db::community::get_migrated_to(&id).ok().flatten();
    let dissolved = db::community::get_community_dissolved(&id).unwrap_or(false);
    let phase = db::community::get_migration_ledger(&id).ok().flatten().map(|(_, p, _)| p).unwrap_or(0);
    let migrated = migrated_to.is_some();
    Ok(json!({
        "unlock_at": migration::MIGRATION_UNLOCK_AT,
        "unlocked": unlocked,
        "eligible": migration::migration_eligible(migrated, phase, dissolved, is_owner),
        "state": migration::migration_state(migrated, phase, dissolved, is_owner, unlocked),
        "phase": phase,
        "migrated_to": migrated_to,
    }))
}

async fn migrate(a: &Args) -> Result<Value, String> {
    let id = a.str("communityId")?;
    db::scoped(async move {
        if is_v2(&id) {
            return Err("this community is already on Concord v2".to_string());
        }
        let community = db::community::load_community(&CommunityId(id32(&id)?))?.ok_or("Community not found")?;
        let transport = LiveTransport::with_timeout(Duration::from_secs(20));
        vector_core::community::migration::migrate_community_to_v2(&transport, &community, now_secs()).await.map(Value::String)
    })
    .await
}

async fn create_role(a: &Args) -> Result<Value, String> {
    let (id, name, color, perms) = (a.str("communityId")?, a.str("name")?, a.de::<u32>("color")?, a.str("permissions")?);
    let channel = a.opt_str("channelId");
    db::scoped(async move { core(VectorCore.create_role(&id, &name, color, &perms, channel.as_deref()).await) }).await
}

async fn edit_role(a: &Args) -> Result<Value, String> {
    let (id, role, name) = (a.str("communityId")?, a.str("roleId")?, a.str("name")?);
    let (color, perms, channel) = (a.de::<u32>("color")?, a.str("permissions")?, a.opt_str("channelId"));
    db::scoped(async move { core(VectorCore.edit_role(&id, &role, &name, color, &perms, channel.as_deref()).await) }).await
}

#[derive(serde::Deserialize)]
struct CropRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// Metadata stripped and size capped before encrypting: every member downloads it.
fn prepare_image(bytes: Vec<u8>, is_banner: bool) -> Result<(Vec<u8>, String), String> {
    let (max_dim, budget, animated_budget) =
        if is_banner { (1500, 600 * 1024, 3 * 1024 * 1024) } else { (512, 256 * 1024, 2 * 1024 * 1024) };
    if vector_core::crypto::mime_from_magic_bytes(&bytes) == "image/gif" {
        if bytes.len() > animated_budget {
            return Err(format!(
                "Animated image is too large ({} KB, max {} KB); please use a smaller one or a static image",
                bytes.len() / 1024,
                animated_budget / 1024
            ));
        }
        return Ok((bytes, "gif".into()));
    }
    let img = crate::files::decode_upright(&bytes).map_err(|_| "Image couldn't be read (unsupported or corrupt file)".to_string())?;
    for dim in [max_dim, max_dim * 3 / 4, max_dim / 2] {
        let sized = if img.width() > dim || img.height() > dim {
            img.resize(dim, dim, image::imageops::FilterType::CatmullRom)
        } else {
            img.clone()
        };
        for quality in [85, 70, 55] {
            let (out, ext) = crate::files::encode(&sized, false, quality)?;
            if out.len() <= budget {
                return Ok((out, ext.into()));
            }
            if ext == "png" {
                break;
            }
        }
    }
    Err(format!("Image is too detailed to fit under {} KB; please pick a simpler or smaller image", budget / 1024))
}

async fn set_community_image(a: &Args) -> Result<Value, String> {
    let (id, filepath, is_banner) = (a.str("communityId")?, a.str("filepath")?, a.bool("isBanner").unwrap_or(false));
    let crop = a.de::<Option<CropRect>>("crop").ok().flatten().map(|c| [c.x, c.y, c.w, c.h]);
    db::scoped(async move {
        let session = db::current_session();
        if !is_v2(&id) {
            return Err(LEGACY.to_string());
        }
        let community = load_v2(&id)?;
        let raw = vector_core::webfiles::read(Path::new(&filepath)).await.map_err(|e| format!("read image: {e}"))?;
        if vector_core::svg::looks_like_svg(&raw) {
            return Err(vector_core::community::SVG_REFUSED.into());
        }
        let raw = match crop {
            Some([x, y, w, h]) => crate::emoji_ops::crop_image(&raw, x, y, w, h)?,
            None => raw,
        };
        let (bytes, ext) = prepare_image(raw, is_banner)?;
        let hash = vector_core::crypto::sha256_hex(&bytes);
        let params = vector_core::crypto::generate_encryption_params();
        let encrypted = vector_core::crypto::encrypt_data(&bytes, &params)?;

        let signer = vector_core::signer::active_signer().map_err(|e| format!("signer: {e}"))?;
        let servers = vector_core::state::get_blossom_servers();
        if servers.is_empty() {
            return Err("No Blossom servers configured.".to_string());
        }
        let cid = id.clone();
        let progress: vector_core::blossom::ProgressCallback = std::sync::Arc::new(move |pct, _| {
            vector_core::emit_event(
                "community_image_upload_progress",
                &json!({ "community_id": cid, "progress": pct.unwrap_or(0), "is_banner": is_banner }),
            );
            Ok(())
        });
        let url = vector_core::blossom::upload_blob_with_progress_and_failover(
            signer, servers, std::sync::Arc::new(encrypted), Some("application/octet-stream"), true, progress, None, None, None,
        )
        .await?
        .url;

        let mut extra = serde_json::Map::new();
        extra.insert("ext".into(), Value::String(ext));
        let img_ref = vector_core::community::v2::control::ImageRef { url, key: params.key, nonce: params.nonce, hash, extra };
        let mut meta = community.metadata();
        if is_banner {
            meta.banner = Some(img_ref.clone());
        } else {
            meta.icon = Some(img_ref.clone());
        }
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        vector_core::community::v2::service::edit_community_metadata(&transport, &community, &meta).await?;
        if let Some(updated) = vector_core::community::v2::service::persist_community_image(community.id(), img_ref, is_banner).await {
            VectorCore.register_v2_chats(&updated, &session).await;
        }
        Ok(Value::Null)
    })
    .await
}

/// A joined community's logo or banner as a local path, `null` when it has none.
pub async fn cache_community_image(a: &Args) -> Result<Value, String> {
    let (id, is_banner) = (a.str("communityId")?, a.bool("isBanner").unwrap_or(false));
    let image = if is_v2(&id) {
        let c = load_v2(&id)?;
        if is_banner { c.banner } else { c.icon }.map(|i| i.to_community_image())
    } else {
        let c = db::community::load_community(&CommunityId(id32(&id)?))?.ok_or("Community not found")?;
        if is_banner { c.banner } else { c.icon }
    };
    match image {
        Some(img) => Ok(json!(cache_encrypted_image(&img, if is_banner { MAX_BANNER } else { MAX_ICON }).await?)),
        None => Ok(Value::Null),
    }
}

/// An invite preview's logo, before the community is joined.
pub async fn cache_invite_logo(a: &Args) -> Result<Value, String> {
    let image: CommunityImage = a.de("image")?;
    Ok(json!(cache_encrypted_image(&image, MAX_ICON).await?))
}

const MAX_ICON: usize = 10 * 1024 * 1024;
/// Other clients upload banner crops uncompressed.
const MAX_BANNER: usize = 16 * 1024 * 1024;

/// Download, decrypt and verify against the committed plaintext hash; kept by that hash.
async fn cache_encrypted_image(image: &CommunityImage, max_bytes: usize) -> Result<String, String> {
    if image.hash.len() != 64 || !image.hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("community image has a malformed hash".into());
    }
    let ext = match image.ext.to_ascii_lowercase().as_str() {
        e @ ("png" | "jpg" | "jpeg" | "gif" | "webp") => e.to_string(),
        _ => "png".to_string(),
    };
    let path = format!("/cache/community/{}.{ext}", image.hash.to_ascii_lowercase());
    if vector_core::webfiles::exists(Path::new(&path)).await {
        return Ok(path);
    }
    vector_core::net::validate_url_not_private(&image.url).map_err(str::to_string)?;
    let encrypted = crate::images::fetch(&image.url, max_bytes).await.map_err(|e| format!("download: {e}"))?;
    let decrypted = vector_core::crypto::decrypt_data_owned(encrypted, &image.key, &image.nonce)?;
    if !vector_core::crypto::sha256_hex(&decrypted).eq_ignore_ascii_case(&image.hash) {
        return Err("community image failed integrity check".into());
    }
    if vector_core::svg::looks_like_svg(&decrypted) {
        return Err(vector_core::community::SVG_REFUSED.into());
    }
    vector_core::webfiles::write(Path::new(&path), &decrypted).await?;
    Ok(path)
}

/// A pinned attachment from the pin proof alone, so it opens with no chat history.
async fn fetch_pinned_attachment(a: &Args) -> Result<Value, String> {
    let (id, channel, rumor_id) = (a.str("communityId")?, a.str("channelId")?, a.str("messageId")?);
    db::scoped(async move {
        if !is_v2(&id) {
            return Err("pins are only available in Concord v2 communities".to_string());
        }
        let community = load_v2(&id)?;
        let pins = vector_core::community::v2::service::read_channel_pins(&community, &ChannelId(id32(&channel)?))?;
        let pin = pins.pins.iter().find(|p| p.rumor_id == rumor_id).ok_or("that message is not pinned")?;
        let tag = pin
            .tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some("imeta"))
            .map(|t| Tag::custom("imeta", t[1..].to_vec()))
            .ok_or("this pin carries no attachment")?;
        let attachment = vector_core::community::attachments::attachment_from_imeta(&tag, &db::get_download_dir())
            .ok_or("this pin's attachment metadata is malformed")?;
        let path = std::path::PathBuf::from(&*attachment.path);
        let respond = || {
            json!({ "path": path.to_string_lossy(), "name": attachment.name.to_string(), "extension": attachment.extension.to_string() })
        };

        let expected = attachment.original_hash.as_deref();
        if let Ok(bytes) = vector_core::webfiles::read(&path).await {
            if !bytes.is_empty() && expected.is_none_or(|want| vector_core::crypto::sha256_hex(&bytes) == want) {
                return Ok(respond());
            }
        }
        let author = PublicKey::from_hex(&pin.author).ok().and_then(|pk| pk.to_bech32().ok());
        let bytes = VectorCore.download_attachment_from(&attachment, author.as_deref()).await.map_err(|e| e.to_string())?;
        if expected.is_some_and(|want| vector_core::crypto::sha256_hex(&bytes) != want) {
            return Err("downloaded bytes do not match the pinned content hash".to_string());
        }
        vector_core::webfiles::write(&path, &bytes).await.map_err(|e| format!("could not cache the attachment: {e}"))?;
        Ok(respond())
    })
    .await
}
