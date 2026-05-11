//! Account management Tauri commands.
//!
//! This module handles account operations:
//! - Login (nsec or seed phrase import)
//! - Logout (data cleanup and restart)
//! - Account creation (new keypair generation)
//! - Key export (nsec and seed phrase retrieval)
//! - PIN encrypt/decrypt for account security

use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Runtime};
use zeroize::Zeroize;

use std::sync::atomic::AtomicBool;
use crate::{STATE, TAURI_APP, NOSTR_CLIENT, nostr_client, set_my_public_key, MY_SECRET_KEY, MNEMONIC_SEED, PENDING_NSEC, active_trusted_relays};
use crate::{Profile, account_manager, db, crypto, commands};

/// Set to true after a full foreground login+sync flow completes.
/// Prevents debug_hot_reload_sync from using partial state preloaded by standalone background sync.
pub(crate) static FULL_SESSION_INITIALIZED: AtomicBool = AtomicBool::new(false);

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
    if nostr_client().is_none() {
        return Err("Backend not initialized - perform normal login".to_string());
    }

    // Get the current user's public key
    let my_npub = crate::my_public_key().ok_or("Public key not initialized")?
        .to_bech32()
        .map_err(|e| format!("Bech32 error: {}", e))?;

    // Get the full state
    let state = STATE.lock().await;

    // Verify we have state from a full app session, not partial state from
    // standalone background sync (which only preloads MLS groups + notification profiles).
    if state.profiles.is_empty() && state.chats.is_empty() {
        return Err("Backend state is empty - perform normal login".to_string());
    }
    if !FULL_SESSION_INITIALIZED.load(std::sync::atomic::Ordering::Acquire) {
        return Err("State is from background sync, not a full session - perform normal login".to_string());
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
        "is_syncing": state.is_syncing
    }))
}

/// Login with an existing key (nsec or seed phrase)
///
/// The private key is stored in PENDING_NSEC for setup_encryption/skip_encryption
/// to consume — it is never returned over IPC.
#[tauri::command]
pub async fn login(mut import_key: String) -> Result<LoginResult, String> {
    let keys: Keys;

    // If we're already logged in (i.e: Developer Mode with frontend hot-loading), just return the existing keys.
    if let Some(_client) = nostr_client() {
        // Validated user input — propagate the parse error instead of
        // panicking on malformed nsec/mnemonic.
        let new_keys = Keys::parse(&import_key)
            .map_err(|_| String::from("Invalid key — could not parse"))?;

        /* Derive our Public Key from the Import and Existing key sets.
         * Both bech32 conversions are infallible for a valid PublicKey, but
         * we surface the error rather than panic to keep the function
         * panic-free under partial-reset / swap-race conditions. */
        let prev_npub = crate::my_public_key()
            .ok_or("Public key not initialized")?
            .to_bech32()
            .map_err(|e| format!("Bech32 error: {}", e))?;
        let new_npub = new_keys.public_key.to_bech32()
            .map_err(|e| format!("Bech32 error: {}", e))?;
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
        // Zeroize the nsec string — Keys struct has the parsed data
        import_key.zeroize();
    } else {
        // Otherwise, we'll try importing it as a mnemonic seed phrase (BIP-39)
        // from_mnemonic takes ownership, so clone for zeroize
        let mnemonic_copy = import_key.clone();
        import_key.zeroize();
        match Keys::from_mnemonic(mnemonic_copy, Some(String::new())) {
            Ok(parsed) => keys = parsed,
            Err(_) => return Err(String::from("Invalid Seed Phrase")),
        };
    }

    // Store nsec in PENDING_NSEC for setup_encryption/skip_encryption (never sent over IPC)
    {
        let mut pending = PENDING_NSEC.lock().unwrap();
        *pending = Some(keys.secret_key().to_bech32().unwrap());
    }

    // Store secret key in the guarded vault, then construct the client with GuardedSigner
    let public_key = keys.public_key;
    MY_SECRET_KEY.store_from_keys(&keys, &[&crate::ENCRYPTION_KEY]);
    set_my_public_key(public_key);
    drop(keys); // Drop the Keys struct (secp256k1 Drop zeroizes)

    let client = Client::builder()
        .signer(vector_core::GuardedSigner::new(public_key))
        .opts(vector_core::nostr_client_options())
        .monitor(Monitor::new(1024))
        .build();
    // The standalone background-sync path on Android can install a client
    // before the Activity reaches this login command. The early-return guard
    // above catches the common case, but a concurrent install between guard
    // and here is possible. Take the write lock once, install only if the
    // slot is empty, and otherwise drop our just-built client.
    {
        let mut slot = NOSTR_CLIENT.write().unwrap();
        if slot.is_some() {
            eprintln!("[Login] NOSTR_CLIENT was set concurrently; reusing existing instance.");
        } else {
            *slot = Some(client);
        }
    }

    // Add our profile (at least, the npub of it) to our state
    let npub = public_key.to_bech32().unwrap();
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

    FULL_SESSION_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
    Ok(LoginResult { public: npub })
}

