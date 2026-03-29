use nostr_sdk::prelude::*;
use tauri::Manager;

#[macro_use]
extern crate vector_core;

#[macro_use]
mod macros;

mod crypto;

mod db;

mod account_manager;

mod mls;
pub use mls::MlsService;

mod voice;

mod net;

mod blossom;

mod util;

#[cfg(target_os = "android")]
#[path = "android/mod.rs"]
mod android;


/// Media server state (Android only) — holds the localhost URL prefix
/// that the frontend uses instead of `asset://` for media elements.
#[cfg(target_os = "android")]
pub struct MediaServerState {
    /// Full URL prefix: `http://127.0.0.1:{port}/{token}`
    pub url_prefix: String,
}

#[cfg(feature = "whisper")]
mod whisper;

mod message;
pub use vector_core::{Message, Attachment, Reaction};

mod profile;
pub use vector_core::{Profile, ProfileFlags, SlimProfile, Status};

mod profile_sync;

mod chat;
pub use vector_core::{Chat, ChatType, ChatMetadata, SerializableChat};

mod rumor;
pub use vector_core::rumor::{RumorEvent, RumorContext, RumorProcessingResult, ConversationType, process_rumor};

pub mod stored_event {
    pub use vector_core::stored_event::*;
}
pub use vector_core::{StoredEvent, StoredEventBuilder};

mod deep_link;

// Mini Apps (WebXDC-compatible) support
mod miniapps;

// Image caching for avatars, banners, and Mini App icons
mod image_cache;

// NIP-17 Kind 10050 (DM Relay List) support
pub(crate) mod inbox_relays {
    pub use vector_core::inbox_relays::*;
}

// PIVX Promos (addressless cryptocurrency payments)
mod pivx;

// Audio processing: resampling (all platforms) + notification playback (desktop only)
mod audio;

// Unified audio engine: persistent cpal stream, mixing, precomputed FFT waveform
mod audio_engine;

// Shared utilities module (error handling, image encoding, state access)
mod shared;

// SIMD-accelerated operations (hex encoding, image alpha, etc.)
mod simd;

// State management module
mod state;
// Re-export commonly used state items at crate root for backwards compatibility
pub(crate) use state::{
    TAURI_APP, NOSTR_CLIENT, MY_SECRET_KEY, MY_PUBLIC_KEY, STATE,
    TRUSTED_RELAYS, active_trusted_relays, NOTIFIED_WELCOMES, WRAPPER_ID_CACHE,
    MNEMONIC_SEED, ENCRYPTION_KEY, PENDING_NSEC, PENDING_INVITE,
    get_blossom_servers, PendingInviteAcceptance,
};

// Command handlers module (organized by domain)
mod commands;

