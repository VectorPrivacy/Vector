//! Account management Tauri commands.
//!
//! This module handles account operations:
//! - Login (nsec or seed phrase import)
//! - Logout (data cleanup and restart)
//! - Account creation (new keypair generation)
//! - Key export (nsec and seed phrase retrieval)
//! - PIN encrypt/decrypt for account security

use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{STATE, TAURI_APP, NOSTR_CLIENT, MY_KEYS, MY_PUBLIC_KEY, MNEMONIC_SEED, PENDING_NSEC, PENDING_INVITE, active_trusted_relays};
use crate::{Profile, account_manager, db, crypto, commands};

// ============================================================================
// Types
// ============================================================================

/// Public key returned from login/create_account (private key stays backend-only)
#[derive(serde::Serialize, Clone)]
pub struct LoginResult {
    pub public: String,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// # Debug Hot-Reload State Sync
///
/// This command ONLY compiles in debug builds and provides a fast-path for
/// frontend hot-reloads during development. When the frontend hot-reloads,
/// the backend retains all state, so we can skip the entire login/decrypt
/// flow and just bulk-send the existing state back to the frontend.
///
/// Returns:
/// - `Ok(json)` with full state if backend is already initialized
/// - `Err(...)` if backend is not initialized (frontend should do normal login)
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn debug_hot_reload_sync() -> Result<serde_json::Value, String> {
    // Check if we have an active Nostr client (meaning we're already logged in)
    if NOSTR_CLIENT.get().is_none() {
        return Err("Backend not initialized - perform normal login".to_string());
    }

    // Get the current user's public key
    let my_npub = crate::MY_PUBLIC_KEY.get().ok_or("Public key not initialized")?
        .to_bech32()
        .map_err(|e| format!("Bech32 error: {}", e))?;

    // Get the full state
    let state = STATE.lock().await;

    // Verify we have meaningful state (not just an empty initialized state)
    if state.profiles.is_empty() && state.chats.is_empty() {
        return Err("Backend state is empty - perform normal login".to_string());
    }

    // Return the full state for the frontend to hydrate
    println!("[Debug Hot-Reload] Sending cached state to frontend ({} profiles, {} chats)",
             state.profiles.len(), state.chats.len());

    // Convert to serializable formats at the boundary
    let serializable_chats: Vec<_> = state.chats.iter()
        .map(|c| c.to_serializable(&state.interner))
        .collect();
    let slim_profiles: Vec<db::SlimProfile> = state.profiles.iter()
        .map(|p| db::SlimProfile::from_profile(p, &state.interner))
        .collect();
    Ok(serde_json::json!({
        "success": true,
        "npub": my_npub,
        "profiles": slim_profiles,
        "chats": serializable_chats,
        "is_syncing": state.is_syncing,
        "sync_mode": format!("{:?}", state.sync_mode)
    }))
}

/// Login with an existing key (nsec or seed phrase)
///
/// The private key is stored in PENDING_NSEC for setup_encryption/skip_encryption
/// to consume — it is never returned over IPC.
#[tauri::command]
pub async fn login(import_key: String) -> Result<LoginResult, String> {
    let keys: Keys;

    // If we're already logged in (i.e: Developer Mode with frontend hot-loading), just return the existing keys.
    if let Some(_client) = NOSTR_CLIENT.get() {
        let new_keys = Keys::parse(&import_key).unwrap();

        /* Derive our Public Key from the Import and Existing key sets */
        let prev_npub = crate::MY_PUBLIC_KEY.get().expect("Public key not initialized").to_bech32().unwrap();
        let new_npub = new_keys.public_key.to_bech32().unwrap();
        if prev_npub == new_npub {
            return Ok(LoginResult { public: prev_npub });
        } else {
            return Err(String::from("An existing Nostr Client instance exists, but a second incompatible key import was requested."));
        }
    }

    // If it's an nsec, import that
    if import_key.starts_with("nsec") {
        match Keys::parse(&import_key) {
            Ok(parsed) => keys = parsed,
            Err(_) => return Err(String::from("Invalid nsec")),
        };
    } else {
        // Otherwise, we'll try importing it as a mnemonic seed phrase (BIP-39)
        match Keys::from_mnemonic(import_key, Some(String::new())) {
            Ok(parsed) => keys = parsed,
            Err(_) => return Err(String::from("Invalid Seed Phrase")),
        };
    }

    // Store nsec in PENDING_NSEC for setup_encryption/skip_encryption (never sent over IPC)
    {
        let mut pending = PENDING_NSEC.lock().unwrap();
        *pending = Some(keys.secret_key().to_bech32().unwrap());
    }

    // Initialise the Nostr client
    let _ = MY_KEYS.set(keys.clone());
    let _ = MY_PUBLIC_KEY.set(keys.public_key);

    let client = Client::builder()
        .signer(keys.clone())
        .opts(ClientOptions::new())
        .monitor(Monitor::new(1024))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Add our profile (at least, the npub of it) to our state
    let npub = keys.public_key.to_bech32().unwrap();
    let mut profile = Profile::new();
    profile.flags.set_mine(true);
    STATE.lock().await.insert_or_replace_profile(&npub, profile);

    // Initialize profile database and set as current account
    if let Some(handle) = TAURI_APP.get() {
        if let Err(e) = account_manager::init_profile_database(handle, &npub).await {
            eprintln!("[Login] Failed to initialize profile database: {}", e);
            let _ = handle.emit("loading_error", &e);
        } else if let Err(e) = account_manager::set_current_account(npub.clone()) {
            eprintln!("[Login] Failed to set current account: {}", e);
            let _ = handle.emit("loading_error", &e);
        } else {
            println!("[Login] Database initialized and account set: {}", npub);
        }
    }

    Ok(LoginResult { public: npub })
}

/// Logout - clears all data and restarts the app
#[tauri::command]
pub async fn logout<R: Runtime>(handle: AppHandle<R>) {
    // Lock the state to ensure nothing is added to the DB before restart
    let _guard = STATE.lock().await;

    // Close the database connection pool BEFORE attempting to delete files
    // This is critical on Windows where open file handles prevent deletion
    account_manager::close_db_connection();

    // Delete the current account's profile directory (SQL database and MLS data)
    if let Ok(npub) = account_manager::get_current_account() {
        if let Ok(profile_dir) = account_manager::get_profile_directory(&handle, &npub) {
            if profile_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&profile_dir) {
                    eprintln!("[Logout] Failed to remove profile directory: {}", e);
                }
            }
        }
    }

    // Delete the downloads folder (vector folder in Downloads or Documents on iOS)
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };

    if let Ok(downloads_dir) = handle.path().resolve("vector", base_directory) {
        if downloads_dir.exists() {
            let _ = std::fs::remove_dir_all(&downloads_dir);
        }
    }

    // Delete the legacy MLS folder in AppData (for backwards compatibility)
    if let Ok(mls_dir) = handle.path().resolve("mls", tauri::path::BaseDirectory::AppData) {
        if mls_dir.exists() {
            let _ = std::fs::remove_dir_all(&mls_dir);
        }
    }

    // Restart the Core process
    handle.restart();
}

