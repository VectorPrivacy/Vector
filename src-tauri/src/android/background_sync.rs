//! JNI bridge for Android background sync and relay polling.
//!
//! Provides three JNI functions called from Kotlin:
//! - `nativeStartBackgroundSync` — called by VectorNotificationService when foreground service starts.
//!   In service-only mode (no Tauri), spawns a background thread that bootstraps a standalone
//!   Nostr client, connects to relays, and subscribes to live GiftWrap events for instant
//!   notifications. Events are delivered in real-time via relay subscriptions.
//! - `nativeStopBackgroundSync` — called when transitioning back to foreground or service destroyed
//! - `pollForNewMessages` — called by RelayPollWorker (WorkManager) for periodic background polling
//!   as a fallback when the foreground service is not running.

use jni::objects::{GlobalRef, JClass, JObject, JString};
use jni::sys::jstring;
use jni::{JavaVM, JNIEnv};
use log::{error, info, warn};
use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::{NOSTR_CLIENT, MY_PUBLIC_KEY};
use crate::commands::relays::DEFAULT_RELAYS;
use crate::services::event_handler::handle_event_with_context;

/// Flag indicating whether background sync is active
static BACKGROUND_SYNC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the Activity is currently in the foreground (visible and focused).
/// Set by JNI calls from MainActivity.onResume/onPause.
/// When true, notifications are suppressed (user is looking at the app).
static ACTIVITY_IN_FOREGROUND: AtomicBool = AtomicBool::new(false);

/// Check if the Activity is currently in the foreground
pub fn is_activity_in_foreground() -> bool {
    ACTIVITY_IN_FOREGROUND.load(Ordering::Acquire)
}

/// Called from MainActivity.onResume via JNI
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnResume(
    _env: JNIEnv,
    _class: JClass,
) {
    ensure_android_logger();
    ACTIVITY_IN_FOREGROUND.store(true, Ordering::Release);
    info!("[BackgroundSync] Activity resumed (foreground)");
}

/// Called from MainActivity.onPause via JNI
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnPause(
    _env: JNIEnv,
    _class: JClass,
) {
    ensure_android_logger();
    ACTIVITY_IN_FOREGROUND.store(false, Ordering::Release);
    info!("[BackgroundSync] Activity paused (background)");
}

/// Signal to stop the standalone sync thread
static STOP_STANDALONE_SYNC: AtomicBool = AtomicBool::new(false);

/// Whether the standalone sync thread is currently running
static STANDALONE_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// Stored JavaVM for cross-thread JNI calls (set from JNI entry points)
static BG_JAVA_VM: OnceLock<JavaVM> = OnceLock::new();

/// Stored application context as GlobalRef (survives Activity destruction)
static BG_APP_CONTEXT: OnceLock<GlobalRef> = OnceLock::new();

/// Ensure the android logger is initialized (safe to call multiple times)
fn ensure_android_logger() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Debug)
                .with_tag("VectorBgSync"),
        );
    });
}

/// Called by VectorNotificationService when the foreground service starts.
/// In full-app mode (NOSTR_CLIENT exists), this is a no-op — live subscriptions handle everything.
/// In service-only mode, spawns a background thread that bootstraps a standalone Nostr client
/// and polls relays every ~2 minutes for new messages, posting notifications via JNI.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_VectorNotificationService_nativeStartBackgroundSync(
    mut env: JNIEnv,
    _class: JClass,
    data_dir: JString<'_>,
    context: JObject<'_>,
) {
    ensure_android_logger();
    info!("[BackgroundSync] Foreground service starting background sync");
    BACKGROUND_SYNC_ACTIVE.store(true, Ordering::SeqCst);
    STOP_STANDALONE_SYNC.store(false, Ordering::SeqCst);

    // Store the JavaVM and application context for cross-thread JNI calls.
    // In service-only mode (no Tauri Activity), ndk_context is not initialized,
    // so we must capture these from the calling JNI method.
    if BG_JAVA_VM.get().is_none() {
        match env.get_java_vm() {
            Ok(vm) => { let _ = BG_JAVA_VM.set(vm); }
            Err(e) => error!("[BackgroundSync] Failed to get JavaVM: {:?}", e),
        }
    }
    if BG_APP_CONTEXT.get().is_none() {
        match env.new_global_ref(&context) {
            Ok(global_ref) => { let _ = BG_APP_CONTEXT.set(global_ref); }
            Err(e) => error!("[BackgroundSync] Failed to create context GlobalRef: {:?}", e),
        }
    }

    // If full app is running, no need for standalone sync
    if NOSTR_CLIENT.get().is_some() {
        info!("[BackgroundSync] Full app is running, skipping standalone sync");
        return;
    }

    // If standalone sync is already running, skip
    if STANDALONE_SYNC_RUNNING.load(Ordering::SeqCst) {
        info!("[BackgroundSync] Standalone sync already running, skipping");
        return;
    }

    let data_dir_str: String = match env.get_string(&data_dir) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("[BackgroundSync] Failed to get dataDir string: {:?}", e);
            return;
        }
    };

    info!("[BackgroundSync] Starting standalone sync thread for service-only mode");

    // Spawn a background thread for persistent polling
    std::thread::spawn(move || {
        STANDALONE_SYNC_RUNNING.store(true, Ordering::SeqCst);
        run_standalone_sync_loop(&data_dir_str);
        STANDALONE_SYNC_RUNNING.store(false, Ordering::SeqCst);
        info!("[BackgroundSync] Standalone sync thread exited");
    });
}

