//! JNI bridge for Android background sync via foreground service.
//!
//! Provides JNI functions called from Kotlin:
//! - `nativeStartBackgroundSync` — called by VectorNotificationService when foreground service starts.
//!   Stores JNI refs and data_dir. In service-only mode (no Tauri), immediately starts a standalone
//!   Nostr client with live relay subscriptions. In full-app mode, deferred to nativeOnPause.
//! - `nativeStopBackgroundSync` — called when service is destroyed
//! - `nativeOnResume` / `nativeOnPause` — called from MainActivity lifecycle.
//!   onPause starts standalone sync when the app is backgrounded.
//!   onResume stops it when the app returns to foreground.

use jni::objects::{GlobalRef, JClass, JObject, JString};
use jni::{JavaVM, JNIEnv};
use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::{NOSTR_CLIENT, MY_PUBLIC_KEY};
use crate::commands::relays::DEFAULT_RELAYS;
use crate::services::event_handler::handle_event_with_context;

/// Log to Android logcat via NDK. println!/eprintln! go to /dev/null on Android.
fn logcat(msg: &str) {
    use std::ffi::CString;
    extern "C" {
        fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
    }
    let tag = CString::new("VectorBgSync").unwrap();
    let text = CString::new(msg).unwrap_or_else(|_| CString::new("(invalid msg)").unwrap());
    unsafe { __android_log_write(4, tag.as_ptr(), text.as_ptr()); } // 4 = INFO
}

/// Flag indicating whether background sync is active
static BACKGROUND_SYNC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the Activity is currently in the foreground (visible and focused).
/// Set by JNI calls from MainActivity.onResume/onPause.
/// When true, notifications are suppressed (user is looking at the app).
static ACTIVITY_IN_FOREGROUND: AtomicBool = AtomicBool::new(false);

/// Whether an Activity has been created in this process's lifetime.
/// Set to true on first onResume, NEVER set back to false.
/// Distinguishes full-app mode (Activity exists, even if briefly paused) from
/// service-only mode (no Activity at all, e.g. BootReceiver/START_STICKY restart).
static ACTIVITY_EVER_CREATED: AtomicBool = AtomicBool::new(false);

/// Check if the Activity is currently in the foreground
pub fn is_activity_in_foreground() -> bool {
    ACTIVITY_IN_FOREGROUND.load(Ordering::Acquire)
}

/// Signal to stop the standalone sync thread
static STOP_STANDALONE_SYNC: AtomicBool = AtomicBool::new(false);

/// Whether the standalone sync thread is currently running
static STANDALONE_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// Stored JavaVM for cross-thread JNI calls (set from JNI entry points)
static BG_JAVA_VM: OnceLock<JavaVM> = OnceLock::new();

/// Stored application context as GlobalRef (survives Activity destruction)
static BG_APP_CONTEXT: OnceLock<GlobalRef> = OnceLock::new();

/// Stored data directory path (captured from nativeStartBackgroundSync for later use in nativeOnPause)
static BG_DATA_DIR: OnceLock<String> = OnceLock::new();

/// Called from MainActivity.onResume via JNI
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnResume(
    _env: JNIEnv,
    _class: JClass,
) {
    ACTIVITY_IN_FOREGROUND.store(true, Ordering::Release);
    ACTIVITY_EVER_CREATED.store(true, Ordering::Release);
    logcat("Activity resumed (foreground)");

    // Stop standalone sync — the full app's live subscriptions take over
    if STANDALONE_SYNC_RUNNING.load(Ordering::SeqCst) {
        logcat("Stopping standalone sync (activity resumed)");
        STOP_STANDALONE_SYNC.store(true, Ordering::SeqCst);
    }
}