/// Creates a new Nostr keypair derived from a BIP39 Seed Phrase
///
/// The private key is stored in PENDING_NSEC for setup_encryption/skip_encryption
/// to consume — it is never returned over IPC.
#[tauri::command]
pub async fn create_account() -> Result<LoginResult, String> {
    // Generate a BIP39 Mnemonic Seed Phrase
    let mnemonic = bip39::Mnemonic::generate(12).map_err(|e| e.to_string())?;
    let mnemonic_string = mnemonic.to_string();

    // Derive our nsec from our Mnemonic
    let keys = Keys::from_mnemonic(mnemonic_string.clone(), None).map_err(|e| e.to_string())?;

    // Store nsec in PENDING_NSEC for setup_encryption/skip_encryption (never sent over IPC)
    {
        let mut pending = PENDING_NSEC.lock().unwrap();
        *pending = Some(keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    }

    // Initialise the Nostr client
    let _ = MY_KEYS.set(keys.clone());
    let _ = MY_PUBLIC_KEY.set(keys.public_key);

    let client = Client::builder()
        .signer(keys.clone())
        .opts(ClientOptions::new())
        .monitor(Monitor::new(1024))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Add our profile (at least, the npub of it) to our state
    let npub = keys.public_key.to_bech32().map_err(|e| e.to_string())?;
    let mut profile = Profile::new();
    profile.flags.set_mine(true);
    STATE.lock().await.insert_or_replace_profile(&npub, profile);

    // Save the seed in memory, ready for post-pin-setup encryption
    let _ = MNEMONIC_SEED.set(mnemonic_string);

    // Store npub temporarily - database will be created when set_pkey is called (after user sets PIN)
    // This prevents creating "dead accounts" if user quits before setting a PIN
    account_manager::set_pending_account(npub.clone())?;

    Ok(LoginResult { public: npub })
}

/// Export account keys (nsec and seed phrase if available)
#[tauri::command]
pub async fn export_keys() -> Result<serde_json::Value, String> {
    let handle = TAURI_APP.get().unwrap();
    let stored = db::get_pkey(handle.clone())?
        .ok_or("No nsec found in database")?;

    // If encryption is disabled the stored value is already plaintext
    let nsec = if crypto::is_encryption_enabled() {
        crypto::internal_decrypt(stored, None).await
            .map_err(|_| "Failed to decrypt nsec".to_string())?
    } else {
        stored
    };

    // Try to get seed phrase from memory first, then from database
    let seed_phrase = if let Some(seed) = MNEMONIC_SEED.get() {
        Some(seed.clone())
    } else {
        match db::get_seed(handle.clone()).await {
            Ok(Some(seed)) => Some(seed),
            _ => None,
        }
    };

    // Create response object
    let response = serde_json::json!({
        "nsec": nsec,
        "seed_phrase": seed_phrase
    });

    Ok(response)
}

// ============================================================================
// PIN Encryption Commands
// ============================================================================

/// Encrypt data with PIN (used during account setup)
/// Also handles post-encryption tasks like saving seed phrase and broadcasting invite acceptance
#[tauri::command]
pub async fn encrypt(input: String, password: Option<String>) -> String {
    let res = crypto::internal_encrypt(input, password).await;

    // If we have one; save the in-memory seedphrase in an encrypted at-rest format
    match MNEMONIC_SEED.get() {
        Some(seed) => {
            // Save the seed phrase to the database
            let handle = TAURI_APP.get().unwrap();
            let _ = db::set_seed(handle.clone(), seed.to_string()).await;
        }
        _ => ()
    }

    // Check if we have a pending invite acceptance to broadcast
    if let Some(pending_invite) = PENDING_INVITE.get() {
        // Get the Nostr client
        if let Some(client) = NOSTR_CLIENT.get() {
            // Clone the data we need before the async block
            let invite_code = pending_invite.invite_code.clone();
            let inviter_pubkey = pending_invite.inviter_pubkey.clone();

            // Spawn the broadcast in a separate task to avoid blocking
            tokio::spawn(async move {
                // Create and publish the acceptance event
                let event_builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite_accepted")
                    .tag(Tag::custom(TagKind::Custom("l".into()), vec!["vector"]))
                    .tag(Tag::custom(TagKind::Custom("d".into()), vec![invite_code.as_str()]))
                    .tag(Tag::public_key(inviter_pubkey));

                // Build the event
                match client.sign_event_builder(event_builder).await {
                    Ok(event) => {
                        // Send only to trusted relays
                        match client.send_event_to(active_trusted_relays().await.into_iter(), &event).await {
                            Ok(_) => println!("Successfully broadcast invite acceptance to trusted relays"),
                            Err(e) => eprintln!("Failed to broadcast invite acceptance: {}", e),
                        }
                    }
                    Err(e) => eprintln!("Failed to sign invite acceptance event: {}", e),
                }
            });
        }
    }

    // Bootstrap MLS device keypackage for newly created accounts (non-blocking)
    // This ensures keypackages are published immediately after PIN setup, not just on restart
    tokio::spawn(async move {
        // Brief delay to allow encryption key to be set
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        // Skip if no account selected (migration pending)
        if crate::account_manager::get_current_account().is_err() {
            println!("[MLS] Skipping KeyPackage bootstrap - no account selected (migration may be pending)");
            return;
        }

        // Skip if a forced regen is pending (connect() post-connect handler owns this)
        let handle = match TAURI_APP.get() {
            Some(h) => h.clone(),
            None => return,
        };
        let force_pending = db::get_sql_setting(handle, "mls_force_keypackage_regen".into())
            .ok().flatten().map(|v| v == "1").unwrap_or(false);
        if force_pending {
            println!("[MLS] Skipping cached KeyPackage bootstrap — forced regen pending (connect handler)");
            return;
        }

        println!("[MLS] Ensuring persistent device KeyPackage after PIN setup...");
        match commands::mls::regenerate_device_keypackage(true).await {
            Ok(info) => {
                let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
            }
            Err(e) => println!("[MLS] Device KeyPackage bootstrap FAILED: {}", e),
        }
    });

    res
}

/// Decrypt data with PIN (used during login)
/// Also handles post-decryption tasks like MLS keypackage bootstrap
#[tauri::command]
pub async fn decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    let res = crypto::internal_decrypt(ciphertext, password).await;

    // On success, ensure persistent device KeyPackage and run non-blocking smoke test
    if res.is_ok() {
        // Best-effort persistent device KeyPackage bootstrap (non-blocking)
        tokio::spawn(async move {
            // brief delay to allow any post-login setup to settle
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;

            // Skip if no account selected (migration pending)
            if crate::account_manager::get_current_account().is_err() {
                println!("[MLS] Skipping KeyPackage bootstrap - no account selected (migration may be pending)");
                return;
            }

            // Skip if a forced regen is pending (connect() post-connect handler owns this)
            let handle = match TAURI_APP.get() {
                Some(h) => h.clone(),
                None => return,
            };
            let force_pending = db::get_sql_setting(handle, "mls_force_keypackage_regen".into())
                .ok().flatten().map(|v| v == "1").unwrap_or(false);
            if force_pending {
                println!("[MLS] Skipping cached KeyPackage bootstrap — forced regen pending (connect handler)");
                return;
            }

            println!("[MLS] Ensuring persistent device KeyPackage...");
            match commands::mls::regenerate_device_keypackage(true).await {
                Ok(info) => {
                    let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                    let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                    println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
                }
                Err(e) => println!("[MLS] Device KeyPackage bootstrap FAILED: {}", e),
            }
        });
    }

    res
}

