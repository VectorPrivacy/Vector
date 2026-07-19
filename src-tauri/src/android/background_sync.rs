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

use jni::objects::{GlobalRef, JClass, JObject, JObjectArray, JString};
use jni::{JavaVM, JNIEnv};
use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::{NOSTR_CLIENT, MY_SECRET_KEY, ENCRYPTION_KEY, set_my_public_key};
use vector_core::state::{set_nostr_client, MY_PUBLIC_KEY};
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

/// Instant wakeup for the stop-checker task. Zero CPU cost when idle (futex-based),
/// unlike the old 5-second polling loop which woke the runtime 17,280 times/day.
static STOP_NOTIFY: std::sync::LazyLock<tokio::sync::Notify> = std::sync::LazyLock::new(|| tokio::sync::Notify::new());

/// Whether the standalone sync thread is currently running
static STANDALONE_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// Stored JavaVM for cross-thread JNI calls (set from JNI entry points)
pub static BG_JAVA_VM: OnceLock<JavaVM> = OnceLock::new();

/// Stored application context as GlobalRef (survives Activity destruction)
pub static BG_APP_CONTEXT: OnceLock<GlobalRef> = OnceLock::new();

/// Stored data directory path (captured from nativeStartBackgroundSync for later use in nativeOnPause)
static BG_DATA_DIR: OnceLock<String> = OnceLock::new();

/// In FULL APP MODE the foreground owns the global client AND the long-lived notification loop. The
/// standalone sync (onPause) replaces the global with its own connected client so backgrounded ops keep
/// working — this stashes the foreground client so `nativeOnResume` can restore it. Without the restore,
/// the loop stays bound to the foreground client while every later subscription (`nostr_client()`) lands
/// on the orphaned standalone client: a community created mid-session never delivers live until restart.
static PRESWAP_FOREGROUND_CLIENT: Mutex<Option<Client>> = Mutex::new(None);

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
        STOP_NOTIFY.notify_one();
    }

    // Restore the foreground client as the global. The standalone sync replaced it with its own client
    // (torn down on the stop above); leaving that in place orphans the foreground notification loop from
    // every subscription added afterward (mid-session community delivery dies until restart). Reconnect
    // it too — its sockets dropped while backgrounded, and the pool re-applies the live subs on connect.
    if let Some(fg) = PRESWAP_FOREGROUND_CLIENT.lock().unwrap().take() {
        logcat("Restoring foreground client as global (post-resume)");
        set_nostr_client(fg.clone());
        tauri::async_runtime::spawn(async move {
            fg.connect().await;
        });
    }

    // A message for the chat the user already has open, arriving while softly backgrounded, is
    // auto-marked read but still posts a notification (the activity wasn't foreground). The chat
    // never re-opens, so the in-app read path can't clear it — cancel the active chat's notification
    // here on resume. Only the active chat; notifications for other chats stay until those are read.
    if let Some(chat_id) = vector_core::state::get_active_chat() {
        cancel_notification_jni(&chat_id);
    }
}

/// Last time (unix secs) the background hook refreshed the query planner's stats. Throttles
/// PRAGMA optimize so the frequent onPause (every app-switch/shade-pull) doesn't re-analyze.
static LAST_BG_OPTIMIZE_SECS: AtomicI64 = AtomicI64::new(0);

/// Refresh planner stats when the app backgrounds — the Android analogue of the desktop app-exit
/// hook. Throttled (stats don't need refreshing more than a couple of times an hour) and run off the
/// lifecycle thread so the first, slower analyze never stalls the app-switch.
fn maybe_optimize_on_pause() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let last = LAST_BG_OPTIMIZE_SECS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < 1800 {
        return; // at most once per 30 min
    }
    LAST_BG_OPTIMIZE_SECS.store(now, Ordering::Relaxed);
    std::thread::spawn(|| {
        crate::account_manager::optimize_db();
        logcat("Refreshed query-planner stats (background)");
    });
}