/// Logout — wipes the active account's data, clears every per-session
/// global, and tells the frontend to reload. Universal path: never calls
/// `app.restart()` because mobile platforms can't rely on it.
///
/// Returns `Err` if an encryption migration is mid-flight; the user can
/// retry once it settles. Proceeding regardless would tear the DB pool
/// out from under the open transaction and leave `migration_state` stuck
/// in `encrypting`/`decrypting`, bricking the account on next boot.
#[tauri::command]
pub async fn logout<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    use tauri::Emitter;

    // Surface the migration-gate refusal under the logout label before
    // delegating; `delete_account` re-checks but its message says
    // "delete the active account" which would mislead a user who clicked
    // Logout.
    account_manager::refuse_if_migration_in_progress("log out")?;

    let active_npub = account_manager::get_current_account()
        .map_err(|_| "Not logged in".to_string())?;

    // `delete_account` handles: marker clear, reset_session, dir wipe,
    // and the last-account cascade (downloads + legacy mls dirs).
    //
    // Emit `session_reload` UNCONDITIONALLY — if delete_account errors
    // AFTER reset_session has run (e.g. Windows AV held a handle on a
    // sub-path), the backend is already in a half-torn-down state and
    // the frontend MUST reload or it sits on a dead UI.
    let result = account_manager::delete_account(handle.clone(), active_npub).await;
    let _ = handle.emit("session_reload", ());
    result.map(|_| ())
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

    // Store secret key in the guarded vault, then construct the client with GuardedSigner
    let public_key = keys.public_key;
    MY_SECRET_KEY.store_from_keys(&keys, &[&crate::ENCRYPTION_KEY]);
    set_my_public_key(public_key);
    drop(keys);

    let client = Client::builder()
        .signer(vector_core::GuardedSigner::new(public_key))
        .opts(vector_core::nostr_client_options())
        .monitor(Monitor::new(1024))
        .build();
    // The standalone background-sync path on Android can install a client
    // before the Activity reaches this login command. The early-return guard
    // above catches the common case, but a concurrent install between guard
    // and here is possible. Take the write lock once, install only if the
    // slot is empty, and otherwise drop our just-built client.
    {
        let mut slot = NOSTR_CLIENT.write().unwrap();
        if slot.is_some() {
            eprintln!("[Login] NOSTR_CLIENT was set concurrently; reusing existing instance.");
        } else {
            *slot = Some(client);
        }
    }

    // Add our profile (at least, the npub of it) to our state
    let npub = public_key.to_bech32().map_err(|e| e.to_string())?;
    let mut profile = Profile::new();
    profile.flags.set_mine(true);
    STATE.lock().await.insert_or_replace_profile(&npub, profile);

    // Save the seed in memory, ready for post-pin-setup encryption
    *MNEMONIC_SEED.lock().unwrap() = Some(mnemonic_string);

    // Store npub temporarily - database will be created when set_pkey is called (after user sets PIN)
    // This prevents creating "dead accounts" if user quits before setting a PIN
    account_manager::set_pending_account(npub.clone())?;

    FULL_SESSION_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
    Ok(LoginResult { public: npub })
}