// ============================================================================
// Backend-Only Key Management (C7 fix: keys never cross IPC)
// ============================================================================

/// Login using the stored private key (boot flow).
/// Reads pkey from DB, decrypts if needed, initializes the Nostr client.
/// Returns only the npub — the private key never crosses IPC.
#[tauri::command]
pub async fn login_from_stored_key(password: Option<String>) -> Result<String, String> {
    // If already logged in, just return the npub
    if let Some(_client) = NOSTR_CLIENT.get() {
        let npub = crate::MY_PUBLIC_KEY.get().ok_or("Public key not initialized")?
            .to_bech32()
            .map_err(|e| format!("Bech32 error: {}", e))?;
        return Ok(npub);
    }

    let handle = TAURI_APP.get().ok_or("App not initialized")?;
    let stored_pkey = db::get_pkey(handle.clone())?
        .ok_or("No private key found")?;

    // Decrypt if password provided
    let nsec = if let Some(pwd) = password {
        crypto::internal_decrypt(stored_pkey, Some(pwd)).await
            .map_err(|_| "Incorrect password".to_string())?
    } else {
        stored_pkey
    };

    // Initialize Client with the key
    let keys = Keys::parse(&nsec).map_err(|_| "Invalid stored key".to_string())?;

    let _ = MY_KEYS.set(keys.clone());
    let _ = MY_PUBLIC_KEY.set(keys.public_key);

    let client = Client::builder()
        .signer(keys.clone())
        .opts(ClientOptions::new())
        .monitor(Monitor::new(1024))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    let npub = keys.public_key.to_bech32().map_err(|e| e.to_string())?;
    let mut profile = Profile::new();
    profile.flags.set_mine(true);
    STATE.lock().await.insert_or_replace_profile(&npub, profile);

    // Initialize profile database
    if let Err(e) = account_manager::init_profile_database(handle, &npub).await {
        eprintln!("[Login] Failed to initialize profile database: {}", e);
        let _ = handle.emit("loading_error", &e);
    } else if let Err(e) = account_manager::set_current_account(npub.clone()) {
        eprintln!("[Login] Failed to set current account: {}", e);
        let _ = handle.emit("loading_error", &e);
    }

    // MLS keypackage bootstrap (non-blocking, same as decrypt command)
    spawn_mls_bootstrap();

    Ok(npub)
}

