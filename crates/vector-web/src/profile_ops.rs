//! Profile, contacts and account management: editing, blocking, nicknames, logout, keys, encryption changes.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use image::{DynamicImage, ImageDecoder, ImageReader};
use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::profile::sync as profile_sync;
use vector_core::synced_prefs::{self, IdList, NicknameMap, Pref};
use vector_core::tags::TagsExt;
use vector_core::{db, state, VectorCore, STATE};

use crate::commands::{to_value, Args};
use crate::images::{self, Kind as ImageKind};
use crate::sync::WebProfileSyncHandler;

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        Some(match cmd {
            // --- Profile ---
            "update_profile" => {
                let s = |k| a.opt_str(k).unwrap_or_default();
                Ok(json!(profile_sync::update_profile(s("name"), s("avatar"), s("banner"), s("about"), &WebProfileSyncHandler).await))
            }
            "update_status" => Ok(json!(profile_sync::update_status(a.opt_str("status").unwrap_or_default()).await)),
            "upload_avatar" => upload_avatar(a).await,
            "set_nickname" => set_nickname(a).await,
            "block_user" => set_blocked(a, true).await,
            "unblock_user" => set_blocked(a, false).await,
            "get_blocked_users" => to_value(profile_sync::get_blocked_users().await),

            // --- Accounts ---
            "logout" => logout().await.map(|_| Value::Null),
            "delete_account" => delete_account(a).await,
            "export_keys" => export_keys().await,
            "enter_add_account_mode" => {
                let _ = db::clear_active_account_file();
                reset_session().await;
                Ok(Value::Null)
            }
            "clear_active_account" => db::clear_active_account_file().map(|_| Value::Null),
            "verify_credential" => verify_credential(a).await,
            "disable_encryption" => crate::encryption::disable().map(|_| Value::Null),
            "enable_encryption" => crate::encryption::enable_cmd(a).await,
            "rekey_encryption" => crate::encryption::rekey_cmd(a).await,

            // --- Invites & badges ---
            "get_or_create_invite_code" => get_or_create_invite_code().await,
            "accept_invite_code" => accept_invite_code(a).await,
            "get_invited_users" => get_invited_users(a).await,
            "check_fawkes_badge" => check_fawkes_badge(a).await,
            "get_bug_hunter_tier" => get_bug_hunter_tier(a).await,

            // --- Storage ---
            "get_storage_info" => Ok(storage_info().await),
            "clear_storage" => clear_storage().await,
            "clear_storage_category" => clear_storage_category(a).await,

            _ => return None,
        })
    })
}

fn pubkey_arg(a: &Args, key: &str) -> Result<PublicKey, String> {
    PublicKey::from_bech32(&a.str(key)?).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// Republish a synced list from local state. Never over a list not yet reconciled with the relay copy.
fn publish_projection(pref: Pref) {
    db::spawn_bound(async move {
        let Some(client) = state::nostr_client() else { return };
        if !synced_prefs::is_hydrated(pref) {
            return;
        }
        let json = match pref {
            Pref::Blocks => {
                let mut list = IdList::default();
                for p in profile_sync::get_blocked_users().await {
                    let _ = list.add(&p.id);
                }
                list.to_json()
            }
            Pref::Nicknames => {
                let mut map = NicknameMap::default();
                let state = STATE.lock().await;
                for p in state.profiles.iter().filter(|p| !p.nickname().is_empty()) {
                    if let Some(npub) = state.interner.resolve(p.id) {
                        let _ = map.set(npub, p.nickname());
                    }
                }
                drop(state);
                map.to_json()
            }
            _ => return,
        };
        if let Err(e) = synced_prefs::publish_raw(&client, pref, &json).await {
            vector_core::log_warn!("[SyncedPrefs] publishing {} failed: {e}", pref.d_tag());
        }
    });
}

async fn set_nickname(a: &Args) -> Result<Value, String> {
    let ok = profile_sync::set_nickname(a.str("npub")?, a.opt_str("nickname").unwrap_or_default(), &WebProfileSyncHandler).await;
    if ok {
        publish_projection(Pref::Nicknames);
    }
    Ok(json!(ok))
}

async fn set_blocked(a: &Args, block: bool) -> Result<Value, String> {
    let npub = a.str("npub")?;
    let ok = if block {
        profile_sync::block_user(npub, &WebProfileSyncHandler).await
    } else {
        profile_sync::unblock_user(npub, &WebProfileSyncHandler).await
    };
    if ok {
        // A block moves other chats' counts too: the sender's community messages stop counting.
        let counts = db::events::unread_counts().await.unwrap_or_default();
        STATE.lock().await.unread_seed(counts);
        publish_projection(Pref::Blocks);
    }
    Ok(json!(ok))
}

/// `(max_dimension_px, static_byte_budget, animated_byte_budget)`, as desktop.
fn upload_budgets(banner: bool) -> (u32, usize, usize) {
    if banner { (1500, 600 * 1024, 3 * 1024 * 1024) } else { (512, 256 * 1024, 2 * 1024 * 1024) }
}

fn animated_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("gif");
    }
    let webp = bytes.len() >= 21 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP";
    let animated = webp
        && ((&bytes[12..16] == b"VP8X" && bytes[20] & 0x02 != 0) || bytes[..bytes.len().min(64)].windows(4).any(|w| w == b"ANIM"));
    animated.then_some("webp")
}