/// Export account keys (nsec and seed phrase if available)
#[tauri::command]
pub async fn export_keys() -> Result<serde_json::Value, String> {
    let stored = db::get_pkey()?
        .ok_or("No nsec found in database")?;

    // If encryption is disabled the stored value is already plaintext.
    // Use the atomic fast-path: it's seeded by `init_encryption_enabled()`
    // through the canonical resolver, so we agree with every other call
    // site about the missing-row case.
    let nsec = if vector_core::state::is_encryption_enabled_fast() {
        crypto::internal_decrypt(stored, None).await
            .map_err(|_| "Failed to decrypt nsec".to_string())?
    } else {
        stored
    };

    // Try to get seed phrase from memory first, then from database
    let seed_from_mem = MNEMONIC_SEED.lock().unwrap().clone();
    let seed_phrase = if seed_from_mem.is_some() {
        seed_from_mem
    } else {
        match db::get_seed().await {
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
    let seed_copy = MNEMONIC_SEED.lock().unwrap().clone();
    if let Some(seed) = seed_copy {
        let _ = db::set_seed(seed).await;
    }

    // Check if we have a pending invite acceptance to broadcast
    if let Some(pending_invite) = crate::state::pending_invite() {
        // Consume the slot up-front so a re-entry of this code path doesn't
        // re-broadcast the same invite. The spawned task owns the data.
        crate::state::clear_pending_invite();

        // Get the Nostr client
        if let Some(client) = nostr_client() {
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
    // This ensures keypackages are published immediately after PIN setup, not just on restart.
    // SessionGuard mirrors the pattern used by setup_encryption / skip_encryption
    // bootstraps — keeps every keypackage spawn consistent.
    let bootstrap_session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        // Brief delay to allow encryption key to be set
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        if !bootstrap_session.is_valid() { return; }

        // Skip if no account selected (migration pending)
        if crate::account_manager::get_current_account().is_err() {
            println!("[MLS] Skipping KeyPackage bootstrap - no account selected (migration may be pending)");
            return;
        }

        // Skip if a forced regen is pending (connect() post-connect handler owns this)
        let force_pending = db::get_sql_setting("mls_force_keypackage_regen".into())
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
        // Best-effort persistent device KeyPackage bootstrap (non-blocking).
        // SessionGuard for consistency with the other keypackage bootstraps.
        let bootstrap_session = vector_core::state::SessionGuard::capture();
        tokio::spawn(async move {
            // brief delay to allow any post-login setup to settle
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;

            if !bootstrap_session.is_valid() { return; }

            // Skip if no account selected (migration pending)
            if crate::account_manager::get_current_account().is_err() {
                println!("[MLS] Skipping KeyPackage bootstrap - no account selected (migration may be pending)");
                return;
            }

            // Skip if a forced regen is pending (connect() post-connect handler owns this)
            let force_pending = db::get_sql_setting("mls_force_keypackage_regen".into())
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
///
/// Self-heals the "Back-after-Add-Profile commit" race: if a stale
/// `NOSTR_CLIENT` (from an aborted new-account creation) holds account B's
/// keys while `CURRENT_ACCOUNT` points at account A, the early-return below
/// would happily return account B's npub and the frontend would silently
/// switch identities. Detect the mismatch up-front and force a full
/// `reset_session()` to clean the slate before re-running the cold path.
#[tauri::command]
pub async fn login_from_stored_key(password: Option<String>) -> Result<String, String> {
    // Defense-in-depth: seed the encryption atomic from the current
    // account's DB at the top of every login. boot_select_account also
    // seeds, but the atomic is process-wide and could be stale after a
    // reset_session() that left it at the post-reset `false`. The cost is
    // one atomic store + one DB read of a small settings row.
    crate::state::init_encryption_enabled();

    // Identity-mismatch self-heal: if NOSTR_CLIENT is set but its npub
    // disagrees with the active marker, the previous session was a
    // half-committed Add Profile that the user backed out of without a
    // session-reload. Reset before continuing; the marker is authoritative.
    if let Some(in_memory_pk) = crate::my_public_key() {
        let in_memory_npub = in_memory_pk.to_bech32().ok();
        let marker_npub = vector_core::db::read_active_account_file().ok().flatten()
            .or_else(|| crate::account_manager::get_current_account().ok());
        match (in_memory_npub, marker_npub) {
            (Some(in_mem), Some(marker)) if in_mem != marker => {
                crate::account_manager::refuse_if_migration_in_progress(
                    "recover identity mismatch",
                )?;
                eprintln!(
                    "[Login] In-memory session ({}…) disagrees with marker ({}…) — forcing reset.",
                    &in_mem[..14.min(in_mem.len())],
                    &marker[..14.min(marker.len())],
                );
                crate::account_manager::reset_session().await;
                // NOSTR_CLIENT is now None; we fall through to the cold
                // path which decrypts the marker account's stored key.
            }
            _ => {}
        }
    }

    // Already logged in (Android standalone bg-sync installed NOSTR_CLIENT
    // before the Activity attached): clear stale state and let the
    // foreground rebuild fresh. GuardedSigner + MY_SECRET_KEY remain valid.
    if let Some(client) = nostr_client() {
        // bg-sync's relays are minimal; the frontend's `connect()` will
        // add the full set.
        let stale_relays: Vec<String> = client.relays().await
            .keys().map(|u| u.to_string()).collect();
        for url in &stale_relays {
            let _ = client.remove_relay(url.as_str()).await;
        }

        // bg-sync only preloads MLS groups + notification profiles —
        // partial state that would trick `debug_hot_reload_sync` into
        // skipping login and showing only group chats. Clear it so the
        // frontend goes through the full boot flow.
        {
            let mut state = STATE.lock().await;
            state.profiles.clear();
            state.chats.clear();
        }

        if !stale_relays.is_empty() {
            println!("[Login] Cleared {} stale relay(s) and partial state from background sync", stale_relays.len());
        }

        // bg-sync never had the password, so ENCRYPTION_KEY is empty.
        // Derive it now so `maybe_decrypt` works. MY_SECRET_KEY is
        // already valid from the bg-sync install.
        if let Some(pwd) = password {
            if vector_core::state::is_encryption_enabled_fast() && !crate::ENCRYPTION_KEY.has_key() {
                let key = crypto::hash_pass(pwd).await;
                crate::ENCRYPTION_KEY.set(key, &[&crate::MY_SECRET_KEY]);
            }
        }

        let npub = crate::my_public_key().ok_or("Public key not initialized")?
            .to_bech32()
            .map_err(|e| format!("Bech32 error: {}", e))?;
        return Ok(npub);
    }

    let handle = TAURI_APP.get().ok_or("App not initialized")?;
    let stored_pkey = db::get_pkey()?
        .ok_or("No private key found")?;

    // Decrypt if password provided
    let mut nsec = if let Some(pwd) = password {
        crypto::internal_decrypt(stored_pkey, Some(pwd)).await
            .map_err(|_| "Incorrect password".to_string())?
    } else {
        stored_pkey
    };

    // Initialize Client with the key, then zeroize the plaintext nsec
    let keys = Keys::parse(&nsec).map_err(|_| "Invalid stored key".to_string())?;
    nsec.zeroize();

    let public_key = keys.public_key;
    MY_SECRET_KEY.store_from_keys(&keys, &[&crate::ENCRYPTION_KEY]);
    set_my_public_key(public_key);
    drop(keys);

    // If the user previously enabled Tor, bootstrap it BEFORE building the
    // Nostr client so the client picks up the SOCKS proxy from the start.
    // First boot takes 5–15s for the consensus fetch; subsequent ~2s from
    // the cached directory under <account>/tor/. Failures fall through to
    // a direct connection so the app still boots.
    #[cfg(feature = "tor")]
    {
        let tor_enabled = matches!(
            vector_core::db::settings::get_sql_setting("tor_enabled".to_string()),
            Ok(Some(ref v)) if v == "1" || v == "true"
        );
        if tor_enabled && !vector_core::tor::is_active() {
            match crate::commands::tor::tor_set_enabled(true).await {
                Ok(_) => println!("[Login] Tor service started from saved preference."),
                Err(e) => eprintln!("[Login] Tor auto-start failed: {} — proceeding direct.", e),
            }
        }
    }

    let client = Client::builder()
        .signer(vector_core::GuardedSigner::new(public_key))
        .opts(vector_core::nostr_client_options())
        .monitor(Monitor::new(1024))
        .build();
    // The standalone background-sync path on Android can install a client
    // before the Activity reaches this login command. The early-return guard
    // above catches the common case, but a concurrent install between guard
    // and here is possible. Take the write lock once, install only if the
    // slot is empty, and otherwise drop our just-built client.
    {
        let mut slot = NOSTR_CLIENT.write().unwrap();
        if slot.is_some() {
            eprintln!("[Login] NOSTR_CLIENT was set concurrently; reusing existing instance.");
        } else {
            *slot = Some(client);
        }
    }

    let npub = public_key.to_bech32().map_err(|e| e.to_string())?;
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
    } else {
        let _ = account_manager::touch_last_active();
        // Re-seed the encryption atomic from this account's DB; the
        // self-heal path above (when triggered) called `reset_session`
        // which left the atomic at `false`. Without this re-seed,
        // `is_encryption_enabled_fast()` returns stale `false` and
        // `maybe_encrypt`/`maybe_decrypt` silently bypass encryption.
        crate::state::init_encryption_enabled();
    }

    // MLS keypackage bootstrap (non-blocking, same as decrypt command)
    spawn_mls_bootstrap();

    FULL_SESSION_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
    Ok(npub)
}

/// Set up encryption for a new account (PIN or password flow).
/// Consumes the nsec from PENDING_NSEC, encrypts it, stores it. Private
/// key never crosses IPC.
///
/// Crash-safety: pkey + `encryption_enabled` + `security_type` +
/// (optional) encrypted seed land in a single SQLite transaction —
/// either all or none. A half-applied write would brick the account
/// because the next boot reads ciphertext as plaintext nsec.
///
/// Retry-safety: PENDING_NSEC is only cleared after the tx commits.
/// A `.take()` up-front would lose the freshly generated key on any
/// transient DB failure.
#[tauri::command]
pub async fn setup_encryption<R: Runtime>(
    handle: AppHandle<R>,
    password: String,
    security_type: String,
) -> Result<(), String> {
    use zeroize::{Zeroize, Zeroizing};

    // Defense in depth — frontend enforces minimum length, but a hostile
    // IPC caller passing "" would otherwise produce an encrypted account
    // whose key is `hash_pass("")`, unlockable by anyone passing "".
    if password.trim().is_empty() {
        return Err("Password must not be empty.".to_string());
    }

    // Zeroize wrapper scrubs the heap on Drop along every exit path.
    // `hash_pass` borrows by `&str` and doesn't zeroize what it borrows,
    // so without this the plaintext password would survive on the heap.
    let password = Zeroizing::new(password);

    // Clone (not take) so a transient DB failure leaves the original
    // PENDING_NSEC intact for retry. The Zeroizing wrapper scrubs the
    // clone on Drop regardless of which `?` propagates Err.
    let nsec = Zeroizing::new(
        PENDING_NSEC.lock().unwrap().clone()
            .ok_or("No pending key — call create_account or login first")?
    );

    // internal_encrypt zeroizes the plaintext it owns; we hand it a
    // clone so the wrapped original stays valid for error paths.
    let encrypted = crypto::internal_encrypt((*nsec).clone(), Some((*password).clone())).await;

    // Encrypt the seed (if any) BEFORE the tx so the transaction stays
    // short. Zeroizing wrapper scrubs the plaintext mnemonic on Drop.
    let seed_plain: Option<Zeroizing<String>> =
        MNEMONIC_SEED.lock().unwrap().clone().map(Zeroizing::new);
    let encrypted_seed = if let Some(ref s) = seed_plain {
        Some(crate::crypto::maybe_encrypt((**s).clone()).await)
    } else {
        None
    };

    // For a fresh account: create the DB, set CURRENT_ACCOUNT, restart
    // Tor against the new account's saved pref. Tor is stopped up-front
    // so the new account's per-account Tor cache hydrates cleanly; if
    // `init_profile_database` then fails, restart Tor against the still-
    // active account so a retry doesn't leave the user direct-connected.
    if let Ok(Some(npub)) = crate::account_manager::get_pending_account() {
        crate::commands::tor::stop_and_join_if_running().await;
        if let Err(e) = crate::account_manager::init_profile_database(&handle, &npub).await {
            let _ = crate::commands::tor::sync_to_active_account().await;
            return Err(e);
        }
        crate::account_manager::set_current_account(npub)?;
        crate::account_manager::clear_pending_account()?;
        if let Err(e) = crate::commands::tor::sync_to_active_account().await {
            eprintln!("[Account] Tor start for new account failed: {}", e);
        }
    }

    // pkey + encryption_enabled + security_type + seed in one tx. Err
    // rolls back, leaving the new DB with no setup rows so retry is clean.
    vector_core::db::settings::commit_account_setup(
        &encrypted,
        true,
        Some(&security_type),
        encrypted_seed.as_deref(),
    )?;

    // Persistent record committed — zeroize in-memory secrets. Globals
    // need explicit zeroize because the slot can be overwritten without
    // dropping the inner String; the `Zeroizing<…>` locals scrub on Drop.
    {
        let mut pending = PENDING_NSEC.lock().unwrap();
        if let Some(ref mut s) = *pending { s.zeroize(); }
        *pending = None;
    }
    {
        let mut guard = MNEMONIC_SEED.lock().unwrap();
        if let Some(ref mut s) = *guard { s.zeroize(); }
        *guard = None;
    }
    drop(nsec);
    drop(seed_plain);

    crate::state::set_encryption_enabled(true);

    // MLS keypackage bootstrap.
    let bootstrap_session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if !bootstrap_session.is_valid() { return; }
        match crate::commands::mls::regenerate_device_keypackage(true).await {
            Ok(info) => {
                let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
            }
            Err(e) => println!("[MLS] Device KeyPackage bootstrap FAILED: {}", e),
        }
    });

    // Broadcast pending invite acceptance — consume up-front so a re-entry
    // can't re-broadcast the same invite.
    broadcast_pending_invite_if_any();

    Ok(())
}

/// Skip encryption for a new account — stores the key in plaintext.
/// Consumes the nsec from PENDING_NSEC only after the DB commit succeeds.
/// The private key never crosses IPC.
#[tauri::command]
pub async fn skip_encryption<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    use zeroize::{Zeroize, Zeroizing};

    // Clone (NOT take) into a Zeroizing wrapper so a transient failure
    // both (a) leaves the key recoverable in PENDING_NSEC and (b) scrubs
    // the heap copy regardless of which `?` propagates Err.
    let nsec = Zeroizing::new(
        PENDING_NSEC.lock().unwrap().clone()
            .ok_or("No pending key — call create_account or login first")?
    );

    let seed_plain: Option<Zeroizing<String>> =
        MNEMONIC_SEED.lock().unwrap().clone().map(Zeroizing::new);
    // Route through maybe_encrypt even though it's a no-op when encryption
    // is off — keeps the on-disk seed format consistent with the encrypted
    // flow so migrations don't have to special-case unencrypted accounts.
    let encrypted_seed = if let Some(ref s) = seed_plain {
        Some(crate::crypto::maybe_encrypt((**s).clone()).await)
    } else {
        None
    };

    if let Ok(Some(npub)) = crate::account_manager::get_pending_account() {
        crate::commands::tor::stop_and_join_if_running().await;
        if let Err(e) = crate::account_manager::init_profile_database(&handle, &npub).await {
            let _ = crate::commands::tor::sync_to_active_account().await;
            return Err(e);
        }
        crate::account_manager::set_current_account(npub)?;
        crate::account_manager::clear_pending_account()?;
        if let Err(e) = crate::commands::tor::sync_to_active_account().await {
            eprintln!("[Account] Tor start for new account failed: {}", e);
        }
    }

    vector_core::db::settings::commit_account_setup(
        &nsec,
        false,
        None,
        encrypted_seed.as_deref(),
    )?;

    // Persistent record committed — zeroize in-memory secrets.
    {
        let mut pending = PENDING_NSEC.lock().unwrap();
        if let Some(ref mut s) = *pending { s.zeroize(); }
        *pending = None;
    }
    {
        let mut guard = MNEMONIC_SEED.lock().unwrap();
        if let Some(ref mut s) = *guard { s.zeroize(); }
        *guard = None;
    }
    drop(nsec);
    drop(seed_plain);

    crate::state::set_encryption_enabled(false);

    // MLS keypackage bootstrap.
    let bootstrap_session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if !bootstrap_session.is_valid() { return; }
        match crate::commands::mls::regenerate_device_keypackage(true).await {
            Ok(info) => {
                let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
            }
            Err(e) => println!("[MLS] Device KeyPackage bootstrap FAILED: {}", e),
        }
    });

    // Broadcast pending invite acceptance.
    broadcast_pending_invite_if_any();

    Ok(())
}

/// Broadcast a pending invite acceptance event to trusted relays.
/// First-invite-wins: the invite is consumed before the spawn so a
/// double-fire of `setup_encryption`/`skip_encryption` can't broadcast
/// twice. SessionGuard ensures a mid-flight swap drops the publish
/// instead of signing under the wrong key.
fn broadcast_pending_invite_if_any() {
    // Order matters: peek → check client → THEN clear. Clearing before
    // the client check would silently drop invites during the transient
    // window where setup_encryption returns before `connect()` populates
    // NOSTR_CLIENT.
    let Some(pending_invite) = crate::state::pending_invite() else { return; };
    let Some(client) = nostr_client() else { return; };
    crate::state::clear_pending_invite();
    let invite_code = pending_invite.invite_code;
    let inviter_pubkey = pending_invite.inviter_pubkey;
    let session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        if !session.is_valid() { return; }
        // `l:vector` tag is part of the published event shape; external
        // indexers / relay filters may key on it.
        let event_builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite_accepted")
            .tag(Tag::custom(TagKind::Custom("l".into()), vec!["vector"]))
            .tag(Tag::custom(TagKind::Custom("d".into()), vec![invite_code.as_str()]))
            .tag(Tag::public_key(inviter_pubkey));
        match client.sign_event_builder(event_builder).await {
            Ok(event) => {
                if !session.is_valid() { return; }
                match client.send_event_to(active_trusted_relays().await.into_iter(), &event).await {
                    Ok(_) => println!("Successfully broadcast invite acceptance to trusted relays"),
                    Err(e) => eprintln!("Failed to broadcast invite acceptance: {}", e),
                }
            }
            Err(e) => eprintln!("Failed to sign invite acceptance event: {}", e),
        }
    });
}

/// Shared MLS keypackage bootstrap (non-blocking, used by login_from_stored_key and setup_encryption)
fn spawn_mls_bootstrap() {
    // Capture the current session before spawn — a swap before the 250ms
    // sleep elapses should abandon the bootstrap rather than write a new
    // keypackage into the WRONG account's MDK storage. Consistent with
    // the inline keypackage spawns in encrypt() / decrypt() / setup_encryption.
    let session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        if !session.is_valid() { return; }

        if crate::account_manager::get_current_account().is_err() {
            println!("[MLS] Skipping KeyPackage bootstrap - no account selected");
            return;
        }

        let force_pending = db::get_sql_setting("mls_force_keypackage_regen".into())
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