/// Set up encryption for a new account (PIN or password flow).
/// Consumes the nsec from PENDING_NSEC, encrypts it, and stores it.
/// The private key never crosses IPC.
#[tauri::command]
pub async fn setup_encryption<R: Runtime>(
    handle: AppHandle<R>,
    password: String,
    security_type: String,
) -> Result<(), String> {
    // Take the pending nsec (consume it — can't be retrieved again)
    let nsec = PENDING_NSEC.lock().unwrap().take()
        .ok_or("No pending key — call create_account or login first")?;

    // Encrypt the key with the user's password
    let encrypted = crypto::internal_encrypt(nsec, Some(password)).await;

    // Store via set_pkey (handles pending account DB creation + MLS bootstrap)
    db::set_pkey(handle.clone(), encrypted).await?;

    // Set security type and ensure encryption flag is cached
    db::set_sql_setting(handle.clone(), "security_type".to_string(), security_type)?;
    crate::state::set_encryption_enabled(true);

    // Save seed phrase if available
    if let Some(seed) = MNEMONIC_SEED.get() {
        let _ = db::set_seed(handle.clone(), seed.to_string()).await;
    }

    // Broadcast pending invite acceptance
    if let Some(pending_invite) = PENDING_INVITE.get() {
        if let Some(client) = NOSTR_CLIENT.get() {
            let invite_code = pending_invite.invite_code.clone();
            let inviter_pubkey = pending_invite.inviter_pubkey;
            tokio::spawn(async move {
                let event_builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite_accepted")
                    .tag(Tag::custom(TagKind::Custom("d".into()), vec![invite_code.as_str()]))
                    .tag(Tag::public_key(inviter_pubkey));
                match client.sign_event_builder(event_builder).await {
                    Ok(event) => {
                        match client.send_event_to(active_trusted_relays().await.into_iter(), &event).await {
                            Ok(_) => println!("Successfully broadcast invite acceptance to trusted relays"),
                            Err(e) => eprintln!("Failed to broadcast invite acceptance: {}", e),
                        }
                    }
                    Err(e) => eprintln!("Failed to sign invite acceptance event: {}", e),
                }
            });
        }
    }

    // MLS keypackage bootstrap (non-blocking)
    spawn_mls_bootstrap();

    Ok(())
}