/// Called from MainActivity.onPause via JNI
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnPause(
    _env: JNIEnv,
    _class: JClass,
) {
    ACTIVITY_IN_FOREGROUND.store(false, Ordering::Release);
    logcat("Activity paused (background)");

    // Backgrounding is our chance to refresh planner stats on this account's live connection, before
    // any standalone-sync re-init below swaps the pool. Throttled + off-thread (see the fn).
    maybe_optimize_on_pause();

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

/// Called from MainActivity when another app shares files/text *into* Vector
/// (ACTION_SEND / ACTION_SEND_MULTIPLE). Forwards the content:// URIs and any
/// text to the share handler, which stores it pending + emits to the frontend.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnShareReceived(
    mut env: JNIEnv,
    _class: JClass,
    uris: JObjectArray<'_>,
    text: JString<'_>,
) {
    let mut uri_vec: Vec<String> = Vec::new();
    if let Ok(len) = env.get_array_length(&uris) {
        for i in 0..len {
            if let Ok(obj) = env.get_object_array_element(&uris, i) {
                let js = JString::from(obj);
                // Convert into an owned String in a single statement so the
                // JavaStr/Result temporaries (which borrow `js`) are dropped at
                // the `;`, before `js` itself drops at the end of the block.
                let owned: Option<String> = env.get_string(&js).ok().map(|s| s.into());
                if let Some(s) = owned {
                    // Only accept content:// URIs. Legitimate cross-app shares
                    // are always content:// (Android blocks file:// in
                    // EXTRA_STREAM); rejecting other schemes stops a crafted
                    // share from coaxing us into reading our own private files
                    // (e.g. file:///data/data/<pkg>/...) and sending them.
                    if s.starts_with("content://") {
                        uri_vec.push(s);
                    } else if !s.is_empty() {
                        logcat(&format!("Share: rejected non-content URI scheme: {}",
                            s.split(':').next().unwrap_or("?")));
                    }
                }
            }
        }
    }
    let text: String = env.get_string(&text).map(|s| s.into()).unwrap_or_default();

    logcat(&format!("Share received: {} file(s), {} text chars", uri_vec.len(), text.len()));
    crate::share::set_pending_share(uri_vec, text);
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
    STOP_NOTIFY.notify_one();
}

/// The service-only process discards Rust stdout/stderr, so a panic in the sync
/// thread otherwise vanishes without a trace (thread dies, no relay connection,
/// no notifications). Route panics to logcat so they're diagnosable.
static BG_PANIC_HOOK: std::sync::Once = std::sync::Once::new();
fn install_bg_panic_logger() {
    BG_PANIC_HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let loc = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "?".to_string());
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            logcat(&format!("RUST PANIC at {}: {}", loc, msg));
            prev(info);
        }));
    });
}

