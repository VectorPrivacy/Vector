//! The IPC surface: Tauri command names and argument shapes, served from core.
//!
//! Anything unlisted rejects with a "not on Vector Web" error, which the
//! frontend already treats as a failed call.

use serde_json::{json, Value};
use vector_core::db;
use vector_core::profile::sync::{self as profile_sync, SyncPriority};

use crate::{account, messaging, sync};

/// Arguments as the frontend sent them (camelCase keys, like Tauri's).
pub struct Args(pub Value);

impl Args {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    fn str(&self, key: &str) -> Result<String, String> {
        self.opt_str(key).ok_or_else(|| format!("missing argument `{key}`"))
    }

    fn opt_str(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_string)
    }

    fn usize(&self, key: &str) -> Result<usize, String> {
        self.get(key).and_then(Value::as_u64).map(|n| n as usize).ok_or_else(|| format!("missing argument `{key}`"))
    }

    fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, String> {
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
            to_value(db::get_sql_setting(a.str("key")?)?)
        }
        "set_sql_setting" => {
            if db::get_current_account().is_ok() {
                db::set_sql_setting(a.str("key")?, a.str("value")?)?;
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
        // A reload restarts the worker, which boots the marked account.
        "swap_session" => Ok(Value::Null),
        "tor_get_state" => Ok(json!({
            "enabled": false, "running": false, "supported": false,
            "status": "", "bootstrap_progress": 0, "socks_proxy": null,
        })),
        "tor_get_bridges" => Ok(json!([])),
        "get_logs" => Ok(json!("")),
        "get_pending_share" | "get_pending_deep_link" => Ok(Value::Null),

        // --- Login ---
        "login" => account::login(a.str("importKey")?).await,
        "create_account" => account::create_account().await,
        "skip_encryption" => account::skip_encryption().await.map(|_| Value::Null),
        "login_from_stored_key" => to_value(account::login_from_stored_key(a.opt_str("password")).await?),
        "connect" => Ok(json!(account::connect().await)),

        // --- Session start ---
        "fetch_messages" => sync::fetch_messages(a.bool("init").unwrap_or(false)).await.map(|_| Value::Null),
        "notifs" => {
            sync::notifs();
            Ok(json!(true))
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
        "run_maintenance" | "monitor_relay_connections" => Ok(json!(true)),
        "list_community_invites" => Ok(json!([])),
        "get_pinned_chats" => to_value(vector_core::pinned_chats::load_local().chats),
        "get_rail_layout" => to_value(vector_core::synced_prefs::load_rail()),
        "get_paused_downloads" => Ok(json!({})),
        "get_unread_counts" => to_value(db::events::unread_counts().await?),
        "update_unread_counter" => Ok(json!(messaging::unread_total().await)),
        "list_emoji_packs" | "get_emoji_usage" => Ok(json!([])),
        "get_theme_slot_anchor" => Ok(Value::Null),
        "bump_emoji_usage_batch" => Ok(Value::Null),
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
        "mark_as_read" => Ok(json!(messaging::mark_as_read(a.str("chatId")?, a.opt_str("messageId")).await)),
        "get_chat_message_count" => to_value(messaging::get_chat_message_count(&a.str("chatId")?)?),
        "get_message_views" => to_value(
            messaging::get_message_views(&a.str("chatId")?, a.usize("limit")?, a.usize("offset")?).await?,
        ),
        "get_system_events" => messaging::get_system_events(&a.str("conversationId")?),
        "pivx_get_chat_payments" => Ok(json!([])),
        "evict_chat_messages" => Ok(Value::Null),
        "message" => {
            messaging::message(a.str("receiver")?, a.opt_str("content").unwrap_or_default(), a.opt_str("repliedTo").unwrap_or_default())
                .await
        }
        "start_typing" => {
            let _ = vector_core::VectorCore.send_typing(&a.str("receiver")?).await;
            Ok(Value::Null)
        }

        _ => {
            vector_core::log_warn!("[Web] unsupported command: {cmd}");
            Err(format!("`{cmd}` is not available on Vector Web yet"))
        }
    }
}