/// Called when transitioning back to foreground or when service is destroyed.
/// Signals the standalone sync thread to stop.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_VectorNotificationService_nativeStopBackgroundSync(
    _env: JNIEnv,
    _class: JClass,
) {
    ensure_android_logger();
    info!("[BackgroundSync] Stopping background sync");
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
            error!("[BackgroundSync] Failed to create tokio runtime: {:?}", e);
            return;
        }
    };

    // Bootstrap the shared processing pipeline (DB, accounts, etc.)
    if let Err(e) = bootstrap_pipeline(data_dir) {
        error!("[BackgroundSync] Failed to bootstrap pipeline: {}", e);
        // Fall through — will still try to connect and notify, just without DB persistence
    }

    rt.block_on(async {
        // Bootstrap the standalone client
        let (client, my_public_key) = match bootstrap_client(data_dir).await {
            Ok(result) => result,
            Err(e) => {
                error!("[BackgroundSync] Failed to bootstrap client: {}", e);
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
                info!("[BackgroundSync] Live GiftWrap subscription active");
                output.val
            }
            Err(e) => {
                error!("[BackgroundSync] Failed to subscribe: {:?}", e);
                return;
            }
        };

        // Spawn a stop-checker task that disconnects the client when stop is signaled.
        // This ensures handle_notifications() returns even if no events arrive.
        let client_for_stop = client.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if STOP_STANDALONE_SYNC.load(Ordering::SeqCst) {
                    info!("[BackgroundSync] Stop signal received, disconnecting client...");
                    client_for_stop.disconnect().await;
                    break;
                }
            }
        });

        // Track seen event IDs to deduplicate across relays
        let seen_events: Arc<Mutex<HashSet<EventId>>> = Arc::new(Mutex::new(HashSet::new()));

        info!("[BackgroundSync] Waiting for incoming GiftWrap events...");

        // Live event handler — runs until stop signal or disconnect
        // Uses the shared pipeline: decrypt → persist to DB → show notification.
        // Notifications are handled inside handle_event_with_context via
        // show_notification_generic → post_notification_jni (no extra unwrap needed).
        let client_for_handler = client.clone();
        let result = client.handle_notifications(move |notification| {
            let client = client_for_handler.clone();
            let seen = seen_events.clone();
            let sub_id = gift_sub_id.clone();

            async move {
                if STOP_STANDALONE_SYNC.load(Ordering::SeqCst) {
                    return Ok(true); // Stop
                }

                if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                    if subscription_id != sub_id {
                        return Ok(false);
                    }

                    // Deduplicate across relays
                    if !seen.lock().unwrap().insert(event.id) {
                        return Ok(false);
                    }

                    // Process through shared pipeline (decrypts, persists to DB, notifies)
                    handle_event_with_context(
                        (*event).clone(), true, &client, my_public_key
                    ).await;

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
            Ok(_) => info!("[BackgroundSync] handle_notifications returned Ok"),
            Err(e) => error!("[BackgroundSync] handle_notifications returned Err: {:?}", e),
        }

        // Clean up
        client.disconnect().await;
        info!("[BackgroundSync] Client disconnected");
    });
}

