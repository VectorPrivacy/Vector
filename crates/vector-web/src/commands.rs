//! The IPC surface: Tauri command names and argument shapes, served from core.
//!
//! Anything unlisted rejects with a "not on Vector Web" error, which the
//! frontend already treats as a failed call.

use serde_json::{json, Value};
use vector_core::{db, VectorCore, STATE};
use vector_core::profile::sync::{self as profile_sync, SyncPriority};

use crate::community::{self as cm, ControlOp};
use crate::{account, attachments, files, messaging, sync};

/// Arguments as the frontend sent them (camelCase keys, like Tauri's).
pub struct Args(pub Value);

impl Args {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    pub fn str(&self, key: &str) -> Result<String, String> {
        self.opt_str(key).ok_or_else(|| format!("missing argument `{key}`"))
    }

    pub fn opt_str(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_string)
    }

    pub fn usize(&self, key: &str) -> Result<usize, String> {
        self.get(key).and_then(Value::as_u64).map(|n| n as usize).ok_or_else(|| format!("missing argument `{key}`"))
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    pub fn de<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<T, String> {
        serde_json::from_value(self.0.get(key).cloned().unwrap_or(Value::Null)).map_err(|e| format!("argument `{key}`: {e}"))
    }
}

/// A core facade result as a command result.
pub fn core<T: serde::Serialize>(r: vector_core::Result<T>) -> Result<Value, String> {
    r.map_err(|e| e.to_string()).and_then(to_value)
}

pub fn to_value<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

fn priority(name: &str) -> SyncPriority {
    match name {
        "critical" => SyncPriority::Critical,
        "high" => SyncPriority::High,
        "low" => SyncPriority::Low,
        _ => SyncPriority::Medium,
    }
}