/// Main loop for standalone background sync.
/// Bootstraps a Nostr client from stored keys, connects to relays,
/// and subscribes to live GiftWrap events for instant notifications.
fn run_standalone_sync_loop(data_dir: &str) {
    install_bg_panic_logger();

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
        // If the account has Tor enabled, bootstrap a TorService now. Without
        // this, every relay connection in the bg-sync flow blackholes against
        // the failsafe — no leak, but no notifications either. The cache was
        // hydrated from the new account's DB by `init_database` above, so
        // sync_to_active_account starts a service iff this account wants Tor.
        // First-boot pays a 5-15s consensus fetch; subsequent boots are ~2s
        // off the cached directory.
        if let Err(e) = crate::commands::tor::sync_to_active_account().await {
            logcat(&format!("Tor bootstrap for bg-sync failed: {} (relays will blackhole)", e));
        }

        // Service-only mode silently drops Rust eprintln (which is what
        // log_info!/log_warn! use), so the Tor lifecycle traces never reach
        // logcat. Surface the resulting transport state via the JNI logger
        // so users can confirm bg-sync is honouring the Tor preference.
        #[cfg(feature = "tor")]
        {
            use vector_core::tor::TorTransportState;
            match vector_core::tor::transport_state() {
                TorTransportState::Active(addr) => {
                    logcat(&format!("Tor active for bg-sync — SOCKS proxy on {}", addr));
                }
                TorTransportState::RequiredButInactive => {
                    logcat("Tor required but inactive — bg-sync relays will blackhole until bootstrap completes");
                }
                TorTransportState::Disabled => {
                    logcat("Tor disabled for this account — bg-sync using direct relay connections");
                }
            }
        }

        // Bootstrap the standalone client — get connected ASAP
        let (client, my_public_key, can_decrypt, keys) = match bootstrap_client(data_dir).await {
            Ok(result) => result,
            Err(e) => {
                logcat(&format!("Failed to bootstrap client: {}", e));
                return;
            }
        };

        // Store the client, keys, and public key in globals so headless operations
        // (notification reply, mark-as-read) can use them in service-only mode.
        // ONLY set for unencrypted accounts — encrypted accounts use a signerless
        // client that would poison the OnceCell and prevent the full app from
        // creating a proper client with signer after PIN unlock.
        if can_decrypt {
            // Full app mode: stash the foreground client BEFORE we overwrite the global with this
            // standalone one, so nativeOnResume can restore it (and keep the notification loop and all
            // future subscriptions on a single live client).
            if ACTIVITY_EVER_CREATED.load(Ordering::Acquire) {
                if let Some(fg) = NOSTR_CLIENT.read().unwrap().as_ref().cloned() {
                    *PRESWAP_FOREGROUND_CLIENT.lock().unwrap() = Some(fg);
                }
            }
            set_nostr_client(client.clone());
            set_my_public_key(my_public_key);
            if let Some(keys) = keys {
                MY_SECRET_KEY.store_from_keys(&keys, &[&ENCRYPTION_KEY]);
                drop(keys);
            }
        }

        // Subscribe to GiftWraps addressed to us (DMs, files)
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

        // Preload profiles — needed for display names in notifications.
        // Skip for encrypted accounts (can't read from encrypted DB).
        if can_decrypt {
            preload_profiles_into_state().await;
        }

        // Spawn a stop-checker task that disconnects the client when stop is signaled.
        // Uses Notify for instant zero-cost wakeup instead of polling.
        let client_for_stop = client.clone();
        tokio::spawn(async move {
            STOP_NOTIFY.notified().await;
            logcat("Stop signal received, disconnecting client...");
            client_for_stop.disconnect().await;
        });

        // Track seen event IDs to deduplicate across relays
        let seen_events: Arc<Mutex<HashSet<EventId>>> = Arc::new(Mutex::new(HashSet::new()));

        logcat("Waiting for incoming events...");

        // Live event handler — runs until stop signal or disconnect.
        // Routes GiftWrap events through the DM/file handler for full state consistency.
        let client_for_handler = client.clone();
        let result = client.handle_notifications(move |notification| {
            let client = client_for_handler.clone();
            let seen = seen_events.clone();
            let gift_id = gift_sub_id.clone();

            async move {
                if STOP_STANDALONE_SYNC.load(Ordering::SeqCst) {
                    return Ok(true); // Stop
                }

                // Relay OKs feed the send pipeline: an OK that outlives the
                // per-attempt wait still confirms delivery, and can rescue a
                // message already marked Failed.
                if let RelayPoolNotification::Message {
                    message: nostr_sdk::RelayMessage::Ok { event_id, status, .. }, ..
                } = &notification {
                    vector_core::sending::note_relay_ok(event_id, *status);
                }

                if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                    // Route by subscription
                    let is_gift = subscription_id == gift_id;

                    if !is_gift {
                        return Ok(false);
                    }

                    // Deduplicate across relays
                    if !seen.lock().unwrap().insert(event.id) {
                        return Ok(false);
                    }

                    if can_decrypt {
                        // Full pipeline — decrypt, persist to DB, show rich notification
                        handle_event_with_context(
                            (*event).clone(), true, &client, my_public_key
                        ).await;
                    } else {
                        // Encrypted account — can't decrypt, but we know something arrived
                        post_notification_jni("Vector", "You have a new message", None, None, None, None, None);
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

        // Clean up: stop the nostr client first, then the Tor service.
        // The TorService was spawned on this transient runtime; if we let
        // the runtime drop without an explicit awaited stop, the SOCKS
        // accept loop and per-stream tasks abort abruptly, leaving the
        // state-dir lockfile release time non-deterministic — Windows can
        // throw sharing violations on the next foreground start, and we
        // can leak the lock across runtimes since `tor_slot()` is process-
        // global. Awaiting stop_and_join here guarantees the JoinSet drains
        // and all `Arc<TorClient>` clones release before the runtime dies.
        client.disconnect().await;
        crate::commands::tor::stop_and_join_if_running().await;
        logcat("Client disconnected; Tor stopped");
    });
}

/// Connect the background client to a SINGLE relay for battery efficiency.
/// Tries each candidate relay in order until one connects successfully.
///
/// Relay priority: user's custom relays first, then non-disabled defaults.
/// Connecting to one relay instead of 4-5 dramatically reduces mobile radio wakeups
/// and battery drain — background only needs one relay for push notifications.
async fn bg_connect_single_relay(client: &Client, data_dir: &str) -> Result<(), String> {
    // Build the candidate list: user's custom relays first, then defaults
    let mut candidates: Vec<String> = Vec::new();

    // Read user's relay config from DB (custom relays + disabled defaults).
    // Marker first, then fall back to first npub dir for pre-marker
    // installs. Picking by `read_dir` order on a multi-account install
    // grabs a filesystem-order-dependent account and silently breaks
    // notifications for the user.
    let data_path = std::path::Path::new(data_dir);
    let npub_dir = vector_core::db::read_active_account_file()
        .ok()
        .flatten()
        // Symlinks rejected so a crafted `<data>/<npub-name>` link
        // can't redirect bg-sync into the wrong tree.
        .or_else(|| {
            std::fs::read_dir(data_dir).ok().and_then(|entries| {
                entries.flatten()
                    .filter(|e| matches!(e.file_type(), Ok(ft) if ft.is_dir() && !ft.is_symlink()))
                    .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                    .find(|n| n.starts_with("npub1"))
            })
        })
        .map(|npub| data_path.join(npub));

    let (custom_relays, disabled_defaults) = if let Some(ref dir) = npub_dir {
        let db_path = dir.join("vector.db");
        if let Ok(conn) = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            let custom: Vec<String> = conn.query_row(
                "SELECT value FROM settings WHERE key = 'custom_relays'",
                [],
                |row| row.get::<_, String>(0),
            ).ok()
            .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(&json).ok())
            .map(|arr| arr.iter().filter_map(|v| {
                // Custom relays are objects with "url" and optionally "enabled" fields
                let url = v.get("url").and_then(|u| u.as_str())?;
                let enabled = v.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
                if enabled { Some(url.to_string()) } else { None }
            }).collect())
            .unwrap_or_default();

            let disabled: Vec<String> = conn.query_row(
                "SELECT value FROM settings WHERE key = 'disabled_default_relays'",
                [],
                |row| row.get::<_, String>(0),
            ).ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();

            (custom, disabled)
        } else {
            (vec![], vec![])
        }
    } else {
        (vec![], vec![])
    };

    // Custom relays first (user's preference)
    candidates.extend(custom_relays);

    // Then non-disabled defaults
    for url in DEFAULT_RELAYS {
        let normalized = url.to_lowercase();
        let is_disabled = disabled_defaults.iter().any(|d| d.eq_ignore_ascii_case(&normalized));
        if !is_disabled && !candidates.iter().any(|c| c.eq_ignore_ascii_case(url)) {
            candidates.push(url.to_string());
        }
    }

    // Fallback: if everything is disabled/empty, use all defaults
    if candidates.is_empty() {
        candidates.extend(DEFAULT_RELAYS.iter().map(|s| s.to_string()));
    }

    // Try each relay until one connects successfully.
    // Use pool().add_relay with tor_aware_relay_options — `client.add_relay()`
    // does NOT inherit `ClientOptions::connection`, so without this the bg
    // sync connects direct over clearnet on Tor-enabled accounts.
    for url in &candidates {
        logcat(&format!("Background: trying relay {}", url));
        let opts = vector_core::tor_aware_relay_options(RelayOptions::new());
        if let Err(e) = client.pool().add_relay(url.as_str(), opts).await {
            logcat(&format!("Failed to add relay {}: {:?}", url, e));
            continue;
        }
        client.connect().await;

        // Poll for connection (500ms intervals, up to 10 seconds).
        // Mobile TLS handshakes can take 3-5s; a single fixed sleep misses slow relays.
        let mut connected = false;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // Bail if user foregrounded the app during connection
            if STOP_STANDALONE_SYNC.load(Ordering::SeqCst) {
                return Err("Stop signal during relay connection".to_string());
            }
            if client.relays().await.values().any(|r| matches!(r.status(), RelayStatus::Connected)) {
                connected = true;
                break;
            }
        }

        if connected {
            logcat(&format!("Background: connected to {}", url));
            return Ok(());
        }

        // Didn't connect — remove and try next
        logcat(&format!("Background: {} failed to connect, trying next", url));
        client.remove_relay(url.as_str()).await;
    }

    Err("Failed to connect to any relay".to_string())
}