/// Called from MainActivity.onPause via JNI
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnPause(
    _env: JNIEnv,
    _class: JClass,
) {
    ACTIVITY_IN_FOREGROUND.store(false, Ordering::Release);
    logcat("Activity paused (background)");

    // Start standalone sync if the foreground service is active but standalone sync isn't running.
    // This handles the case where the app was open (standalone sync was skipped), and the user
    // now backgrounds the app — the service is still alive but nobody is processing events.
    if BACKGROUND_SYNC_ACTIVE.load(Ordering::SeqCst)
        && !STANDALONE_SYNC_RUNNING.load(Ordering::SeqCst)
    {
        if let Some(data_dir) = BG_DATA_DIR.get() {
            logcat("Activity paused, starting standalone sync");
            let data_dir = data_dir.clone();
            STOP_STANDALONE_SYNC.store(false, Ordering::SeqCst);
            std::thread::spawn(move || {
                STANDALONE_SYNC_RUNNING.store(true, Ordering::SeqCst);
                run_standalone_sync_loop(&data_dir);
                STANDALONE_SYNC_RUNNING.store(false, Ordering::SeqCst);
                logcat("Standalone sync thread exited");
            });
        } else {
            logcat("Activity paused but no data_dir stored, cannot start sync");
        }
    }
}

/// Called from MainActivity when user taps a notification with a chat_id extra.
/// Stores the chat_id as a pending deep link action and emits to the frontend.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnNotificationTap(
    mut env: JNIEnv,
    _class: JClass,
    chat_id: JString<'_>,
) {
    let chat_id: String = env.get_string(&chat_id)
        .map(|s| s.into())
        .unwrap_or_default();
    if chat_id.is_empty() {
        return;
    }
    logcat(&format!("Notification tap → chat: {}...", &chat_id[..chat_id.len().min(20)]));

    // Store as pending action (frontend may not be ready yet)
    crate::deep_link::set_pending_notification_action(&chat_id);

    // Emit to frontend immediately if Tauri is running
    if let Some(handle) = crate::TAURI_APP.get() {
        use tauri::Emitter;
        let action = crate::deep_link::DeepLinkAction {
            action_type: "chat".to_string(),
            target: chat_id,
        };
        let _ = handle.emit("deep_link_action", &action);
    }
}

/// Called by VectorNotificationService when the foreground service starts.
/// Stores JNI refs and data_dir for later use. In service-only mode (no Activity),
/// immediately starts the standalone sync. In full-app mode, standalone sync is
/// deferred until nativeOnPause (when the user backgrounds the app).
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_VectorNotificationService_nativeStartBackgroundSync(
    mut env: JNIEnv,
    _class: JClass,
    data_dir: JString<'_>,
    context: JObject<'_>,
) {
    logcat("Foreground service starting background sync");
    BACKGROUND_SYNC_ACTIVE.store(true, Ordering::SeqCst);
    STOP_STANDALONE_SYNC.store(false, Ordering::SeqCst);

    // Store the JavaVM and application context for cross-thread JNI calls.
    if BG_JAVA_VM.get().is_none() {
        match env.get_java_vm() {
            Ok(vm) => { let _ = BG_JAVA_VM.set(vm); }
            Err(e) => logcat(&format!("Failed to get JavaVM: {:?}", e)),
        }
    }
    if BG_APP_CONTEXT.get().is_none() {
        match env.new_global_ref(&context) {
            Ok(global_ref) => { let _ = BG_APP_CONTEXT.set(global_ref); }
            Err(e) => logcat(&format!("Failed to create context GlobalRef: {:?}", e)),
        }
    }

    let data_dir_str: String = match env.get_string(&data_dir) {
        Ok(s) => s.into(),
        Err(e) => {
            logcat(&format!("Failed to get dataDir string: {:?}", e));
            return;
        }
    };

    // Store data_dir for later use by nativeOnPause
    let _ = BG_DATA_DIR.set(data_dir_str.clone());

    // If an Activity has ever been created in this process, we're in full-app mode.
    // Defer standalone sync to nativeOnPause (when the user actually backgrounds).
    // This avoids racing with the Activity lifecycle flicker (onResume→onPause→onResume)
    // that happens during Activity creation, where ACTIVITY_IN_FOREGROUND is briefly false.
    let activity_exists = ACTIVITY_EVER_CREATED.load(Ordering::Acquire);
    let tauri_running = crate::TAURI_APP.get().is_some();
    logcat(&format!("Guard check: activity_exists={}, TAURI_APP={}", activity_exists, tauri_running));
    if activity_exists || tauri_running {
        logcat("Full app mode, standalone sync deferred to onPause");
        return;
    }

    // If standalone sync is already running, skip
    if STANDALONE_SYNC_RUNNING.load(Ordering::SeqCst) {
        logcat("Standalone sync already running, skipping");
        return;
    }

    logcat("Starting standalone sync thread for service-only mode");

    // Spawn a background thread for persistent relay subscription
    std::thread::spawn(move || {
        STANDALONE_SYNC_RUNNING.store(true, Ordering::SeqCst);
        run_standalone_sync_loop(&data_dir_str);
        STANDALONE_SYNC_RUNNING.store(false, Ordering::SeqCst);
        logcat("Standalone sync thread exited");
    });
}