pub async fn dispatch(cmd: &str, a: Args) -> Result<Value, String> {
    match cmd {
        // --- Boot ---
        "get_platform_features" => Ok(json!({
            "transcription": false,
            "notification_sounds": false,
            "os": "web",
            "is_mobile": false,
            "debug_mode": false,
            "media_url": null,
            "self_update": false,
            "share_audio": false,
            "storage": crate::storage::Storage::current().name(),
        })),
        "check_account_downgrade" => Ok(Value::Null),
        "get_theme" => {
            if db::get_current_account().is_err() {
                return Ok(Value::Null);
            }
            to_value(db::get_sql_setting("theme".into())?)
        }
        "get_sql_setting" => {
            if db::get_current_account().is_err() {
                return Ok(Value::Null);
            }
            let key = a.str("key")?;
            if db::settings::SECRET_SETTINGS.contains(&key.as_str()) {
                return Err(format!("`{key}` is not readable here"));
            }
            to_value(db::get_sql_setting(key)?)
        }
        "set_sql_setting" => {
            let key = a.str("key")?;
            if db::settings::PROTECTED_SETTINGS.contains(&key.as_str()) {
                return Err(format!("`{key}` is managed by the app and can't be set here"));
            }
            if db::get_current_account().is_ok() {
                db::set_sql_setting(key, a.str("value")?)?;
            }
            Ok(Value::Null)
        }
        "get_encryption_and_key" => account::get_encryption_and_key(),
        "get_encryption_status" => Ok(account::get_encryption_status()),
        "get_current_account" => to_value(db::get_current_account()?),
        "list_accounts_with_metadata" => Ok(account::list_accounts_with_metadata()),
        "set_active_account" => {
            db::write_active_account_file(&a.str("npub")?)?;
            Ok(Value::Null)
        }
        // A reload restarts the worker, which boots the marked account and ends every task of this one.
        "swap_session" => {
            crate::storage::reload().await;
            Ok(Value::Null)
        }
        "tor_get_state" => Ok(json!({
            "enabled": false, "prelogin": false, "multi_circuit": true, "running": false, "supported": false,
            "status": "", "bootstrap_progress": 0, "socks_proxy": null,
        })),
        "tor_get_bridges" => Ok(json!([])),
        // The browser has no sockets of its own: Vector Web connects directly, always.
        "transport_get_state" => to_value(vector_core::transport::status::view()),
        "transport_allow_realtime" => to_value(vector_core::transport::status::view()),
        "transport_set" | "transport_set_prelogin" => match a.str("kind")?.as_str() {
            "clearnet" => to_value(vector_core::transport::status::view()),
            _ => Err("Vector Web connects directly. Tor and I2P need the desktop or Android app.".into()),
        },
        "transport_retry" => to_value(vector_core::transport::status::view()),
        "transport_get_routes" => {
            let urls: Vec<String> = a.de("urls")?;
            to_value(urls.iter().take(512).map(|u| vector_core::transport::route_view(u)).collect::<Vec<_>>())
        }
        "transport_get_aliases" => Ok(json!([])),
        "transport_set_alias" | "transport_check_alias" | "transport_find_alias" | "i2p_set_router" | "i2p_test_router"
        | "i2p_set_exit" | "i2p_set_outproxies" => Err("I2P isn't available on Vector Web.".into()),
        "i2p_get_config" => to_value(vector_core::transport::i2p_config::view_of(&Default::default())),
        "tor_prelogin_abandon" | "tor_get_host_circuit" | "transport_prelogin_abandon" => Ok(Value::Null),
        "get_logs" => Ok(json!("")),
        "get_pending_share" | "get_pending_deep_link" => Ok(Value::Null),

        // --- Login ---
        "login" => account::login(a.str("importKey")?).await,
        "create_account" => account::create_account().await,
        "skip_encryption" => account::skip_encryption().await.map(|_| Value::Null),
        "setup_encryption" => account::setup_encryption(a.str("password")?, a.opt_str("securityType").unwrap_or_else(|| "pin".into()))
            .await
            .map(|_| Value::Null),
        "login_from_stored_key" => to_value(account::login_from_stored_key(a.opt_str("password")).await?),
        "connect" => Ok(json!(account::connect().await)),

        // --- Session start ---
        "fetch_messages" => sync::fetch_messages(a.bool("init").unwrap_or(false)).await.map(|_| Value::Null),
        "notifs" => {
            sync::notifs();
            Ok(json!(true))
        }
        "web_capabilities" => {
            crate::miniapps::set_available(a.bool("miniApps").unwrap_or(true));
            Ok(Value::Null)
        }
        "web_resume" => {
            let hidden_ms = a.get("hiddenMs").and_then(Value::as_u64).unwrap_or(0);
            db::spawn_bound(crate::catchup::resume(hidden_ms));
            Ok(Value::Null)
        }
        "sync_all_profiles" => {
            profile_sync::sync_all_profiles().await;
            Ok(Value::Null)
        }
        "queue_profile_sync" => {
            let p = a.opt_str("priority").unwrap_or_default();
            profile_sync::queue_profile_sync(a.str("npub")?, priority(&p), a.bool("forceRefresh").unwrap_or(false));
            Ok(Value::Null)
        }
        "load_profile" => {
            let ok = profile_sync::load_profile(a.str("npub")?, &sync::WebProfileSyncHandler).await;
            Ok(json!(ok))
        }
        "queue_chat_profiles_sync" => {
            profile_sync::queue_chat_profiles(a.str("chatId")?, a.bool("isOpening").unwrap_or(false)).await;
            Ok(Value::Null)
        }
        "refresh_profile_now" => {
            profile_sync::refresh_profile_now(a.str("npub")?);
            Ok(Value::Null)
        }
        "is_scanning" => Ok(json!(STATE.lock().await.is_syncing)),
        "get_dm_contacts" => to_value(db::events::get_dm_contact_npubs()?),
        "get_install_source" => Ok(json!({ "has_store": false, "label": "" })),
        "run_maintenance" | "monitor_relay_connections" => Ok(json!(true)),
        "pin_chat" | "unpin_chat" => {
            let client = vector_core::state::nostr_client().ok_or("Nostr client not initialized")?;
            let id = a.str("chatId")?;
            let list = if cmd == "pin_chat" {
                vector_core::pinned_chats::pin_chat(&client, &id).await?
            } else {
                vector_core::pinned_chats::unpin_chat(&client, &id).await?
            };
            vector_core::traits::emit_event_json("pinned_chats_updated", json!(list.chats));
            Ok(json!(list.chats))
        }
        "get_pinned_chats" => to_value(vector_core::pinned_chats::load_local().chats),
        "get_rail_layout" => to_value(vector_core::synced_prefs::load_rail()),
        "get_hidden_banners" => to_value(vector_core::synced_prefs::load_hidden_banners().ids),
        "set_banner_hidden" => {
            let hidden = a.bool("hidden").ok_or("missing argument `hidden`")?;
            let list = vector_core::synced_prefs::set_banner_hidden(&a.str("communityId")?, hidden)?;
            vector_core::traits::emit_event_json("hidden_banners_updated", json!({ "ids": list.ids }));
            crate::network_ops::publish_projection(vector_core::synced_prefs::Pref::Banners);
            Ok(json!(list.ids))
        }
        "locate_message" => to_value(db::events::locate_message(&a.str("messageId")?)?),
        "get_synced_settings" => Ok(vector_core::synced_prefs::load_settings().view()),
        "set_advanced_mode" => {
            let on = a.bool("on").ok_or("missing argument `on`")?;
            let settings = vector_core::synced_prefs::set_advanced(on)?;
            vector_core::traits::emit_event_json("synced_settings_updated", settings.view());
            crate::network_ops::publish_projection(vector_core::synced_prefs::Pref::Settings);
            Ok(settings.view())
        }
        "set_streamer_mode" | "set_streamer_notif" | "set_streamer_wallpapers" => {
            let before = vector_core::synced_prefs::load_settings().streamer;
            let settings = match cmd {
                "set_streamer_mode" => vector_core::synced_prefs::set_streamer_on(a.bool("on").ok_or("missing argument `on`")?)?,
                "set_streamer_notif" => vector_core::synced_prefs::set_streamer_notif(&a.str("level")?)?,
                _ => vector_core::synced_prefs::set_streamer_wallpapers(a.bool("hide").ok_or("missing argument `hide`")?)?,
            };
            crate::selfsync::settings_changed(&before, &settings);
            crate::network_ops::publish_projection(vector_core::synced_prefs::Pref::Settings);
            Ok(settings.view())
        }
        "get_paused_downloads" => Ok(json!({})),
        "get_unread_counts" => to_value(db::events::unread_counts().await?),
        "update_unread_counter" => Ok(json!(messaging::unread_total().await)),
        "list_emoji_packs" => to_value(vector_core::emoji_packs::load_all_packs()?),
        "refresh_emoji_packs" => to_value(vector_core::emoji_packs::refresh_subscribed_packs().await?),
        "get_pack_share_naddr" => to_value(vector_core::emoji_packs::share_naddr(&a.str("naddr")?).await?),
        "get_theme_emoji_pack" => to_value(vector_core::emoji_packs::get_or_fetch_theme_pack(&a.str("naddr")?).await?),
        "subscribe_emoji_pack" => to_value(vector_core::emoji_packs::subscribe_pack(&a.str("naddr")?).await?),
        "get_theme_slot_anchor" => to_value(vector_core::emoji_packs::get_theme_slot_anchor()?),
        "set_theme_emoji_pack" => {
            let emojis: Vec<vector_core::emoji_packs::PackEmoji> = a.de("emojis")?;
            vector_core::emoji_packs::set_theme_emoji_tags(emojis.into_iter().map(|e| (e.shortcode, e.url)).collect());
            Ok(Value::Null)
        }
        "get_emoji_usage" => {
            if db::get_current_account().is_err() {
                return Ok(json!([]));
            }
            to_value(vector_core::emoji_usage::ranked(a.de::<Option<usize>>("limit")?))
        }
        "bump_emoji_usage_batch" => {
            if db::get_current_account().is_ok() {
                vector_core::emoji_usage::bump_batch(&a.de::<Vec<vector_core::emoji_usage::EmojiUse>>("entries")?)?;
            }
            Ok(Value::Null)
        }
        "get_blossom_servers_config" => to_value(vector_core::blossom_servers::list_all_servers()),
        "get_media_servers" => to_value(vector_core::state::get_blossom_servers()),
        "get_my_badges" => Ok(json!({
            "vector": vector_core::badges::has_vector_badge(),
            "tier": vector_core::badges::effective_tier(),
            "bug_hunter": vector_core::badges::bug_hunter_tier(),
        })),
        "get_max_account_tier" => Ok(json!(vector_core::badges::max_account_tier())),

        // --- Chats ---
        "set_active_chat" => {
            vector_core::state::set_active_chat(a.opt_str("chatId"));
            Ok(Value::Null)
        }
        "get_self_destruct_timer" => to_value(vector_core::self_destruct::chat_duration_secs(&a.str("chatId")?)),
        "detect_secret" => to_value(vector_core::secrets::detect(&a.str("text")?)),
        "mark_as_read" => Ok(json!(messaging::mark_as_read(a.str("chatId")?, a.opt_str("messageId")).await)),
        "get_chat_message_count" => to_value(messaging::get_chat_message_count(&a.str("chatId")?)?),
        "get_message_views" => to_value(
            messaging::get_message_views(&a.str("chatId")?, a.usize("limit")?, a.usize("offset")?).await?,
        ),
        "get_messages_around" => {
            let chat = db::id_cache::get_chat_id_by_identifier(&a.str("chatId")?)?;
            let (before, after) = (a.usize("before")?.min(512), a.usize("after")?.min(512));
            to_value(db::events::get_messages_around(chat, &a.str("anchorId")?, before, after).await?)
        }
        "get_system_events" => messaging::get_system_events(&a.str("conversationId")?),
        "pivx_get_chat_payments" => Ok(json!([])),
        "download_attachment" => {
            Ok(json!(attachments::download_attachment(a.str("npub")?, a.str("msgId")?, a.str("attachmentId")?).await))
        }
        "evict_chat_messages" => Ok(Value::Null),
        "message" => {
            messaging::message(a.str("receiver")?, a.opt_str("content").unwrap_or_default(), a.opt_str("repliedTo").unwrap_or_default())
                .await
        }
        // --- Files ---
        "cache_android_file" | "get_file_info" => files::file_info(&a.str("filePath").or_else(|_| a.str("path"))?).await,
        "is_directory" => Ok(json!(false)),
        "start_image_precompression" => files::start_precompression(a.str("filePath")?).await.map(|_| Value::Null),
        "get_compression_status" => files::compression_status(&a.str("filePath")?),
        "clear_compression_cache" => {
            files::clear_compression(&a.str("filePath")?);
            Ok(Value::Null)
        }
        "start_cached_bytes_compression" => files::start_precompression(String::new()).await.map(|_| Value::Null),
        "get_cached_bytes_compression_status" => files::compression_status(""),
        "get_cached_file_info" => Ok(files::cached_info()),
        "preview_cached_file" => files::preview_cached().await,
        "clear_cached_file" => {
            files::clear_cached();
            Ok(Value::Null)
        }
        "read_image_preview" => files::image_preview(&a.str("path")?).await,
        "render_svg" => files::render_svg(&a.str("path")?, a.usize("maxDim").unwrap_or(1024) as u32).await,
        "allow_video_preview" | "clear_android_file_cache" | "clear_all_android_file_cache" => Ok(Value::Null),
        "generate_thumbhash_for_preview" => files::thumbhash_preview(&a.opt_str("filePath").unwrap_or_default()).await,
        "file_has_metadata" => Ok(json!(files::has_metadata(&a.str("filePath")?).await)),
        "file_message" => {
            files::file_message(a.str("receiver")?, a.opt_str("repliedTo").unwrap_or_default(), a.str("filePath")?,
                a.bool("keepMetadata").unwrap_or(false), a.opt_str("nameOverride").unwrap_or_default()).await
        }
        "send_cached_compressed_file" => {
            files::send_compressed(a.str("receiver")?, a.opt_str("repliedTo").unwrap_or_default(), a.str("filePath")?,
                a.bool("keepMetadata").unwrap_or(false), a.opt_str("nameOverride").unwrap_or_default()).await
        }
        "send_cached_file" => {
            files::send_cached(a.str("receiver")?, a.opt_str("repliedTo").unwrap_or_default(), a.bool("useCompression").unwrap_or(false),
                a.bool("keepMetadata").unwrap_or(false), a.opt_str("nameOverride").unwrap_or_default()).await
        }
        "send_community_files" => {
            let channel = a.str("channelId")?;
            let paths: Vec<String> = a.de("filePaths")?;
            let names: Vec<String> = a.de::<Option<Vec<String>>>("nameOverrides")?.unwrap_or_default();
            let (compress, keep) = (a.bool("useCompression").unwrap_or(false), a.bool("keepMetadata").unwrap_or(false));
            let reply = a.opt_str("repliedTo").unwrap_or_default();
            for (i, path) in paths.iter().enumerate() {
                let name = names.get(i).cloned().unwrap_or_default();
                if compress {
                    files::send_compressed(channel.clone(), reply.clone(), path.clone(), keep, name).await?;
                } else {
                    files::file_message(channel.clone(), reply.clone(), path.clone(), keep, name).await?;
                }
            }
            Ok(Value::Null)
        }
        "send_community_cached_file" => {
            files::send_cached(a.str("channelId")?, a.opt_str("repliedTo").unwrap_or_default(), a.bool("useCompression").unwrap_or(false),
                a.bool("keepMetadata").unwrap_or(false), a.opt_str("nameOverride").unwrap_or_default()).await.map(|_| Value::Null)
        }
        "web_send_voice" => files::send_voice(a.str("receiver")?, a.opt_str("repliedTo").unwrap_or_default(), a.str("filePath")?).await,
        "cancel_upload" => {
            messaging::cancel_upload(&a.str("pendingId").or_else(|_| a.str("messageId"))?);
            Ok(Value::Null)
        }
        "generate_thumbhash_preview" => {
            let msg_id = a.str("msgId")?;
            let thumb = VectorCore.get_message(&msg_id).await.and_then(|(_, m)| {
                m.attachments.iter().find_map(|a| a.img_meta.as_ref().map(|i| i.thumbhash.clone()))
            });
            Ok(json!(files::thumbhash_data_url(&thumb.ok_or("No image attachment found")?).ok_or("Failed to decode thumbhash")?))
        }
        "decode_thumbhash" => Ok(json!(files::thumbhash_data_url(&a.str("thumbhash")?).ok_or("Failed to decode thumbhash")?)),

        "get_or_cache_image" => {
            let kind = crate::images::Kind::parse(&a.str("imageType")?).ok_or("Invalid image type")?;
            Ok(json!(crate::images::cache(&a.str("url")?, kind).await?))
        }
        "cache_url_image" => {
            let url = a.str("url")?;
            let path = crate::images::cache(&url, crate::images::Kind::InlineImage).await?;
            vector_core::emit_event("inline_image_cached", &json!({ "url": url, "path": path }));
            Ok(json!(path))
        }

        // --- Wallpapers ---
        "preview_wallpaper" => {
            let p = vector_core::wallpaper::prepare_wallpaper_preview(&a.str("chatId")?, &a.str("filePath")?).await?;
            Ok(json!({ "path": p.path, "was_animated": p.was_animated, "recommended_dim": p.recommended_dim }))
        }
        "publish_wallpaper" => {
            let (blur, dim) = (a.de::<u8>("blur")?, a.de::<u8>("dim")?);
            vector_core::wallpaper::publish_wallpaper(&a.str("chatId")?, blur, dim).await.map(|_| Value::Null)
        }
        "cancel_wallpaper_preview" => vector_core::wallpaper::cancel_wallpaper_preview(&a.str("chatId")?).await.map(|_| Value::Null),
        "remove_wallpaper" => vector_core::wallpaper::remove_wallpaper(&a.str("chatId")?).await.map(|_| Value::Null),

        // --- Communities ---
        "list_communities" => cm::list_communities(),
        "get_community" => cm::get_community(&a.str("communityId")?),
        "get_community_admins" => cm::get_community_admins(&a.str("communityId")?),
        "get_community_members" => Ok(cm::get_community_members(&a.str("communityId")?).await),
        "get_community_banlist" => Ok(cm::get_community_banlist(&a.str("communityId")?)),
        "get_community_invite_summary" => cm::get_community_invite_summary(&a.str("communityId")?),
        "get_community_capabilities" => core(VectorCore.community_capabilities(&a.str("communityId")?)),
        "get_community_roles_view" => core(VectorCore.community_roles_view(&a.str("communityId")?)),
        "get_community_role_graph" => core(VectorCore.community_role_graph(&a.str("communityId")?)),
        "check_community_raid" => core(VectorCore.check_community_raid(&a.str("communityId")?)),
        "list_community_invites" => to_value(db::community::list_pending_invites()?),
        "list_public_invites" => to_value(db::community::list_public_invites(&a.str("communityId")?)?),
        "sync_community_channel" => {
            cm::sync_community_channel(&a.str("channelId")?, a.de("beforeMs")?, a.bool("resetCursor").unwrap_or(false)).await
        }
        "send_community_message" => cm::send_community_message(a.str("channelId")?, a.opt_str("content").unwrap_or_default(), a.opt_str("repliedTo"), a.opt_str("bot"))
            .await
            .map(|_| Value::Null),
        "react_to_community_message" => {
            let (emoji, url) = (a.str("emoji")?, a.opt_str("emojiUrl"));
            cm::community_control(&a.str("channelId")?, &a.str("messageId")?, ControlOp::React { emoji: &emoji, url: url.as_deref() })
                .await
                .map(|_| Value::Null)
        }
        "edit_community_message" => {
            let content = a.str("newContent")?;
            cm::community_control(&a.str("channelId")?, &a.str("messageId")?, ControlOp::Edit(&content)).await.map(|_| Value::Null)
        }
        "delete_community_message" => {
            let message_id = a.str("messageId")?;
            let channel = STATE.lock().await.find_message(&message_id).map(|(c, _)| c.id.clone()).ok_or("message not found (already deleted?)")?;
            cm::community_control(&channel, &message_id, ControlOp::Delete).await.map(|_| Value::Null)
        }
        "create_community" => cm::create_community(a.str("name")?, a.opt_str("channelName"), a.de("relays")?).await,
        "create_community_channel" => {
            cm::create_community_channel(a.str("communityId")?, a.str("name")?, a.bool("private").unwrap_or(false)).await
        }
        "update_community_metadata" => {
            let id = a.str("communityId")?;
            let (name, description) = (a.opt_str("name"), a.opt_str("description"));
            VectorCore.edit_community_metadata(&id, name.as_deref(), description.as_deref()).await.map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "preview_public_invite" => cm::preview_public_invite(&a.str("url")?).await,
        "accept_public_invite" => cm::accept_public_invite(a.str("url")?).await,
        "accept_community_invite" => cm::accept_community_invite(&a.str("communityId")?).await,
        "decline_community_invite" => {
            db::community::delete_pending_invite(&a.str("communityId")?)?;
            Ok(Value::Null)
        }
        "create_public_invite" => {
            let expires_at_ms = a.de::<Option<u64>>("expiresInSecs")?.map(|secs| {
                let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default().as_secs();
                now.saturating_add(secs).saturating_mul(1000)
            });
            core(VectorCore.create_public_invite(&a.str("communityId")?, expires_at_ms, a.opt_str("label")).await)
        }
        "revoke_public_invite" => core(VectorCore.revoke_public_invite(&a.str("communityId")?, &a.str("token")?).await),
        "invite_to_community" => core(VectorCore.invite_to_community(&a.str("communityId")?, &a.str("inviteeNpub")?).await),
        "leave_community" => core(VectorCore.leave_community(&a.str("communityId")?).await),
        "get_channel_pins" => core(VectorCore.get_channel_pins(&a.str("communityId")?, &a.str("channelId")?)),
        "pin_community_message" => {
            core(VectorCore.pin_community_message(&a.str("communityId")?, &a.str("channelId")?, &a.str("messageId")?).await)
        }
        "unpin_community_message" => {
            core(VectorCore.unpin_community_message(&a.str("communityId")?, &a.str("channelId")?, &a.str("messageId")?).await)
        }
        "reorder_community_roles" => core(VectorCore.reorder_roles(&a.str("communityId")?, &a.de::<Vec<String>>("ordered")?).await),
        "set_community_member_roles" => {
            core(VectorCore.set_member_roles(&a.str("communityId")?, &a.str("npub")?, a.de("roleIds")?).await)
        }
        "get_chat_commands" => to_value(VectorCore.get_chat_commands(&a.str("chatId")?).await),
        "cache_community_image" => crate::community_ops::cache_community_image(&a).await,
        "cache_invite_logo" => crate::community_ops::cache_invite_logo(&a).await,

        "start_typing" => {
            let receiver = a.str("receiver")?;
            if receiver.starts_with("npub1") {
                let _ = VectorCore.send_typing(&receiver).await;
            } else {
                let _ = VectorCore.send_community_typing(&receiver).await;
            }
            Ok(Value::Null)
        }

        _ => {
            for module in crate::MODULES {
                if let Some(result) = module(cmd, &a).await {
                    return result;
                }
            }
            vector_core::log_warn!("[Web] unsupported command: {cmd}");
            Err(format!("`{cmd}` is not available on Vector Web yet"))
        }
    }
}
