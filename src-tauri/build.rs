fn main() {
    // By default, Tauri enables all commands in all windows.
    // Adding `.commands()` explicitly here makes it generate
    // permissions for the commands, and makes it require to specify
    // the permissions for our commands in capabilities explicitly
    // for each window / webview.
    // 
    // This is critical for security: Mini Apps should only be able to
    // call miniapp_* commands, not arbitrary app commands.
    //
    // See: https://v2.tauri.app/security/permissions/
    // See: https://docs.rs/tauri-build/latest/tauri_build/struct.AppManifest.html
    tauri_build::try_build(tauri_build::Attributes::default().app_manifest(
        tauri_build::AppManifest::default().commands(&[
            // Database commands
            "get_theme",
            "get_pkey",
            "set_pkey",
            "get_seed",
            "set_seed",
            "get_sql_setting",
            "set_sql_setting",
            "remove_setting",
            // Profile commands
            "load_profile",
            "update_profile",
            "update_status",
            "upload_avatar",
            "toggle_muted",
            "set_nickname",
            // Chat commands
            "mark_as_read",
            // Message commands
            "message",
            "paste_message",
            "voice_message",
            "file_message",
            "file_message_compressed",
            "get_file_info",
            "cache_android_file",
            "cache_file_bytes",
            "get_cached_file_info",
            "get_cached_image_preview",
            "start_cached_bytes_compression",
            "get_cached_bytes_compression_status",
            "send_cached_file",
            "send_file_bytes",
            "clear_cached_file",
            "clear_android_file_cache",
            "clear_all_android_file_cache",
            "get_image_preview_base64",
            "start_image_precompression",
            "get_compression_status",
            "clear_compression_cache",
            "send_cached_compressed_file",
            // Image cache commands
            "get_or_cache_image",
            "clear_image_cache",
            "get_image_cache_stats",
            "cache_url_image",
            "react_to_message",
            "fetch_msg_metadata",
            // Core commands
            "fetch_messages",
            "deep_rescan",
            "is_scanning",
            "get_chat_messages_paginated",
            "get_message_views",
            "get_messages_around_id",
            "get_chat_message_count",
            "get_file_hash_index",
            "evict_chat_messages",
            "generate_blurhash_preview",
            "decode_blurhash",
            "download_attachment",
            "login",
            "notifs",
            "get_relays",
            "get_media_servers",
            "monitor_relay_connections",
            "start_typing",
            "send_webxdc_peer_advertisement",
            "connect",
            "encrypt",
            "decrypt",
            "start_recording",
            "stop_recording",
            "update_unread_counter",
            "logout",
            "create_account",
            "get_platform_features",
            "transcribe",
            "download_whisper_model",
            "get_or_create_invite_code",
            "accept_invite_code",
            "get_invited_users",
            "check_fawkes_badge",
            "get_storage_info",
            "clear_storage",
            "load_mls_device_id",
            "load_mls_keypackages",
            "export_keys",
            "regenerate_device_keypackage",
            // MLS core commands
            "create_group_chat",
            "create_mls_group",
            "sync_mls_groups_now",
            "list_mls_groups",
            "get_mls_group_metadata",
            // MLS welcome/invite commands
            "list_pending_mls_welcomes",
            "accept_mls_welcome",
            // MLS advanced helpers
            "add_mls_member_device",
            "invite_member_to_group",
            "remove_mls_member_device",
            "get_mls_group_members",
            "leave_mls_group",
            "list_group_cursors",
            "refresh_keypackages_for_contact",
            // Profile sync commands
            "queue_profile_sync",
            "queue_chat_profiles_sync",
            "refresh_profile_now",
            "sync_all_profiles",
            // Deep link commands
            "get_pending_deep_link",
            // Account manager commands
            "get_current_account",
            "list_all_accounts",
            "check_any_account_exists",
            "switch_account",
            // Mini Apps commands (these are the ONLY commands Mini Apps can call)
            "miniapp_load_info",
            "miniapp_load_info_from_bytes",
            "miniapp_open",
            "miniapp_close",
            "miniapp_get_updates",
            "miniapp_send_update",
            "miniapp_list_open",
            // Mini Apps realtime channel commands (Iroh P2P)
            "miniapp_join_realtime_channel",
            "miniapp_send_realtime_data",
            "miniapp_leave_realtime_channel",
            "miniapp_add_realtime_peer",
            "miniapp_get_realtime_node_addr",
            "miniapp_get_realtime_status",
            // Marketplace commands
            "marketplace_fetch_apps",
            "marketplace_get_cached_apps",
            "marketplace_get_app",
            "marketplace_get_install_status",
            "marketplace_install_app",
            "marketplace_check_installed",
            "marketplace_sync_install_status",
            "marketplace_add_trusted_publisher",
            "marketplace_open_app",
            "marketplace_uninstall_app",
            "marketplace_update_app",
            "marketplace_publish_app",
            "marketplace_get_trusted_publisher",
            // Whisper commands (conditional)
            "delete_whisper_model",
            "list_models",
            // Notification sound commands
            "get_notification_settings",
            "set_notification_settings",
            "preview_notification_sound",
            "select_custom_notification_sound",
            // Maintenance
            "run_maintenance",
            // Debug commands (conditional)
            "debug_hot_reload_sync",
        ]),
    ))
    .expect("failed to run tauri-build");
}