/// Called when transitioning back to foreground or when service is destroyed.
/// Signals the standalone sync thread to stop.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_VectorNotificationService_nativeStopBackgroundSync(
    _env: JNIEnv,
    _class: JClass,
) {
    logcat("Stopping background sync");
    BACKGROUND_SYNC_ACTIVE.store(false, Ordering::SeqCst);
    STOP_STANDALONE_SYNC.store(true, Ordering::SeqCst);
}

/// Main loop for standalone background sync.
/// Bootstraps a Nostr client from stored keys, connects to relays,
/// and subscribes to live GiftWrap events for instant notifications.
fn run_standalone_sync_loop(data_dir: &str) {
    // Create a persistent tokio runtime for this thread
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            logcat(&format!("Failed to create tokio runtime: {:?}", e));
            return;
        }
    };

    // Bootstrap the shared processing pipeline (DB, accounts, etc.)
    if let Err(e) = bootstrap_pipeline(data_dir) {
        logcat(&format!("Failed to bootstrap pipeline: {}", e));
        // Fall through — will still try to connect and notify, just without DB persistence
    }

    rt.block_on(async {
        // Bootstrap the standalone client — get connected ASAP
        let (client, my_public_key, can_decrypt) = match bootstrap_client(data_dir).await {
            Ok(result) => result,
            Err(e) => {
                logcat(&format!("Failed to bootstrap client: {}", e));
                return;
            }
        };

        // Subscribe to GiftWraps addressed to us (DMs, files, MLS welcomes)
        // limit(0) = only new events going forward (real-time streaming)
        let giftwrap_filter = Filter::new()
            .pubkey(my_public_key)
            .kind(Kind::GiftWrap)
            .limit(0);

        let gift_sub_id = match client.subscribe(giftwrap_filter, None).await {
            Ok(output) => {
                logcat("Live GiftWrap subscription active");
                output.val
            }
            Err(e) => {
                logcat(&format!("Failed to subscribe: {:?}", e));
                return;
            }
        };

        // Subscribe to MLS group messages (only useful for unencrypted accounts that can decrypt)
        let mls_sub_id = if can_decrypt {
            let mls_filter = Filter::new()
                .kind(Kind::MlsGroupMessage)
                .limit(0);
            match client.subscribe(mls_filter, None).await {
                Ok(output) => {
                    logcat("Live MLS group message subscription active");
                    Some(output.val)
                }
                Err(e) => {
                    logcat(&format!("Failed to subscribe to MLS messages: {:?}", e));
                    None
                }
            }
        } else {
            None
        };

        // Preload profiles AFTER subscribe — runs while relay TCP/TLS handshakes complete.
        // Profiles are only needed when a notification arrives (to resolve display names).
        // Skip for encrypted accounts (can't read profiles from encrypted DB).
        if can_decrypt {
            preload_profiles_into_state().await;
            preload_mls_groups_into_state().await;
        }

        // Spawn a stop-checker task that disconnects the client when stop is signaled.
        // This ensures handle_notifications() returns even if no events arrive.
        let client_for_stop = client.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if STOP_STANDALONE_SYNC.load(Ordering::SeqCst) {
                    logcat("Stop signal received, disconnecting client...");
                    client_for_stop.disconnect().await;
                    break;
                }
            }
        });

        // Track seen event IDs to deduplicate across relays
        let seen_events: Arc<Mutex<HashSet<EventId>>> = Arc::new(Mutex::new(HashSet::new()));

        logcat("Waiting for incoming events...");

        // Live event handler — runs until stop signal or disconnect.
        // Routes GiftWrap events through the DM/file handler and MLS group messages
        // through the shared MLS handler for full state consistency.
        let client_for_handler = client.clone();
        let result = client.handle_notifications(move |notification| {
            let client = client_for_handler.clone();
            let seen = seen_events.clone();
            let gift_id = gift_sub_id.clone();
            let mls_id = mls_sub_id.clone();

            async move {
                if STOP_STANDALONE_SYNC.load(Ordering::SeqCst) {
                    return Ok(true); // Stop
                }

                if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                    // Route by subscription
                    let is_gift = subscription_id == gift_id;
                    let is_mls = mls_id.as_ref().map_or(false, |id| subscription_id == *id);

                    if !is_gift && !is_mls {
                        return Ok(false);
                    }

                    // Deduplicate across relays
                    if !seen.lock().unwrap().insert(event.id) {
                        return Ok(false);
                    }

                    if is_gift {
                        if can_decrypt {
                            // Full pipeline — decrypt, persist to DB, show rich notification
                            handle_event_with_context(
                                (*event).clone(), true, &client, my_public_key
                            ).await;
                        } else {
                            // Encrypted account — can't decrypt, but we know something arrived
                            post_notification_jni("Vector", "You have a new message", None, None, None, None, None);
                        }
                    } else if is_mls {
                        // MLS group message — process through shared handler
                        crate::services::subscription_handler::handle_mls_group_message(
                            (*event).clone(), my_public_key
                        ).await;
                    }

                    // Cap the seen set to prevent unbounded memory growth
                    let seen_len = seen.lock().unwrap().len();
                    if seen_len > 1000 {
                        seen.lock().unwrap().clear();
                    }
                }

                Ok(false) // Continue listening
            }
        }).await;

        match &result {
            Ok(_) => logcat("handle_notifications returned Ok"),
            Err(e) => logcat(&format!("handle_notifications returned Err: {:?}", e)),
        }

        // Clean up
        client.disconnect().await;
        logcat("Client disconnected");
    });
}