fn decode_upright(bytes: &[u8]) -> Result<DynamicImage, String> {
    const UNREADABLE: &str = "Image couldn't be read (unsupported or corrupt file)";
    let mut decoder = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| UNREADABLE.to_string())?
        .into_decoder()
        .map_err(|_| UNREADABLE.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|_| UNREADABLE.to_string())?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

fn fit_within(img: &DynamicImage, max_dim: u32) -> std::borrow::Cow<'_, DynamicImage> {
    if img.width() > max_dim || img.height() > max_dim {
        std::borrow::Cow::Owned(img.resize(max_dim, max_dim, image::imageops::FilterType::CatmullRom))
    } else {
        std::borrow::Cow::Borrowed(img)
    }
}

/// PNG when transparency is actually used, JPEG otherwise.
fn encode_auto(img: &DynamicImage, quality: u8) -> Result<(Vec<u8>, &'static str), String> {
    let mut out = Vec::new();
    if img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255) {
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
        return Ok((out, "png"));
    }
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&img.to_rgb8())
        .map_err(|e| e.to_string())?;
    Ok((out, "jpg"))
}

/// Strip metadata by re-encoding, fit the dimension cap, then step down until under the byte budget.
fn prepare_upload_image(bytes: Vec<u8>, banner: bool) -> Result<(Vec<u8>, &'static str), String> {
    let (max_dim, budget, animated_budget) = upload_budgets(banner);
    if let Some(ext) = animated_extension(&bytes) {
        // No animation re-encoder here: an animation within budget ships as-is.
        if bytes.len() > animated_budget {
            return Err(format!(
                "Animated image is too large ({} KB, max {} KB); please use a smaller one or a static image",
                bytes.len() / 1024,
                animated_budget / 1024,
            ));
        }
        return Ok((bytes, ext));
    }
    let img = decode_upright(&bytes)?;
    drop(bytes);
    let base = fit_within(&img, max_dim);
    let first = encode_auto(&base, 85)?;
    if first.0.len() <= budget {
        return Ok(first);
    }
    if first.1 == "jpg" {
        for quality in [70, 55] {
            let enc = encode_auto(&base, quality)?;
            if enc.0.len() <= budget {
                return Ok(enc);
            }
        }
    }
    let mut smallest = first.0.len();
    for dim in [max_dim * 3 / 4, max_dim / 2] {
        let enc = encode_auto(&fit_within(&img, dim.max(1)), 60)?;
        if enc.0.len() <= budget {
            return Ok(enc);
        }
        smallest = smallest.min(enc.0.len());
    }
    Err(format!(
        "Image is too detailed to fit under {} KB even after compression (smallest was {} KB); please pick a simpler or smaller image",
        budget / 1024,
        smallest / 1024,
    ))
}