/// Skip encryption for a new account — stores the key in plaintext.
/// Consumes the nsec from PENDING_NSEC. The private key never crosses IPC.
#[tauri::command]
pub async fn skip_encryption<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    // Take the pending nsec (consume it — can't be retrieved again)
    let nsec = PENDING_NSEC.lock().unwrap().take()
        .ok_or("No pending key — call create_account or login first")?;

    // Store plaintext (handles pending account DB creation + MLS bootstrap)
    db::set_pkey(handle.clone(), nsec).await?;

    // Mark encryption as disabled
    db::set_sql_setting(handle.clone(), "encryption_enabled".to_string(), "false".to_string())?;
    crate::state::set_encryption_enabled(false);

    // Save seed phrase if available (stored plaintext since encryption is disabled)
    if let Some(seed) = MNEMONIC_SEED.get() {
        let _ = db::set_seed(handle.clone(), seed.to_string()).await;
    }

    Ok(())
}

/// Shared MLS keypackage bootstrap (non-blocking, used by login_from_stored_key and setup_encryption)
fn spawn_mls_bootstrap() {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        if crate::account_manager::get_current_account().is_err() {
            println!("[MLS] Skipping KeyPackage bootstrap - no account selected");
            return;
        }

        let handle = match TAURI_APP.get() {
            Some(h) => h.clone(),
            None => return,
        };
        let force_pending = db::get_sql_setting(handle, "mls_force_keypackage_regen".into())
            .ok().flatten().map(|v| v == "1").unwrap_or(false);
        if force_pending {
            println!("[MLS] Skipping cached KeyPackage bootstrap — forced regen pending");
            return;
        }

        println!("[MLS] Ensuring persistent device KeyPackage...");
        match commands::mls::regenerate_device_keypackage(true).await {
            Ok(info) => {
                let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
            }
            Err(e) => println!("[MLS] Device KeyPackage bootstrap FAILED: {}", e),
        }
    });
}

// ============================================================================
// Handler Registration
// ============================================================================

// Handler list for this module (for reference):
// - login
// - login_from_stored_key
// - debug_hot_reload_sync (debug only)
// - logout
// - create_account
// - setup_encryption
// - skip_encryption
// - export_keys
// - encrypt
// - decrypt