/// Bootstrap a standalone Nostr client from stored keys in the database.
/// Returns (client, public_key, can_decrypt).
/// When local encryption is enabled, returns a read-only client (no signer) that can
/// subscribe to events but not decrypt them — used for generic "New message" notifications.
async fn bootstrap_client(data_dir: &str) -> Result<(Client, PublicKey, bool), String> {
    let data_path = std::path::Path::new(data_dir);

    // Scan for npub directories
    let entries = std::fs::read_dir(data_path)
        .map_err(|e| format!("Failed to read dataDir: {:?}", e))?;

    let mut npub_dir = None;
    let mut npub_name = String::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("npub1") {
                logcat(&format!("Found account dir: {}", name));
                npub_name = name.to_string();
                npub_dir = Some(entry.path());
                break;
            }
        }
    }

    let npub_dir = npub_dir.ok_or("No npub account directory found")?;
    let db_path = npub_dir.join("vector.db");

    if !db_path.exists() {
        return Err("Database file not found".into());
    }

    logcat(&format!("Opening database: {:?}", db_path));

    // Open database directly (read-only)
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("Failed to open database: {:?}", e))?;

    // Check if encryption is enabled
    let encryption_enabled: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'encryption_enabled'",
            [],
            |row| row.get(0),
        )
        .ok();

    let encrypted = encryption_enabled.as_deref() == Some("true");

    if encrypted {
        // Encrypted account — can't read nsec, but we can derive the pubkey from the
        // directory name (npub is never encrypted) and subscribe read-only.
        drop(conn);

        let my_public_key = PublicKey::from_bech32(&npub_name)
            .map_err(|e| format!("Failed to parse npub from dir name: {:?}", e))?;

        logcat(&format!("Encrypted account — read-only mode for {}...",
            &npub_name[..20.min(npub_name.len())]));

        let client = Client::builder().build();

        for relay_url in DEFAULT_RELAYS {
            if let Err(e) = client.add_relay(*relay_url).await {
                logcat(&format!("Failed to add relay {}: {:?}", relay_url, e));
            }
        }

        logcat(&format!("Connecting to {} relays...", DEFAULT_RELAYS.len()));
        client.connect().await;

        Ok((client, my_public_key, false))
    } else {
        // Normal account — full signer client
        let pkey: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'pkey'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read pkey: {:?}", e))?;

        drop(conn);

        let keys = Keys::parse(&pkey)
            .map_err(|e| format!("Failed to parse stored key: {:?}", e))?;

        let my_public_key = keys.public_key();

        logcat(&format!("Bootstrapped client for {}...",
            &my_public_key.to_bech32().unwrap_or_default()[..20.min(my_public_key.to_bech32().unwrap_or_default().len())]));

        let client = Client::builder()
            .signer(keys)
            .build();

        for relay_url in DEFAULT_RELAYS {
            if let Err(e) = client.add_relay(*relay_url).await {
                logcat(&format!("Failed to add relay {}: {:?}", relay_url, e));
            }
        }

        logcat(&format!("Connecting to {} relays...", DEFAULT_RELAYS.len()));
        client.connect().await;

        Ok((client, my_public_key, true))
    }
}