/// Bootstrap a standalone Nostr client from stored keys in the database.
/// Returns (client, public_key, can_decrypt).
/// When local encryption is enabled, returns a read-only client (no signer) that can
/// subscribe to events but not decrypt them — used for generic "New message" notifications.
async fn bootstrap_client(data_dir: &str) -> Result<(Client, PublicKey, bool, Option<Keys>), String> {
    let data_path = std::path::Path::new(data_dir);

    // Marker first; fall back to first npub dir only on legacy installs.
    // Same pattern as `bg_connect_single_relay` and `bootstrap_pipeline`.
    let (npub_name, npub_dir) = match vector_core::db::read_active_account_file().ok().flatten() {
        Some(npub) => {
            logcat(&format!("Resolved account via marker: {}", npub));
            let dir = data_path.join(&npub);
            (npub, dir)
        }
        None => {
            let entries = std::fs::read_dir(data_path)
                .map_err(|e| format!("Failed to read dataDir: {:?}", e))?;
            let mut name = String::new();
            let mut dir = None;
            for entry in entries.flatten() {
                // Symlinks rejected — a crafted link named after a
                // valid npub must not redirect bg-sync.
                let Ok(ft) = entry.file_type() else { continue; };
                if !ft.is_dir() || ft.is_symlink() { continue; }
                if let Some(n) = entry.file_name().to_str() {
                    if n.starts_with("npub1") {
                        logcat(&format!("No marker; falling back to first npub dir: {}", n));
                        name = n.to_string();
                        dir = Some(entry.path());
                        break;
                    }
                }
            }
            (name, dir.ok_or("No npub account directory found")?)
        }
    };
    // Validate the resolved path's basename matches a real account dir on
    // disk — defends against a stale marker pointing at a directory the
    // user deleted out of band.
    if !npub_dir.exists() {
        return Err(format!("Account directory missing for {}", npub_name));
    }
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

    // Detect encryption status from two DB keys:
    // - encryption_enabled = "false" → explicitly disabled (skip_encryption or disable_encryption)
    // - encryption_enabled missing + security_type exists → encrypted (setup_encryption)
    // This handles the case where encryption was enabled then later disabled:
    // security_type remains in DB but encryption_enabled is set to "false".
    let encryption_enabled_val: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'encryption_enabled'",
            [],
            |row| row.get(0),
        )
        .ok();

    let security_type: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'security_type'",
            [],
            |row| row.get(0),
        )
        .ok();

    // Canonical resolver — bg-sync and Activity must agree on the
    // "is this account encrypted?" answer.
    let encrypted = vector_core::state::resolve_encryption_enabled(
        encryption_enabled_val.as_deref(),
        security_type.as_deref(),
    );

    if encrypted {
        // Encrypted account — can't read nsec, but we can derive the pubkey from the
        // directory name (npub is never encrypted) and subscribe read-only.
        drop(conn);

        let my_public_key = PublicKey::from_bech32(&npub_name)
            .map_err(|e| format!("Failed to parse npub from dir name: {:?}", e))?;

        logcat(&format!("Encrypted account — read-only mode for {}...",
            &npub_name[..20.min(npub_name.len())]));

        let client = Client::builder()
            .opts(vector_core::nostr_client_options())
            .build();
        bg_connect_single_relay(&client, data_dir).await?;

        Ok((client, my_public_key, false, None))
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

        let public_key_for_signer = keys.public_key();
        MY_SECRET_KEY.store_from_keys(&keys, &[&ENCRYPTION_KEY]);

        let client = Client::builder()
            .signer(vector_core::GuardedSigner::new(public_key_for_signer))
            .opts(vector_core::nostr_client_options())
            .build();

        bg_connect_single_relay(&client, data_dir).await?;

        Ok((client, my_public_key, true, Some(keys)))
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

    // Install the download dir override before any inbound event handler
    // persists an attachment. Must match the `lib.rs` setup hook (OnceLock::set
    // is winner-takes-all and the two race depending on bg-sync vs Activity
    // start order); both resolve the same external media dir
    // (/Android/media/<pkg>/Vector). The service is itself a Context, so the
    // JNI call works here.
    if let Some(dir) = crate::android::storage::external_media_dir().map(std::path::PathBuf::from) {
        vector_core::db::set_download_dir(dir);
    }

    // Prefer the active-account marker so background sync stays aligned with
    // whichever account the user last picked in the GUI. Fall back to the
    // first npub directory if no marker is set yet.
    let npub = if let Ok(Some(active)) = vector_core::db::read_active_account_file() {
        active
    } else {
        let entries = std::fs::read_dir(&data_path)
            .map_err(|e| format!("Failed to read dataDir: {:?}", e))?;
        let mut found = None;
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("npub1") {
                    found = Some(name.to_string());
                    break;
                }
            }
        }
        let picked = found.ok_or("No npub account directory found")?;
        logcat(&format!(
            "No active-account marker; bg-sync falling back to first npub dir: {}",
            &picked[..20.min(picked.len())]
        ));
        picked
    };

    // Set current account + initialize DB pool (account_manager delegates to vector-core)
    crate::account_manager::set_current_account(npub.clone())?;
    vector_core::db::init_database(&npub)?;

    // Seed the encryption atomic so any subsequent maybe_encrypt /
    // maybe_decrypt call inside bg-sync agrees with on-disk state. The
    // foreground Activity also seeds via `get_encryption_and_key`, but
    // bg-sync may run alone for minutes before the Activity attaches.
    vector_core::state::init_encryption_enabled();

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

    // Apply the user's content-privacy preference. This is the single chokepoint
    // for every Android notification: the foreground path (show_notification_generic)
    // and the background-sync service both land here. chat_id is untouched so
    // tap-to-open still works (it isn't displayed).
    let (title, body, avatar_path, sender_name, group_name, group_avatar_path):
        (String, String, Option<String>, Option<String>, Option<String>, Option<String>) =
        match crate::services::notif_content_privacy() {
            crate::services::NotifContentPrivacy::Full => (
                title.to_string(), body.to_string(),
                avatar_path.map(str::to_string), sender_name.map(str::to_string),
                group_name.map(str::to_string), group_avatar_path.map(str::to_string),
            ),
            crate::services::NotifContentPrivacy::HideContent => {
                let b = if group_name.is_some() { "Sent a message" } else { "Sent you a message" };
                (
                    title.to_string(), b.to_string(),
                    avatar_path.map(str::to_string), sender_name.map(str::to_string),
                    group_name.map(str::to_string), group_avatar_path.map(str::to_string),
                )
            }
            crate::services::NotifContentPrivacy::HideAll => (
                "Vector".to_string(), "You received a message".to_string(),
                None, None, None, None,
            ),
        };

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

        let jtitle = env.new_string(&title)
            .map_err(|e| format!("Failed to create title string: {:?}", e))?;
        let jbody = env.new_string(&body)
            .map_err(|e| format!("Failed to create body string: {:?}", e))?;
        let javatar = env.new_string(avatar_path.as_deref().unwrap_or(""))
            .map_err(|e| format!("Failed to create avatar string: {:?}", e))?;
        let jchat_id = env.new_string(chat_id.unwrap_or(""))
            .map_err(|e| format!("Failed to create chat_id string: {:?}", e))?;
        let jsender_name = env.new_string(sender_name.as_deref().unwrap_or(""))
            .map_err(|e| format!("Failed to create sender_name string: {:?}", e))?;
        let jgroup_name = env.new_string(group_name.as_deref().unwrap_or(""))
            .map_err(|e| format!("Failed to create group_name string: {:?}", e))?;
        let jgroup_avatar = env.new_string(group_avatar_path.as_deref().unwrap_or(""))
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