fn upload_mime_for(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

async fn upload_avatar(a: &Args) -> Result<Value, String> {
    let filepath = a.str("filepath")?;
    let upload_type = a.opt_str("uploadType").unwrap_or_else(|| "avatar".into());
    let extension = filepath.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_else(|| "bin".into());
    vector_core::crypto::mime_from_extension_safe(&extension, true)
        .map_err(|_| "File type is not allowed for avatars (only images are permitted)")?;
    let bytes = vector_core::webfiles::read(Path::new(&filepath))
        .await
        .map_err(|_| "Image couldn't be loaded from disk")?;
    let banner = upload_type == "banner";
    let (prepared, ext) = prepare_upload_image(bytes, banner)?;
    let upload_bytes = Arc::new(prepared);

    state::nostr_client().ok_or("Not connected")?;
    let signer = vector_core::signer::active_signer().map_err(|e| format!("Signer unavailable: {e}"))?;
    let kind = upload_type.clone();
    let progress: vector_core::blossom::ProgressCallback = Arc::new(move |pct, sent| {
        vector_core::emit_event(
            "profile_upload_progress",
            &json!({ "type": kind, "progress": pct.unwrap_or(0), "bytes": sent.unwrap_or(0) }),
        );
        Ok(())
    });
    let url = vector_core::blossom::upload_blob_with_progress_and_failover(
        signer,
        state::get_blossom_servers(),
        upload_bytes.clone(),
        Some(upload_mime_for(ext)),
        false,
        progress,
        None,
        None,
        None,
    )
    .await?
    .url;

    images::precache(&url, if banner { ImageKind::Banner } else { ImageKind::Avatar }, &upload_bytes).await;
    // Point our own profile at the new image now, not when the kind-0 echoes back.
    if let Ok(me) = db::get_current_account() {
        if banner {
            images::cache_profile_images(&me, "", &url).await;
        } else {
            images::cache_profile_images(&me, &url, "").await;
        }
    }
    Ok(Value::String(url))
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

/// Return the worker to its fresh-boot state, as desktop's `reset_session`.
async fn reset_session() {
    VectorCore.swap_session().await;
    crate::account::clear_pending();
    db::clear_current_account_in_memory();
    state::set_encryption_enabled(false);
    state::open_processing_gate();
}

/// Logout deletes the active account's local data, then reloads.
async fn logout() -> Result<(), String> {
    let npub = db::get_current_account().map_err(|_| "Not logged in".to_string())?;
    let result = remove_account(npub).await;
    crate::emitter::emit("session_reload", &());
    result.map(|_| ())
}

async fn delete_account(a: &Args) -> Result<Value, String> {
    let was_active = remove_account(a.str("npub")?).await?;
    if was_active {
        crate::emitter::emit("session_reload", &());
    }
    Ok(json!(was_active))
}

/// Remove `npub`'s databases (main file, journal, WAL) from the OPFS pool.
async fn delete_account_databases(npub: &str) -> Result<(), String> {
    // Installing again only hands back the pool `start` registered.
    let cfg = sqlite_wasm_vfs::sahpool::OpfsSAHPoolCfgBuilder::new().directory("vector-web").build();
    let pool = sqlite_wasm_vfs::sahpool::install::<sqlite_wasm_rs::WasmOsCallback>(&cfg, true)
        .await
        .map_err(|e| format!("OPFS storage unavailable: {e:?}"))?;
    let marker = format!("/{npub}/");
    for name in pool.list().into_iter().filter(|n| n.contains(&marker)) {
        pool.delete_db(&name).map_err(|e| format!("Failed to remove {name}: {e:?}"))?;
    }
    Ok(())
}

/// Permanently delete an account. True when it was the active one.
async fn remove_account(npub: String) -> Result<bool, String> {
    if !db::get_accounts()?.iter().any(|a| a == &npub) {
        return Err(format!("Unknown account: {npub}"));
    }
    let was_active = matches!(db::get_current_account(), Ok(active) if active == npub);
    if was_active {
        let _ = db::clear_active_account_file();
        reset_session().await;
    } else if db::read_active_account_file()?.as_deref() == Some(npub.as_str()) {
        let _ = db::clear_active_account_file();
    }

    let app_data = db::get_app_data_dir()?.clone();
    let dir = db::account_dir(&npub)?;
    // Databases first: a registry row outliving its database is harmless, the reverse orphans a pool slot.
    // A connection the old session still holds errors on its next I/O; the page reloads after this.
    delete_account_databases(&npub).await?;
    db::webfs::remove_tree(&app_data, &dir)?;
    vector_core::webfiles::remove(&dir).await;
    // Originals the user picked still carry their metadata.
    vector_core::webfiles::remove(std::path::Path::new("/picked")).await;

    if db::list_account_npubs().is_ok_and(|v| v.is_empty()) {
        vector_core::webfiles::remove(&db::get_download_dir()).await;
        crate::images::clear_all().await;
    }
    Ok(was_active)
}

async fn export_keys() -> Result<Value, String> {
    if vector_core::is_keyless() {
        return Err("This is an external signer account. Your identity key lives on your signer app, never on this device, so there's nothing to export here.".into());
    }
    let stored = db::get_pkey()?.ok_or("No nsec found in database")?;
    let nsec = if state::is_encryption_enabled_fast() {
        vector_core::crypto::maybe_decrypt_inner(stored, None).await.map_err(|_| "Failed to decrypt nsec".to_string())?
    } else {
        stored
    };
    let pending_seed = state::MNEMONIC_SEED.lock().unwrap().clone();
    let seed_phrase = match pending_seed {
        Some(seed) => Some(seed),
        None => match db::get_seed()? {
            Some(stored) => {
                Some(vector_core::crypto::maybe_decrypt(stored).await.map_err(|_| "Failed to decrypt seed phrase".to_string())?)
            }
            None => None,
        },
    };
    Ok(json!({ "nsec": nsec, "seed_phrase": seed_phrase }))
}

async fn verify_credential(a: &Args) -> Result<Value, String> {
    let credential = zeroize::Zeroizing::new(a.str("credential")?);
    let key = zeroize::Zeroizing::new(vector_core::crypto::hash_pass(&credential).await);
    let stored = db::get_pkey()?.ok_or("No private key found — cannot verify credential.")?;
    match vector_core::crypto::decrypt_with_key(&stored, &key).map(zeroize::Zeroizing::new) {
        Ok(plain) if plain.starts_with("nsec") => Ok(Value::Null),
        _ => Err("Incorrect credential.".into()),
    }
}

// ---------------------------------------------------------------------------
// Invites & badges
// ---------------------------------------------------------------------------

fn generate_invite_code() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng().sample_iter(&Alphanumeric).take(8).map(char::from).collect::<String>().to_uppercase()
}

fn invite_code_of(event: &Event) -> Option<String> {
    if event.content != "vector_invite" {
        return None;
    }
    event.tags.find_kind("r").and_then(|t| t.content()).map(str::to_string)
}

/// Every event matching `filter` on the trusted relays, collected within the usual 10s window.
async fn fetch_trusted(client: &Client, filter: Filter, stop: impl Fn(&Event) -> bool) -> Result<Vec<Event>, String> {
    let relays = state::active_trusted_relays().await;
    let mut stream = client
        .stream_events(ReqTarget::manual(relays.into_iter().map(|u| (u, vec![filter.clone()]))))
        .timeout(std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    while let Some((_relay, res)) = stream.next().await {
        let Ok(event) = res else { continue };
        let done = stop(&event);
        events.push(event);
        if done {
            break;
        }
    }
    Ok(events)
}

async fn get_or_create_invite_code() -> Result<Value, String> {
    if let Ok(Some(code)) = db::get_sql_setting("invite_code".into()) {
        return Ok(Value::String(code));
    }
    let client = state::nostr_client().ok_or("Nostr client not initialized")?;
    let me = vector_core::my_public_key().ok_or("Public key not initialized")?;
    let filter = Filter::new()
        .author(me)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, "vector")
        .limit(100);
    let mut stream = client
        .stream_events(filter)
        .timeout(std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    while let Some((_relay, res)) = stream.next().await {
        if let Some(code) = res.ok().as_ref().and_then(invite_code_of) {
            db::set_sql_setting("invite_code".into(), code.clone())?;
            return Ok(Value::String(code));
        }
    }

    let code = generate_invite_code();
    let builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite")
        .tag(Tag::custom("d", vec!["vector"]))
        .tag(Tag::custom("r", vec![code.as_str()]));
    let event = vector_core::sign_builder(builder).await.map_err(|e| e.to_string())?;
    client.send_event(&event).to(state::active_trusted_relays().await).await.map_err(|e| e.to_string())?;
    db::set_sql_setting("invite_code".into(), code.clone())?;
    Ok(Value::String(code))
}

/// Resolve the inviter now; the acceptance is broadcast once account setup commits.
async fn accept_invite_code(a: &Args) -> Result<Value, String> {
    let invite_code = a.str("inviteCode")?;
    let client = state::nostr_client().ok_or("Nostr client not initialized")?;
    if invite_code.len() != 8 || !invite_code.chars().all(|c| c.is_alphanumeric()) {
        return Err("Invalid invite code format".into());
    }
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, "vector")
        .custom_tag(SingleLetterTag::LOWERCASE_R, &invite_code)
        .limit(1);
    let is_invite = |e: &Event| e.content == "vector_invite";
    let invite = fetch_trusted(&client, filter, is_invite).await?.into_iter().find(is_invite).ok_or("Invite code not found")?;
    let inviter_pubkey = invite.pubkey;
    if inviter_pubkey == vector_core::my_public_key().ok_or("Public key not initialized")? {
        return Err("Cannot accept your own invite code".into());
    }
    if state::pending_invite().is_none() {
        state::set_pending_invite(state::PendingInviteAcceptance { invite_code, inviter_pubkey });
    }
    Ok(Value::String(inviter_pubkey.to_bech32().map_err(|e| e.to_string())?))
}

async fn get_invited_users(a: &Args) -> Result<Value, String> {
    let inviter = pubkey_arg(a, "npub")?;
    let client = state::nostr_client().ok_or("Nostr client not initialized")?;
    let filter = Filter::new()
        .author(inviter)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, "vector")
        .limit(100);
    let code = fetch_trusted(&client, filter, |e| invite_code_of(e).is_some())
        .await?
        .iter()
        .find_map(invite_code_of)
        .ok_or("No invite code found for this user")?;

    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, code)
        .limit(1000);
    let acceptors: HashSet<PublicKey> = fetch_trusted(&client, filter, |_| false)
        .await?
        .into_iter()
        .filter(|e| e.content == "vector_invite_accepted" && e.tags.public_keys().any(|pk| pk == inviter))
        .map(|e| e.pubkey)
        .collect();
    Ok(json!(acceptors.len() as u32))
}