/// Bootstrap a standalone Nostr client from stored keys in the database.
/// Returns the client and the user's public key.
async fn bootstrap_client(data_dir: &str) -> Result<(Client, PublicKey), String> {
    let data_path = std::path::Path::new(data_dir);

    // Scan for npub directories
    let entries = std::fs::read_dir(data_path)
        .map_err(|e| format!("Failed to read dataDir: {:?}", e))?;

    let mut npub_dir = None;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("npub1") {
                info!("[BackgroundSync] Found account dir: {}", name);
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

    info!("[BackgroundSync] Opening database: {:?}", db_path);

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

    if encryption_enabled.as_deref() == Some("true") {
        return Err("Local encryption is enabled, cannot bootstrap background client".into());
    }

    // Read the stored nsec key
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

    info!("[BackgroundSync] Bootstrapped client for {}...",
        &my_public_key.to_bech32().unwrap_or_default()[..20.min(my_public_key.to_bech32().unwrap_or_default().len())]);

    let client = Client::builder()
        .signer(keys)
        .build();

    // Add default relays
    for relay_url in DEFAULT_RELAYS {
        if let Err(e) = client.add_relay(*relay_url).await {
            warn!("[BackgroundSync] Failed to add relay {}: {:?}", relay_url, e);
        }
    }

    info!("[BackgroundSync] Connecting to {} relays...", DEFAULT_RELAYS.len());
    client.connect().await;

    // Wait for connections to establish
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    let relays = client.relays().await;
    let connected = relays.values().filter(|r| r.status() == RelayStatus::Connected).count();
    info!("[BackgroundSync] Connected to {}/{} relays", connected, relays.len());

    Ok((client, my_public_key))
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

    info!("[BackgroundSync] Pipeline bootstrapped for {}", &npub[..20.min(npub.len())]);
    Ok(npub)
}

/// Poll relays for new GiftWrap events and post notifications for unseen messages.
/// Returns the number of new notifications posted.
async fn poll_and_notify(
    client: &Client,
    my_public_key: PublicKey,
    seen_events: &mut HashSet<EventId>,
) -> usize {
    let now = Timestamp::now();
    let since = Timestamp::from_secs(now.as_secs().saturating_sub(5 * 60));

    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .since(since);

    let mut events = match client
        .stream_events(filter, std::time::Duration::from_secs(15))
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            error!("[BackgroundSync] Failed to stream events: {:?}", e);
            return 0;
        }
    };

    let mut new_notifications = 0usize;

    while let Some(event) = events.next().await {
        // Skip already-seen events
        if !seen_events.insert(event.id) {
            continue;
        }

        // Process through shared pipeline (decrypts, persists to DB, notifies)
        let processed = handle_event_with_context(
            event.clone(), true, client, my_public_key
        ).await;

        if processed {
            new_notifications += 1;
        }
    }

    new_notifications
}

/// Post a notification by calling VectorNotificationService.showMessageNotification() in Kotlin.
/// Uses the stored JavaVM + GlobalRef context (captured during JNI entry) instead of
/// ndk_context, which is NOT initialized in service-only mode (no Tauri Activity).
///
/// Also used by `show_notification_generic` on Android to bypass the Tauri notification plugin.
pub fn post_notification_jni(title: &str, body: &str) {
    // Don't post notifications when the user is actively using the app
    if is_activity_in_foreground() {
        return;
    }
    let vm = match BG_JAVA_VM.get() {
        Some(vm) => vm,
        None => {
            error!("[BackgroundSync] post_notification_jni: JavaVM not stored");
            return;
        }
    };
    let context_ref = match BG_APP_CONTEXT.get() {
        Some(ctx) => ctx,
        None => {
            error!("[BackgroundSync] post_notification_jni: App context not stored");
            return;
        }
    };

    let mut env = match vm.attach_current_thread() {
        Ok(env) => env,
        Err(e) => {
            error!("[BackgroundSync] post_notification_jni: Failed to attach thread: {:?}", e);
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

        env.call_static_method(
            &service_jclass,
            "showMessageNotification",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V",
            &[context.into(), (&jtitle).into(), (&jbody).into()],
        )
        .map_err(|e| format!("Failed to call showMessageNotification: {:?}", e))?;

        info!("[BackgroundSync] Notification posted via JNI");
        Ok(())
    })();

    if let Err(e) = result {
        error!("[BackgroundSync] Failed to post notification: {}", e);
    }
}

// ─── WorkManager fallback (RelayPollWorker) ───────────────────────────────────
// Used when the foreground service is NOT running (e.g., service killed by OS).
// WorkManager polls every 15 minutes as a safety net.