/// Revoke a chat's OS notification (chat read in-app, or answered on another device).
/// No-op Kotlin-side if nothing is showing for that chat. Mirrors post_notification_jni's
/// class-loader dance so it works from any JNI-attached thread.
pub fn cancel_notification_jni(chat_id: &str) {
    if chat_id.is_empty() { return; }

    let vm = match BG_JAVA_VM.get() {
        Some(vm) => vm,
        None => { logcat("cancel_notification_jni: JavaVM not stored"); return; }
    };
    let context_ref = match BG_APP_CONTEXT.get() {
        Some(ctx) => ctx,
        None => { logcat("cancel_notification_jni: App context not stored"); return; }
    };
    let mut env = match vm.attach_current_thread() {
        Ok(env) => env,
        Err(e) => { logcat(&format!("cancel_notification_jni: attach failed: {:?}", e)); return; }
    };
    let context = context_ref.as_obj();

    let result: Result<(), String> = (|| {
        let class_loader = env.call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(|e| format!("getClassLoader: {:?}", e))?
            .l().map_err(|e| format!("ClassLoader cast: {:?}", e))?;
        let class_name = env.new_string("io.vectorapp.VectorNotificationService")
            .map_err(|e| format!("class name: {:?}", e))?;
        let service_class = env.call_method(&class_loader, "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)])
            .map_err(|e| format!("loadClass: {:?}", e))?
            .l().map_err(|e| format!("class cast: {:?}", e))?;
        let service_jclass = jni::objects::JClass::from(service_class);

        let jchat_id = env.new_string(chat_id).map_err(|e| format!("chat_id: {:?}", e))?;
        env.call_static_method(&service_jclass, "cancelNotification",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[context.into(), (&jchat_id).into()])
            .map_err(|e| format!("cancelNotification: {:?}", e))?;
        Ok(())
    })();

    if let Err(e) = result {
        logcat(&format!("Failed to cancel notification: {}", e));
    }
}

/// Called from NotificationActionReceiver when the user taps "Mark as Read".
/// Marks the chat as read in STATE + DB (headless, no TAURI_APP needed).
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_NotificationActionReceiver_nativeMarkAsRead(
    mut env: JNIEnv,
    _class: JClass,
    chat_id: JString<'_>,
) {
    let chat_id: String = match env.get_string(&chat_id) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    if chat_id.is_empty() { return; }
    logcat(&format!("Mark as read: {}...", &chat_id[..chat_id.len().min(20)]));

    // Spawn a thread with its own tokio runtime (JNI calls are synchronous)
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => { logcat(&format!("mark_as_read rt error: {:?}", e)); return; }
        };
        rt.block_on(async {
            let result = crate::chat::mark_as_read_headless(&chat_id).await;
            logcat(&format!("mark_as_read_headless result: {}", result));
        });
    });
}