// Business logic services
mod services;
// Re-export notification types for backwards compatibility
pub(crate) use services::{NotificationData, show_notification_generic};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Install a panic hook that logs the crash before the process dies.
    // Without this, panics in spawned tasks vanish silently.
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let msg = format!("[PANIC {:02}:{:02}:{:02}Z] {info}\n\nBacktrace:\n{backtrace}\n",
            (secs / 3600) % 24, (secs / 60) % 60, secs % 60);
        eprintln!("{msg}");
        // Append to log file (shared with log_error!)
        if let Ok(data_dir) = account_manager::get_app_data_dir() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(data_dir.join("vector.log")) {
                let _ = write!(f, "{}\n", &msg);
            }
        }
    }));

    // Harden against memory inspection and debugger attachment (release builds only).
    // macOS:   PT_DENY_ATTACH blocks task_for_pid + debugger attachment.
    // Linux:   PR_SET_DUMPABLE(0) blocks /proc/pid/mem + ptrace + core dumps.
    // Windows: Strip PROCESS_VM_READ from process DACL, block unsigned DLL injection,
    //          and exit if a debugger is attached.
    #[cfg(not(debug_assertions))]
    {
        #[cfg(target_os = "macos")]
        unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0); }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0); }

        #[cfg(target_os = "windows")]
        unsafe {
            extern "system" {
                // kernel32.dll
                fn IsDebuggerPresent() -> i32;
                fn GetCurrentProcess() -> isize;
                fn SetProcessMitigationPolicy(policy: u32, buf: *const u8, len: usize) -> i32;
                // advapi32.dll — strip memory-read from process handle
                fn SetSecurityInfo(
                    handle: isize, object_type: u32, info: u32,
                    owner: *const u8, group: *const u8, dacl: *const u8, sacl: *const u8,
                ) -> u32;
                fn SetEntriesInAclA(
                    count: u32, entries: *const ExplicitAccessA,
                    old_acl: *const u8, new_acl: *mut *mut u8,
                ) -> u32;
            }

            #[repr(C)]
            struct ExplicitAccessA {
                access_permissions: u32,
                access_mode: u32, // DENY_ACCESS = 3
                inheritance: u32,
                trustee: TrusteeA,
            }
            #[repr(C)]
            struct TrusteeA {
                multiple_trustee: *const u8,
                multiple_trustee_operation: u32,
                trustee_form: u32, // TRUSTEE_IS_NAME = 1
                trustee_type: u32, // TRUSTEE_IS_WELL_KNOWN_GROUP = 5
                trustee_name: *const u8,
            }

            // 1. Exit if debugger is attached
            if IsDebuggerPresent() != 0 {
                std::process::exit(0);
            }

            // 2. Block unsigned DLL injection (ProcessSignaturePolicy = 8, MicrosoftSignedOnly)
            let signature_policy: [u8; 4] = [0x01, 0x00, 0x00, 0x00]; // MicrosoftSignedOnly = 1
            SetProcessMitigationPolicy(8, signature_policy.as_ptr(), 4);

            // 3. Strip PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_DUP_HANDLE from Everyone.
            //    MUST merge into the existing DACL — passing null as old_acl would discard all
            //    default allow ACEs, making the process inaccessible to Explorer (breaks taskbar
            //    pinning, jump lists, and other shell integrations).
            extern "system" {
                fn GetSecurityInfo(
                    handle: isize, object_type: u32, info: u32,
                    owner: *mut *mut u8, group: *mut *mut u8,
                    dacl: *mut *mut u8, sacl: *mut *mut u8,
                    descriptor: *mut *mut u8,
                ) -> u32;
            }

            let mut existing_dacl: *mut u8 = std::ptr::null_mut();
            let mut security_descriptor: *mut u8 = std::ptr::null_mut();
            // SE_KERNEL_OBJECT = 6, DACL_SECURITY_INFORMATION = 4
            let got_dacl = GetSecurityInfo(
                GetCurrentProcess(), 6, 4,
                std::ptr::null_mut(), std::ptr::null_mut(),
                &mut existing_dacl, std::ptr::null_mut(),
                &mut security_descriptor,
            ) == 0;

            let everyone = b"Everyone\0";
            let entry = ExplicitAccessA {
                access_permissions: 0x0010 | 0x0020 | 0x0040, // VM_READ | VM_WRITE | DUP_HANDLE
                access_mode: 3, // DENY_ACCESS
                inheritance: 0, // NO_INHERITANCE
                trustee: TrusteeA {
                    multiple_trustee: std::ptr::null(),
                    multiple_trustee_operation: 0,
                    trustee_form: 1, // TRUSTEE_IS_NAME
                    trustee_type: 5, // TRUSTEE_IS_WELL_KNOWN_GROUP
                    trustee_name: everyone.as_ptr(),
                },
            };
            let mut new_dacl: *mut u8 = std::ptr::null_mut();
            // Merge our deny ACE into the existing DACL (preserving default allow ACEs)
            let old_dacl = if got_dacl && !existing_dacl.is_null() { existing_dacl } else { std::ptr::null() };
            if SetEntriesInAclA(1, &entry, old_dacl, &mut new_dacl) == 0 && !new_dacl.is_null() {
                SetSecurityInfo(GetCurrentProcess(), 6, 4, std::ptr::null(), std::ptr::null(), new_dacl, std::ptr::null());
            }
        }
    }

    // Install rustls crypto provider before any TLS usage (required when both
    // 'ring' and 'aws-lc-rs' features are pulled by different transitive deps)
    let _ = rustls::crypto::ring::default_provider().install_default();

    #[cfg(target_os = "linux")]
    {
        // WebKitGTK can be quite funky cross-platform: as a result, we'll fallback to a more compatible renderer
        // In theory, this will make Vector run more consistently across a wider range of Linux Desktop distros.
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    #[cfg(target_os = "windows")]
    {
        // WebView2's GPU blocklist can cause software rendering fallback, resulting in
        // extremely poor WebGL performance (e.g. WebXDC games at ~5fps on gaming hardware).
        // This env var is applied globally before any WebView2 is created, avoiding the
        // freeze issues that occur with per-window additional_browser_args.
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--ignore-gpu-blocklist");
    }

    #[allow(unused_mut)] // mut needed on desktop for plugin registration
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        // Register the webxdc:// custom protocol for Mini Apps (async to avoid deadlocks on Windows)
        .register_asynchronous_uri_scheme_protocol("webxdc", miniapps::scheme::miniapp_protocol)
        // Register Mini Apps state
        .manage(miniapps::state::MiniAppsState::new());

    // MCP Bridge plugin for AI-assisted debugging (desktop debug builds only)
    #[cfg(all(debug_assertions, desktop))]
    {
        builder = builder.plugin(tauri_plugin_mcp_bridge::init());
    }

    // Desktop-only plugins
    #[cfg(desktop)]
    {
        // Window state plugin: saves and restores window position, size, maximized state, etc.
        // Exclude VISIBLE flag so window starts hidden (we show it after content loads to prevent white flash)
        use tauri_plugin_window_state::StateFlags;
        builder = builder.plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .build()
        );
        
        // Single-instance plugin: ensures deep links are passed to existing instance
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Handle deep links from single-instance (Windows/Linux)
            let urls: Vec<String> = args.iter()
                .filter(|arg| arg.starts_with("vector://") || arg.contains("vectorapp.io"))
                .cloned()
                .collect();
            if !urls.is_empty() {
                deep_link::handle_deep_link(app, urls);
            }
            // Focus the existing window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }));
    }

    builder
        .setup(|app| {
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_process::init())?;
            
            let handle = app.app_handle().clone();

            let window = app.get_webview_window("main").unwrap();

            // Setup a graceful shutdown for our Nostr subscriptions
            #[cfg(desktop)]
            let handle_for_window_state = handle.clone();
            window.on_window_event(move |event| {
                match event {
                    // This catches when the window is being closed
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // Block close during encryption migration
                        if !state::is_processing_allowed() {
                            api.prevent_close();
                            return;
                        }

                        // Save window state (position, size, maximized, etc.) before closing
                        #[cfg(desktop)]
                        {
                            use tauri_plugin_window_state::{AppHandleExt, StateFlags};
                            let _ = handle_for_window_state.save_window_state(StateFlags::all());
                        }

                        // Cleanly shutdown our Nostr client
                        if let Some(nostr_client) = NOSTR_CLIENT.get() {
                            tauri::async_runtime::block_on(async {
                                // Shutdown the Nostr client
                                nostr_client.shutdown().await;
                            });
                        }
                    }
                    _ => {}
                }
            });

            // Set the static app data directory FIRST (before any DB access)
            // This must happen before auto_select_account so that static DB
            // connection functions can resolve paths correctly.
            if let Ok(data_dir) = handle.path().app_data_dir() {
                account_manager::set_app_data_dir(data_dir);
            }

            // Auto-select account on startup if one exists but isn't selected
            {
                let handle_clone = handle.clone();
                let _ = account_manager::auto_select_account(&handle_clone);
            }

            // Startup log: persistent MLS device_id if present
            {
                tauri::async_runtime::spawn(async move {
                    if let Ok(Some(id)) = db::load_mls_device_id().await {
                        println!("[MLS] Found persistent mls_device_id at startup: {}", id);
                    }
                });
            }

            // Set as our accessible static app handle
            TAURI_APP.set(handle.clone()).unwrap();

            // Initialize the unified audio engine (persistent cpal output stream)
            audio_engine::AudioEngine::init();

            // Start localhost media server on Android (provides Range request support for
            // <video> and <audio> elements that asset:// doesn't support)
            #[cfg(target_os = "android")]
            {
                let mut allowed_dirs = Vec::new();
                if let Ok(dir) = handle.path().download_dir() {
                    allowed_dirs.push(dir.join("vector"));
                }
                if let Ok(dir) = handle.path().document_dir() {
                    allowed_dirs.push(dir.join("vector"));
                }
                if let Ok(dir) = handle.path().app_data_dir() {
                    allowed_dirs.push(dir);
                }
                let url_prefix = match tauri::async_runtime::block_on(android::media_server::start(allowed_dirs)) {
                    Ok((port, token)) => format!("http://127.0.0.1:{port}/{token}"),
                    Err(e) => {
                        eprintln!("[media_server] failed to start: {e}");
                        String::new() // empty = frontend falls back to asset://
                    }
                };
                app.manage(MediaServerState {
                    url_prefix,
                });
            }

            // Start the profile sync background processor
            tauri::async_runtime::spawn(async {
                profile_sync::start_profile_sync_processor().await;
            });

            
            // Setup deep link listener for macOS/iOS/Android
            // On these platforms, deep links are received as events rather than CLI args
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle_for_deep_link = handle.clone();
                let _listener_id = app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> = event.urls().iter().map(|u| u.to_string()).collect();
                    deep_link::handle_deep_link(&handle_for_deep_link, urls);
                });
            }
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Settings commands (db/settings.rs)
            db::settings::get_theme,
            db::settings::get_pkey,
            db::settings::set_pkey,
            db::settings::get_seed,
            db::settings::set_seed,
            db::settings::get_sql_setting,
            db::settings::set_sql_setting,
            db::settings::remove_setting,
            profile::load_profile,
            profile::update_profile,
            profile::update_status,
            profile::upload_avatar,
            chat::mark_as_read,
            chat::toggle_chat_mute,
            profile::set_nickname,
            profile::block_user,
            profile::unblock_user,
            profile::get_blocked_users,
            message::message,
            message::delete_failed_message,
            message::cancel_upload,
            message::paste_message,
            message::file_message,
            message::file_message_compressed,
            message::forward_attachment,
            message::get_file_info,
            message::cache_android_file,
            message::cache_file_bytes,
            message::get_cached_file_info,
            message::get_cached_image_preview,
            message::generate_thumbhash_for_preview,
            message::start_cached_bytes_compression,
            message::get_cached_bytes_compression_status,
            message::send_cached_file,
            message::send_file_bytes,
            message::clear_cached_file,
            message::clear_android_file_cache,
            message::clear_all_android_file_cache,
            message::get_image_preview_base64,
            message::start_image_precompression,
            message::get_compression_status,
            message::clear_compression_cache,
            message::send_cached_compressed_file,
            message::is_directory,
            message::zip_directory,
            message::cleanup_zip,
            message::react_to_message,
            message::edit_message,
            message::fetch_msg_metadata,
            // Sync commands (commands/sync.rs)
            commands::sync::fetch_messages,
            commands::sync::is_scanning,
            // Messaging commands (commands/messaging.rs)
            commands::messaging::get_chat_messages_paginated,
            commands::messaging::get_message_views,
            commands::messaging::get_messages_around_id,
            commands::messaging::get_system_events,
            commands::messaging::get_chat_message_count,
            commands::messaging::evict_chat_messages,
            // Realtime signaling commands (commands/realtime.rs)
            commands::realtime::notifs,
            commands::realtime::start_typing,
            commands::realtime::send_webxdc_peer_advertisement,
            commands::relays::connect,
            // Account crypto commands (commands/account.rs)
            commands::account::encrypt,
            commands::account::decrypt,
            // Media commands (commands/media.rs)
            commands::media::start_recording,
            commands::media::stop_recording,
            commands::media::transcribe,
            commands::media::download_whisper_model,
            commands::messaging::update_unread_counter,
            commands::system::get_platform_features,
            commands::system::get_device_memory,
            // Invite and badge commands (commands/invites.rs)
            commands::invites::get_or_create_invite_code,
            commands::invites::accept_invite_code,
            commands::invites::get_invited_users,
            commands::invites::check_fawkes_badge,
            commands::system::get_storage_info,
            commands::system::clear_storage,
            commands::system::check_battery_optimized,
            commands::system::request_battery_optimization,
            commands::system::get_background_service_enabled,
            commands::system::set_background_service_enabled,
            commands::system::get_background_service_prompted,
            commands::system::set_background_service_prompted,
            // MLS commands (commands/mls.rs)
            commands::mls::load_mls_device_id,
            commands::mls::load_mls_keypackages,
            commands::mls::list_mls_groups,
            commands::mls::get_mls_group_metadata,
            commands::mls::list_group_cursors,
            commands::mls::create_mls_group,
            commands::mls::create_group_chat,
            commands::mls::upload_group_avatar,
            commands::mls::cache_group_avatar,
            commands::mls::cache_invite_avatar,
            commands::mls::invite_member_to_group,
            commands::mls::remove_mls_member_device,
            commands::mls::sync_mls_groups_now,
            commands::mls::add_mls_member_device,
            commands::mls::get_mls_group_members,
            commands::mls::leave_mls_group,
            commands::mls::update_group_metadata,
            commands::mls::refresh_keypackages_for_contact,
            commands::mls::regenerate_device_keypackage,
            // MLS welcome/invite commands
            commands::mls::list_pending_mls_welcomes,
            commands::mls::accept_mls_welcome,
            // Deep link commands
            deep_link::get_pending_deep_link,
            // Account manager commands
            account_manager::get_current_account,
            account_manager::list_all_accounts,
            account_manager::check_any_account_exists,
            account_manager::switch_account,
            // Mini Apps commands
            miniapps::commands::miniapp_load_info,
            miniapps::commands::miniapp_load_info_from_bytes,
            miniapps::commands::miniapp_open,
            miniapps::commands::miniapp_close,
            miniapps::commands::miniapp_get_updates,
            miniapps::commands::miniapp_send_update,
            miniapps::commands::miniapp_list_open,
            // Mini Apps realtime channel commands (Iroh P2P)
            miniapps::commands::miniapp_join_realtime_channel,
            miniapps::commands::miniapp_leave_realtime_channel,
            miniapps::commands::miniapp_send_realtime_data,
            miniapps::commands::miniapp_add_realtime_peer,
            miniapps::commands::miniapp_get_realtime_node_addr,
            miniapps::commands::miniapp_get_realtime_status,
            // Mini Apps history commands
            miniapps::commands::miniapp_record_opened,
            miniapps::commands::miniapp_get_history,
            miniapps::commands::miniapp_remove_from_history,
            miniapps::commands::miniapp_toggle_favorite,
            miniapps::commands::miniapp_set_favorite,
            // Mini Apps marketplace commands
            miniapps::commands::marketplace_fetch_apps,
            miniapps::commands::marketplace_get_cached_apps,
            miniapps::commands::marketplace_get_app,
            miniapps::commands::marketplace_get_app_by_hash,
            miniapps::commands::marketplace_get_install_status,
            miniapps::commands::marketplace_install_app,
            miniapps::commands::marketplace_check_installed,
            miniapps::commands::marketplace_sync_install_status,
            miniapps::commands::marketplace_add_trusted_publisher,
            miniapps::commands::marketplace_open_app,
            miniapps::commands::marketplace_uninstall_app,
            miniapps::commands::marketplace_update_app,
            miniapps::commands::marketplace_publish_app,
            miniapps::commands::marketplace_get_trusted_publisher,
            // Mini App permissions commands
            miniapps::commands::miniapp_get_available_permissions,
            miniapps::commands::miniapp_get_granted_permissions,
            miniapps::commands::miniapp_get_granted_permissions_for_window,
            miniapps::commands::miniapp_set_permission,
            miniapps::commands::miniapp_set_permissions,
            miniapps::commands::miniapp_has_permission_prompt,
            miniapps::commands::miniapp_revoke_all_permissions,
            // Image cache commands
            image_cache::get_or_cache_image,
            image_cache::clear_image_cache,
            image_cache::get_image_cache_stats,
            image_cache::cache_url_image,
            // PIVX Promos commands
            pivx::pivx_create_promo,
            pivx::pivx_get_promo_balance,
            pivx::pivx_get_wallet_balance,
            pivx::pivx_list_promos,
            pivx::pivx_sweep_promo,
            pivx::pivx_set_wallet_address,
            pivx::pivx_get_wallet_address,
            pivx::pivx_claim_from_message,
            pivx::pivx_import_promo,
            pivx::pivx_refresh_balances,
            pivx::pivx_send_payment,
            pivx::pivx_send_existing_promo,
            pivx::pivx_get_chat_payments,
            pivx::pivx_check_address_balance,
            pivx::pivx_withdraw,
            pivx::pivx_get_currencies,
            pivx::pivx_get_price,
            pivx::pivx_set_preferred_currency,
            pivx::pivx_get_preferred_currency,
            // Audio engine commands (all platforms)
            commands::audio::audio_probe,
            commands::audio::get_audio_metadata,
            commands::audio::audio_load,
            commands::audio::audio_play,
            commands::audio::audio_pause,
            commands::audio::audio_seek,
            commands::audio::audio_stop,
            commands::audio::audio_stop_all,
            commands::audio::audio_set_volume,
            commands::audio::send_recording,
            // Notification sound commands (desktop only)
            #[cfg(desktop)]
            audio::get_notification_settings,
            #[cfg(desktop)]
            audio::set_notification_settings,
            #[cfg(desktop)]
            audio::preview_notification_sound,
            #[cfg(desktop)]
            audio::select_custom_notification_sound,
            // ================================================================
            // Extracted commands (from src/commands/ modules)
            // ================================================================
            // Account commands (commands/account.rs)
            commands::account::login,
            commands::account::login_from_stored_key,
            commands::account::setup_encryption,
            commands::account::skip_encryption,
            #[cfg(debug_assertions)]
            commands::account::debug_hot_reload_sync,
            commands::account::logout,
            commands::account::create_account,
            commands::account::export_keys,
            // Relay commands (commands/relays.rs)
            commands::relays::get_relays,
            commands::relays::get_media_servers,
            commands::relays::get_custom_relays,
            commands::relays::add_custom_relay,
            commands::relays::remove_custom_relay,
            commands::relays::toggle_custom_relay,
            commands::relays::toggle_default_relay,
            commands::relays::update_relay_mode,
            commands::relays::validate_relay_url_cmd,
            commands::relays::get_relay_metrics,
            commands::relays::get_relay_logs,
            commands::relays::monitor_relay_connections,
            // Attachment commands (commands/attachments.rs)
            commands::attachments::generate_thumbhash_preview,
            commands::attachments::decode_thumbhash,
            commands::attachments::download_attachment,
            // Sync commands (commands/sync.rs)
            commands::sync::queue_profile_sync,
            commands::sync::queue_chat_profiles_sync,
            commands::sync::refresh_profile_now,
            commands::sync::sync_all_profiles,
            // System commands (commands/system.rs)
            commands::system::run_maintenance,
            commands::system::get_logs,
            // Encryption toggle commands (commands/encryption.rs)
            commands::encryption::get_encryption_status,
            commands::encryption::get_encryption_and_key,
            commands::encryption::disable_encryption,
            commands::encryption::enable_encryption,
            commands::encryption::rekey_encryption,
            commands::encryption::verify_credential,
            #[cfg(feature = "whisper")]
            whisper::delete_whisper_model,
            #[cfg(feature = "whisper")]
            whisper::list_models,
            #[cfg(feature = "whisper")]
            whisper::cancel_whisper_download
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