/// Bootstrap the shared processing pipeline for headless (service-only) mode.
/// Sets up APP_DATA_DIR, CURRENT_ACCOUNT, and the DB connection pool so that
/// `handle_event_with_context()` can persist decrypted messages to the database.
/// Call this once before processing events in the background service.
fn bootstrap_pipeline(data_dir: &str) -> Result<String, String> {
    let data_path = std::path::PathBuf::from(data_dir);

    // Set APP_DATA_DIR so static path helpers work
    crate::account_manager::set_app_data_dir(data_path.clone());

    // Find the npub directory
    let entries = std::fs::read_dir(&data_path)
        .map_err(|e| format!("Failed to read dataDir: {:?}", e))?;

    let mut npub = None;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("npub1") {
                npub = Some(name.to_string());
                break;
            }
        }
    }

    let npub = npub.ok_or("No npub account directory found")?;

    // Set CURRENT_ACCOUNT so DB path resolution works
    crate::account_manager::set_current_account(npub.clone())?;

    // Initialize the DB pool
    let db_path = crate::account_manager::get_database_path_static(&npub)?;
    crate::account_manager::init_db_pool_static(&db_path)?;

    logcat(&format!("Pipeline bootstrapped for {}", &npub[..20.min(npub.len())]));
    Ok(npub)
}

/// Load saved profiles from the database into STATE so that notifications
/// can show real display names instead of "New Message".
async fn preload_profiles_into_state() {
    match crate::db::get_all_profiles().await {
        Ok(profiles) => {
            let count = profiles.len();
            let mut state = crate::STATE.lock().await;
            for slim in profiles {
                let npub = slim.id.clone();
                let profile = slim.to_profile();
                state.insert_or_replace_profile(&npub, profile);
            }
            logcat(&format!("Preloaded {} profiles into STATE", count));
        }
        Err(e) => {
            logcat(&format!("Failed to preload profiles: {}", e));
        }
    }
}

/// Load MLS group metadata from the database into STATE so that group message
/// notifications can show real group names instead of "Group Chat".
async fn preload_mls_groups_into_state() {
    match crate::db::load_mls_groups().await {
        Ok(groups) => {
            let count = groups.iter().filter(|g| !g.evicted).count();
            let mut state = crate::STATE.lock().await;
            for group in groups {
                if group.evicted { continue; }
                state.create_or_get_mls_group_chat(&group.group_id, vec![]);
                if let Some(chat) = state.get_chat_mut(&group.group_id) {
                    chat.metadata.set_name(group.name.clone());
                }
            }
            logcat(&format!("Preloaded {} MLS groups into STATE", count));
        }
        Err(e) => {
            logcat(&format!("Failed to preload MLS groups: {}", e));
        }
    }
}