/// Called by RelayPollWorker to poll relays for new messages.
/// This is the WorkManager fallback path — only used when the foreground service
/// is not running and cannot maintain persistent polling.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_RelayPollWorker_pollForNewMessages(
    mut env: JNIEnv,
    this: JObject<'_>,
    data_dir: JString<'_>,
) -> jstring {
    ensure_android_logger();
    info!("[BackgroundSync] WorkManager poll worker triggered");

    // Store JavaVM + context for cross-thread JNI calls (same as foreground service path)
    if BG_JAVA_VM.get().is_none() {
        if let Ok(vm) = env.get_java_vm() {
            let _ = BG_JAVA_VM.set(vm);
        }
    }
    if BG_APP_CONTEXT.get().is_none() {
        if let Ok(ctx) = env.call_method(&this, "getApplicationContext", "()Landroid/content/Context;", &[])
            .and_then(|v| v.l())
        {
            if let Ok(global_ref) = env.new_global_ref(&ctx) {
                let _ = BG_APP_CONTEXT.set(global_ref);
            }
        }
    }

    // If standalone sync is already running in the foreground service, skip
    if STANDALONE_SYNC_RUNNING.load(Ordering::SeqCst) {
        info!("[BackgroundSync] Standalone sync is running, WorkManager poll skipped");
        return make_jstring(&mut env, "");
    }

    let data_dir_str: String = match env.get_string(&data_dir) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("[BackgroundSync] Failed to get dataDir string: {:?}", e);
            return make_jstring(&mut env, "-1");
        }
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("[BackgroundSync] Failed to create tokio runtime: {:?}", e);
            return make_jstring(&mut env, "-1");
        }
    };

    // Quick check for global client
    let client_ready = rt.block_on(async {
        for i in 0..3 {
            if NOSTR_CLIENT.get().is_some() && MY_PUBLIC_KEY.get().is_some() {
                return true;
            }
            if i == 0 {
                info!("[BackgroundSync] Checking for existing Nostr client...");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        false
    });

    if client_ready {
        let client = NOSTR_CLIENT.get().unwrap();
        let pubkey = *MY_PUBLIC_KEY.get().unwrap();
        info!("[BackgroundSync] Using existing Nostr client (full app mode)");
        rt.block_on(async {
            poll_relays_full_app(client, pubkey).await;
        });
        return make_jstring(&mut env, "");
    }

    // Bootstrap pipeline for headless mode
    if let Err(e) = bootstrap_pipeline(&data_dir_str) {
        error!("[BackgroundSync] Failed to bootstrap pipeline for WorkManager: {}", e);
    }

    // Bootstrap and poll (one-shot)
    info!("[BackgroundSync] Bootstrapping standalone client for WorkManager poll");
    let result = rt.block_on(async {
        let (client, my_public_key) = match bootstrap_client(&data_dir_str).await {
            Ok(result) => result,
            Err(e) => {
                error!("[BackgroundSync] Bootstrap failed: {}", e);
                return "-1".to_string();
            }
        };

        let mut seen = HashSet::new();
        let count = poll_and_notify(&client, my_public_key, &mut seen).await;
        client.disconnect().await;

        info!("[BackgroundSync] WorkManager poll complete: {} notifications", count);
        // Return empty — notifications are posted directly via JNI
        String::new()
    });

    make_jstring(&mut env, &result)
}

/// Helper to create a JString, returning null on failure
fn make_jstring(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s)
        .map(|js| js.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Poll relays in full-app mode — uses the existing handle_event pipeline.
async fn poll_relays_full_app(client: &Client, my_public_key: PublicKey) {
    let now = Timestamp::now();
    let since = Timestamp::from_secs(now.as_secs().saturating_sub(20 * 60));

    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .since(since)
        .until(now);

    let mut events = match client
        .stream_events(filter, std::time::Duration::from_secs(30))
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            error!("[BackgroundSync] Failed to stream events: {:?}", e);
            return;
        }
    };

    let mut count = 0u32;
    while let Some(event) = events.next().await {
        crate::services::handle_event(event, false).await;
        count += 1;
    }

    info!("[BackgroundSync] Full-app poll complete, processed {} events", count);
}

/// Check if background sync is currently active (foreground service running)
#[allow(dead_code)]
pub fn is_background_sync_active() -> bool {
    BACKGROUND_SYNC_ACTIVE.load(Ordering::SeqCst)
}