/// Called from NotificationActionReceiver when the user sends an inline reply.
/// Sends a DM and marks the chat as read.
/// On success, re-posts the notification with the reply appended.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_NotificationActionReceiver_nativeSendReply(
    mut env: JNIEnv,
    _class: JClass,
    chat_id: JString<'_>,
    content: JString<'_>,
) {
    let chat_id: String = match env.get_string(&chat_id) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    let content: String = match env.get_string(&content) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    if chat_id.is_empty() || content.is_empty() { return; }
    let chat_preview: String = chat_id.chars().take(20).collect();
    let content_preview: String = content.chars().take(40).collect();
    logcat(&format!("Inline reply to {}...: {}", chat_preview, content_preview));

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => { logcat(&format!("send_reply rt error: {:?}", e)); return; }
        };
        rt.block_on(async {
            // Wait for background sync to finish initializing the client (up to 15s).
            // This handles the race where the service just restarted and bootstrap
            // hasn't completed yet when the user taps Reply.
            let mut waited = 0u32;
            while crate::nostr_client().is_none() && waited < 150 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                waited += 1;
            }
            if waited > 0 {
                logcat(&format!("Waited {}ms for client init", waited * 100));
            }

            match crate::message::send_text_reply_headless(&chat_id, &content).await {
                Ok(_) => {
                    logcat("Inline reply sent successfully");
                    // Look up our own profile for avatar and display name
                    let (sender_label, avatar) = {
                        let my_pk = crate::my_public_key()
                            .and_then(|pk| pk.to_bech32().ok());
                        match my_pk {
                            Some(npub) => {
                                let state = crate::STATE.lock().await;
                                match state.get_profile(&npub) {
                                    Some(profile) => {
                                        let name = if !profile.display_name.is_empty() {
                                            format!("Me ({})", &*profile.display_name)
                                        } else if !profile.name.is_empty() {
                                            format!("Me ({})", &*profile.name)
                                        } else {
                                            "Me".to_string()
                                        };
                                        let av = if !profile.avatar_cached.is_empty() {
                                            Some(profile.avatar_cached.to_string())
                                        } else {
                                            None
                                        };
                                        (name, av)
                                    }
                                    None => ("Me".to_string(), None),
                                }
                            }
                            None => ("Me".to_string(), None),
                        }
                    };
                    // Re-post notification with our reply appended so the user sees confirmation
                    post_notification_jni(
                        &sender_label,
                        &content,
                        avatar.as_deref(),
                        Some(&chat_id),
                        Some(&sender_label),
                        None,
                        None,
                    );
                }
                Err(e) => {
                    logcat(&format!("Inline reply failed: {}", e));
                    // Post a failure notice so the notification spinner clears
                    post_notification_jni(
                        "Vector",
                        "Reply failed to send",
                        None,
                        Some(&chat_id),
                        None,
                        None,
                        None,
                    );
                }
            }
        });
    });
}

/// Check if background sync is currently active (foreground service running)
#[allow(dead_code)]
pub fn is_background_sync_active() -> bool {
    BACKGROUND_SYNC_ACTIVE.load(Ordering::SeqCst)
}