/// Post a notification by calling VectorNotificationService.showMessageNotification() in Kotlin.
/// Uses the stored JavaVM + GlobalRef context (captured during JNI entry) instead of
/// ndk_context, which is NOT initialized in service-only mode (no Tauri Activity).
///
/// Also used by `show_notification_generic` on Android to bypass the Tauri notification plugin.
pub fn post_notification_jni(
    title: &str,
    body: &str,
    avatar_path: Option<&str>,
    chat_id: Option<&str>,
    sender_name: Option<&str>,
    group_name: Option<&str>,
    group_avatar_path: Option<&str>,
) {
    // Don't post notifications when the user is actively using the app
    if is_activity_in_foreground() {
        return;
    }
    let vm = match BG_JAVA_VM.get() {
        Some(vm) => vm,
        None => {
            logcat("post_notification_jni: JavaVM not stored");
            return;
        }
    };
    let context_ref = match BG_APP_CONTEXT.get() {
        Some(ctx) => ctx,
        None => {
            logcat("post_notification_jni: App context not stored");
            return;
        }
    };

    let mut env = match vm.attach_current_thread() {
        Ok(env) => env,
        Err(e) => {
            logcat(&format!("post_notification_jni: Failed to attach thread: {:?}", e));
            return;
        }
    };

    let context = context_ref.as_obj();

    // Get the app's class loader via Context.getClassLoader() (NOT Class.getClassLoader()).
    // Context.getClassLoader() returns the app's PathClassLoader which has all DEX classes.
    // Class.getClassLoader() on a framework class (Application) returns the boot class loader.
    let result: Result<(), String> = (|| {
        let class_loader = env.call_method(
            context,
            "getClassLoader",
            "()Ljava/lang/ClassLoader;",
            &[],
        )
        .map_err(|e| format!("Failed to get ClassLoader: {:?}", e))?
        .l()
        .map_err(|e| format!("Failed to convert ClassLoader: {:?}", e))?;

        let class_name = env.new_string("io.vectorapp.VectorNotificationService")
            .map_err(|e| format!("Failed to create class name string: {:?}", e))?;
        let service_class = env.call_method(
            &class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)],
        )
        .map_err(|e| format!("Failed to load VectorNotificationService class: {:?}", e))?
        .l()
        .map_err(|e| format!("Failed to convert class: {:?}", e))?;

        let service_jclass = jni::objects::JClass::from(service_class);

        let jtitle = env.new_string(title)
            .map_err(|e| format!("Failed to create title string: {:?}", e))?;
        let jbody = env.new_string(body)
            .map_err(|e| format!("Failed to create body string: {:?}", e))?;
        let javatar = env.new_string(avatar_path.unwrap_or(""))
            .map_err(|e| format!("Failed to create avatar string: {:?}", e))?;
        let jchat_id = env.new_string(chat_id.unwrap_or(""))
            .map_err(|e| format!("Failed to create chat_id string: {:?}", e))?;
        let jsender_name = env.new_string(sender_name.unwrap_or(""))
            .map_err(|e| format!("Failed to create sender_name string: {:?}", e))?;
        let jgroup_name = env.new_string(group_name.unwrap_or(""))
            .map_err(|e| format!("Failed to create group_name string: {:?}", e))?;
        let jgroup_avatar = env.new_string(group_avatar_path.unwrap_or(""))
            .map_err(|e| format!("Failed to create group_avatar string: {:?}", e))?;

        env.call_static_method(
            &service_jclass,
            "showMessageNotification",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[context.into(), (&jtitle).into(), (&jbody).into(), (&javatar).into(), (&jchat_id).into(), (&jsender_name).into(), (&jgroup_name).into(), (&jgroup_avatar).into()],
        )
        .map_err(|e| format!("Failed to call showMessageNotification: {:?}", e))?;

        logcat("Notification posted via JNI");
        Ok(())
    })();

    if let Err(e) = result {
        logcat(&format!("Failed to post notification: {}", e));
    }
}

/// Check if background sync is currently active (foreground service running)
#[allow(dead_code)]
pub fn is_background_sync_active() -> bool {
    BACKGROUND_SYNC_ACTIVE.load(Ordering::SeqCst)
}