async fn check_fawkes_badge(a: &Args) -> Result<Value, String> {
    let pk = pubkey_arg(a, "npub")?;
    let has = vector_core::badges::has_fawkes_badge(&pk).await?;
    vector_core::badges::note_own_badge_confirmed(&pk, has);
    Ok(json!(has))
}

async fn get_bug_hunter_tier(a: &Args) -> Result<Value, String> {
    let (tier, _revoked) = vector_core::badges::fetch_bug_hunter_tier(&pubkey_arg(a, "npub")?).await?;
    Ok(json!(tier))
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// `(relative name, size)` of the files under a storage directory.
async fn list_files(dir: &Path, recursive: bool) -> Vec<(String, u64)> {
    vector_core::webfiles::tree(dir, recursive).await
}

/// The extension the storage chart buckets by; dotfiles are markers, never content.
fn bucket_of(name: &str) -> Option<String> {
    if name.starts_with('.') {
        return None;
    }
    name.rsplit('.').next().map(str::to_lowercase)
}

async fn storage_info() -> Value {
    let downloads = db::get_download_dir();
    let (mut total, mut count) = (0u64, 0u64);
    let mut distribution: HashMap<String, u64> = HashMap::new();
    for (name, size) in list_files(&downloads, false).await {
        let Some(ext) = bucket_of(&name) else { continue };
        total += size;
        count += 1;
        *distribution.entry(ext).or_insert(0) += size;
    }
    let cache: u64 = list_files(Path::new("/cache"), true).await.iter().map(|(_, s)| s).sum();
    if cache > 0 {
        distribution.insert("/cache".into(), cache);
        total += cache;
    }
    json!({
        "path": downloads.to_string_lossy(),
        "total_bytes": total,
        "file_count": count,
        "total_formatted": vector_core::crypto::format_bytes(total),
        "type_distribution": distribution,
    })
}

async fn total_bytes() -> u64 {
    storage_info().await["total_bytes"].as_u64().unwrap_or(0)
}

/// Delete downloaded attachments (all, or those with the given extensions) and reset their
/// metadata. Only files in the download dir are Vector's to delete. Returns the chats touched.
async fn clear_attachment_files(exts: Option<&HashSet<String>>) -> Result<usize, String> {
    let download_prefix = format!("{}/", db::get_download_dir().to_string_lossy().trim_end_matches('/'));
    let mut doomed: Vec<String> = Vec::new();
    let mut updates: Vec<(String, Vec<vector_core::Message>)> = Vec::new();
    {
        let mut state = STATE.lock().await;
        for chat_idx in 0..state.chats.len() {
            let mut touched = Vec::new();
            for message in state.chats[chat_idx].messages.iter_mut() {
                let mut changed = false;
                for attachment in &mut message.attachments {
                    if !attachment.path.starts_with(&download_prefix) {
                        continue;
                    }
                    if let Some(set) = exts {
                        let ext = attachment.path.rsplit('/').next().and_then(bucket_of);
                        if !ext.is_some_and(|e| set.contains(&e)) {
                            continue;
                        }
                    }
                    doomed.push(attachment.path.to_string());
                    attachment.set_downloaded(false);
                    attachment.set_downloading(false);
                    attachment.path = Box::default();
                    changed = true;
                }
                if changed {
                    touched.push(message.id);
                }
            }
            if touched.is_empty() {
                continue;
            }
            let chat = &state.chats[chat_idx];
            let messages = touched
                .iter()
                .filter_map(|id| chat.messages.find_by_hex_id(&vector_core::simd::hex::bytes_to_hex_32(id)))
                .map(|m| m.to_message(&state.interner))
                .collect();
            updates.push((chat.id().to_string(), messages));
        }
    }

    doomed.sort_unstable();
    doomed.dedup();
    for path in &doomed {
        vector_core::webfiles::remove(Path::new(path)).await;
    }
    for (chat_id, messages) in &updates {
        for message in messages {
            db::events::save_message(chat_id, message).await?;
            vector_core::emit_event("message_update", &json!({ "old_id": message.id, "message": message, "chat_id": chat_id }));
        }
    }
    Ok(updates.len())
}

/// Forget every cached picture, repaint the profiles that showed one, and fetch them again.
async fn clear_media_caches() {
    images::clear_all().await;
    let mut refetch = Vec::new();
    {
        let mut state = STATE.lock().await;
        let mut cleared = Vec::new();
        for profile in state.profiles.iter_mut() {
            if !profile.avatar_cached.is_empty() || !profile.banner_cached.is_empty() {
                profile.avatar_cached = Box::default();
                profile.banner_cached = Box::default();
                cleared.push(profile.id);
            }
        }
        for id in cleared {
            let Some(slim) = state.serialize_profile(id) else { continue };
            if let Some(p) = state.get_profile_by_id(id) {
                if !p.avatar.is_empty() || !p.banner.is_empty() {
                    refetch.push((slim.id.clone(), p.avatar.to_string(), p.banner.to_string()));
                }
            }
            let _ = db::profiles::set_profile(&slim);
            vector_core::emit_event("profile_update", &slim);
        }
    }
    if !refetch.is_empty() {
        db::spawn_bound(async move {
            for (npub, avatar, banner) in refetch {
                images::cache_profile_images(&npub, &avatar, &banner).await;
            }
        });
    }
}

async fn clear_storage() -> Result<Value, String> {
    let before = total_bytes().await;
    let updated_chats = clear_attachment_files(None).await?;
    clear_media_caches().await;
    let freed = before.saturating_sub(total_bytes().await);
    Ok(json!({
        "freed_bytes": freed,
        "freed_formatted": vector_core::crypto::format_bytes(freed),
        "updated_chats": updated_chats,
    }))
}

async fn clear_storage_category(a: &Args) -> Result<Value, String> {
    let before = total_bytes().await;
    match a.opt_str("category").unwrap_or_default().as_str() {
        "cache" => clear_media_caches().await,
        "ai" => {}
        _ => {
            let exts: Vec<String> = a.de::<Option<Vec<String>>>("exts")?.unwrap_or_default();
            let set: HashSet<String> = exts.iter().map(|e| e.to_lowercase()).collect();
            if set.is_empty() {
                return Err("No file types to clear".into());
            }
            clear_attachment_files(Some(&set)).await?;
            let downloads = db::get_download_dir();
            for (name, _) in list_files(&downloads, false).await {
                if bucket_of(&name).is_some_and(|e| set.contains(&e)) {
                    vector_core::webfiles::remove(&downloads.join(&name)).await;
                }
            }
        }
    }
    let freed = before.saturating_sub(total_bytes().await);
    Ok(json!({ "freed_bytes": freed, "freed_formatted": vector_core::crypto::format_bytes(freed) }))
}
