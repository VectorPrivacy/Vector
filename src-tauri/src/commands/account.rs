//! Account management Tauri commands.
//!
//! This module handles account operations:
//! - Login (nsec or seed phrase import)
//! - Logout (data cleanup and restart)
//! - Account creation (new keypair generation)
//! - Key export (nsec and seed phrase retrieval)
//! - PIN encrypt/decrypt for account security

use nostr_sdk::prelude::*;
use tauri::{AppHandle, Manager, Runtime};

use crate::{STATE, TAURI_APP, NOSTR_CLIENT, MNEMONIC_SEED, ENCRYPTION_KEY, PENDING_INVITE, TRUSTED_RELAYS};
use crate::{Profile, account_manager, db, crypto, commands};

// ============================================================================
// Types
// ============================================================================

/// Key pair returned from login/create_account
#[derive(serde::Serialize, Clone)]
pub struct LoginKeyPair {
    pub public: String,
    pub private: String,
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
    let client = match NOSTR_CLIENT.get() {
        Some(c) => c,
        None => return Err("Backend not initialized - perform normal login".to_string()),
    };

    // Get the current user's public key
    let signer = client.signer().await.map_err(|e| format!("Signer error: {}", e))?;
    let my_npub = signer.get_public_key().await
        .map_err(|e| format!("Public key error: {}", e))?
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

    // Convert chats to serializable format
    let serializable_chats: Vec<_> = state.chats.iter()
        .map(|c| c.to_serializable(&state.interner))
        .collect();
    Ok(serde_json::json!({
        "success": true,
        "npub": my_npub,
        "profiles": &state.profiles,
        "chats": serializable_chats,
        "is_syncing": state.is_syncing,
        "sync_mode": format!("{:?}", state.sync_mode)
    }))
}

/// Login with an existing key (nsec or seed phrase)
#[tauri::command]
pub async fn login(import_key: String) -> Result<LoginKeyPair, String> {
    let keys: Keys;

    // If we're already logged in (i.e: Developer Mode with frontend hot-loading), just return the existing keys.
    if let Some(client) = NOSTR_CLIENT.get() {
        let signer = client.signer().await.unwrap();
        let new_keys = Keys::parse(&import_key).unwrap();

        /* Derive our Public Key from the Import and Existing key sets */
        let prev_npub = signer.get_public_key().await.unwrap().to_bech32().unwrap();
        let new_npub = new_keys.public_key.to_bech32().unwrap();
        if prev_npub == new_npub {
            // Simply return the same KeyPair and allow the frontend to continue login as usual
            return Ok(LoginKeyPair {
                public: prev_npub,
                private: new_keys.secret_key().to_bech32().unwrap(),
            });
        } else {
            // This shouldn't happen in the real-world, but just in case...
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

    // Initialise the Nostr client
    let client = Client::builder()
        .signer(keys.clone())
        .opts(ClientOptions::new())
        .monitor(Monitor::new(1024))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Add our profile (at least, the npub of it) to our state
    let npub = keys.public_key.to_bech32().unwrap();
    let mut profile = Profile::new();
    profile.id = npub.clone();
    profile.mine = true;
    STATE.lock().await.profiles.push(profile);

    // Initialize profile database and set as current account
    // Always init DB - this handles both new and existing accounts:
    // - Creates schema if needed (IF NOT EXISTS)
    // - Runs any pending migrations atomically
    // - Pools the connection for fast subsequent access
    if let Some(handle) = TAURI_APP.get() {
        if let Err(e) = account_manager::init_profile_database(handle, &npub).await {
            eprintln!("[Login] Failed to initialize profile database: {}", e);
        } else if let Err(e) = account_manager::set_current_account(npub.clone()) {
            eprintln!("[Login] Failed to set current account: {}", e);
        } else {
            println!("[Login] Database initialized and account set: {}", npub);
        }
    }

    // Return our npub to the frontend client
    Ok(LoginKeyPair {
        public: npub,
        private: keys.secret_key().to_bech32().unwrap(),
    })
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
#[tauri::command]
pub async fn create_account() -> Result<LoginKeyPair, String> {
    // Generate a BIP39 Mnemonic Seed Phrase
    let mnemonic = bip39::Mnemonic::generate(12).map_err(|e| e.to_string())?;
    let mnemonic_string = mnemonic.to_string();

    // Derive our nsec from our Mnemonic
    let keys = Keys::from_mnemonic(mnemonic_string.clone(), None).map_err(|e| e.to_string())?;

    // Initialise the Nostr client
    let client = Client::builder()
        .signer(keys.clone())
        .opts(ClientOptions::new())
        .monitor(Monitor::new(1024))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Add our profile (at least, the npub of it) to our state
    let npub = keys.public_key.to_bech32().map_err(|e| e.to_string())?;
    let mut profile = Profile::new();
    profile.id = npub.clone();
    profile.mine = true;
    STATE.lock().await.profiles.push(profile);

    // Save the seed in memory, ready for post-pin-setup encryption
    let _ = MNEMONIC_SEED.set(mnemonic_string);

    // Store npub temporarily - database will be created when set_pkey is called (after user sets PIN)
    // This prevents creating "dead accounts" if user quits before setting a PIN
    account_manager::set_pending_account(npub.clone())?;

    // Return the keypair in the same format as the login function
    Ok(LoginKeyPair {
        public: npub,
        private: keys.secret_key().to_bech32().map_err(|e| e.to_string())?,
    })
}

/// Export account keys (nsec and seed phrase if available)
#[tauri::command]
pub async fn export_keys() -> Result<serde_json::Value, String> {
    // Try to get nsec from database first
    let handle = TAURI_APP.get().unwrap();
    let nsec = if let Some(enc_pkey) = db::get_pkey(handle.clone())? {
        // Decrypt the nsec
        match crypto::internal_decrypt(enc_pkey, None).await {
            Ok(decrypted_nsec) => decrypted_nsec,
            Err(_) => return Err("Failed to decrypt nsec".to_string()),
        }
    } else {
        return Err("No nsec found in database".to_string());
    };

    // Try to get seed phrase from memory first
    let seed_phrase = if let Some(seed) = MNEMONIC_SEED.get() {
        Some(seed.clone())
    } else {
        // If not in memory, try to get from database
        if ENCRYPTION_KEY.get().is_some() {
            match db::get_seed(handle.clone()).await {
                Ok(Some(seed)) => Some(seed),
                Ok(None) => None,
                Err(_) => None,
            }
        } else {
            None
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
                        match client.send_event_to(TRUSTED_RELAYS.iter().copied(), &event).await {
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
    // Perform decryption
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
// Handler Registration
// ============================================================================

// Handler list for this module (for reference):
// - login
// - debug_hot_reload_sync (debug only)
// - logout
// - create_account
// - export_keys
// - encrypt
// - decrypt
