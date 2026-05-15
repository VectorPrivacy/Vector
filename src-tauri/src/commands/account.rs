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

/// Public key returned from login/create_account (private key stays backend-only).
///
/// `existing = true` signals the resulting npub matches an account already on
/// disk; the backend has written the active-account marker and emitted
/// `session_reload` so the frontend should skip the encryption-setup flow and
/// let the boot path load the stored credentials.
#[derive(serde::Serialize, Clone)]
pub struct LoginResult {
    pub public: String,
    #[serde(default)]
    pub existing: bool,
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
pub async fn login<R: Runtime>(
    handle: AppHandle<R>,
    mut import_key: String,
) -> Result<LoginResult, String> {
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
            return Ok(LoginResult { public: prev_npub, existing: false });
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

    // Existing-account collision: typing in / pasting an nsec for an account
    // already on disk lands here in add-account mode. Switch into the existing
    // account instead of stomping its per-account dir with a fresh
    // PENDING_NSEC install.
    let public_key = keys.public_key;
    let new_npub = public_key.to_bech32()
        .map_err(|e| format!("Bech32 error: {}", e))?;
    if let Ok(existing) = account_manager::list_accounts(&handle) {
        if existing.iter().any(|n| n == &new_npub) {
            drop(keys);
            let _ = vector_core::db::write_active_account_file(&new_npub);
            let _ = handle.emit("session_reload", ());
            return Ok(LoginResult { public: new_npub, existing: true });
        }
    }

    // Store nsec in PENDING_NSEC for setup_encryption/skip_encryption (never sent over IPC)
    {
        let mut pending = PENDING_NSEC.lock().unwrap();
        *pending = Some(keys.secret_key().to_bech32().unwrap());
    }

    // Store secret key in the guarded vault, then construct the client with GuardedSigner
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
    Ok(LoginResult { public: npub, existing: false })
}

/// Re-authorize an existing bunker account whose signer has lost / wiped
/// the pairing (e.g. user hit "Reset Permissions" in Amber). Generates a
/// fresh `nostrconnect://` URL using the EXISTING client keypair (loaded
/// from `MY_SECRET_KEY` during the failed login attempt) — Vector's bunker
/// account row, chat history, settings stay intact. The user scans / pastes
/// in their signer, signer re-creates the pairing, Vector resumes.
///
/// Returns the nostrconnect URL immediately; a background task awaits the
/// signer's response and emits `bunker_reauthorize_succeeded { npub }` on
/// success or `bunker_reauthorize_failed { error }` on failure.
///
/// Refuses to swap accounts: if the signer returns a different remote pubkey
/// than the one Vector has on record, the re-pair is aborted. The user must
/// logout and re-add the new account instead.
///
/// Crash-safety invariant: `bunker_url` on disk is the source of truth for
/// the active session's identity. `MY_PUBLIC_KEY` is rebuilt at boot by
/// `login_from_stored_key` running `attempt_bunker_login` against the stored
/// URL, so a kill between `set_bunker_url` (line 348) and `set_my_public_key`
/// (line 366) self-heals on the next launch.
#[tauri::command]
pub async fn reauthorize_bunker<R: Runtime>(handle: AppHandle<R>) -> Result<String, String> {
    // Snapshot the session generation. A concurrent `swap_session` / `logout`
    // can fire between reading MY_SECRET_KEY (below) and installing the new
    // bunker signer; without a guard the stale client keypair would land
    // under a fresh, foreign account.
    let session = vector_core::state::SessionGuard::capture();

    let client_keys = crate::MY_SECRET_KEY.to_keys()
        .ok_or("No client keypair loaded — please return to the login screen and try again")?;

    let signer_type = vector_core::db::get_signer_type().unwrap_or_else(|_| "local".to_string());
    if signer_type != "bunker" {
        return Err("This account is not a remote-signer account.".into());
    }

    let expected_remote_pk_hex = vector_core::db::get_bunker_remote_pubkey()
        .ok().flatten()
        .ok_or("Bunker account missing cached remote pubkey")?
        .to_ascii_lowercase();

    if !session.is_valid() {
        return Err("Account changed during re-authorization. Please try again.".into());
    }

    let relays: Vec<RelayUrl> = vector_core::state::TRUSTED_RELAYS.iter()
        .filter_map(|s| RelayUrl::parse(*s).ok())
        .collect();
    if relays.is_empty() {
        return Err("No trusted relays configured".into());
    }

    let (nc, uri_string) = vector_core::build_nostrconnect_session(
        client_keys,
        relays,
        std::time::Duration::from_secs(120),
    )?;

    if !session.is_valid() {
        return Err("Account changed during re-authorization. Please try again.".into());
    }

    // The new NostrConnect is held LOCALLY in the spawn until the signer
    // confirms identity. The live BUNKER_SIGNER / NOSTR_CLIENT remain
    // installed and operational throughout — backing out of the form costs
    // nothing, and a failed re-pair leaves the current session untouched.
    // We swap in only after the identity check passes.

    let handle_for_task = handle.clone();
    // Reuse the entry-captured guard so the spawn shares the same generation
    // snapshot — capturing fresh here would mask a swap occurring between the
    // last entry-check and the spawn.
    tokio::spawn(async move {
        vector_core::log_debug!("[bunker-reauth] awaiting signer pair…");

        let bunker_uri = match nc.bunker_uri().await {
            Ok(uri) => uri,
            Err(e) => {
                vector_core::log_warn!("[bunker-reauth] bunker_uri failed: {}", e);
                let _ = handle_for_task.emit("bunker_reauthorize_failed",
                    serde_json::json!({ "error": e.to_string() }));
                let _ = nc.shutdown().await;
                return;
            }
        };
        let storage_url = bunker_uri.to_string();

        let _ = handle_for_task.emit("bunker_awaiting_approval", serde_json::json!({}));
        let remote_pk = match nc.get_public_key().await {
            Ok(pk) => pk,
            Err(e) => {
                vector_core::log_warn!("[bunker-reauth] get_public_key failed: {}", e);
                let _ = handle_for_task.emit("bunker_reauthorize_failed",
                    serde_json::json!({ "error": format!(
                        "Signer didn't return your pubkey. Check the signer app for an approval prompt. ({})",
                        e
                    )}));
                let _ = nc.shutdown().await;
                return;
            }
        };
        let remote_pk_hex = remote_pk.to_hex().to_ascii_lowercase();

        // Identity-swap guard. If the signer returns a different identity
        // than we have on file, the user is trying to re-authorize the
        // wrong account — block and surface a clear error instead of
        // silently corrupting the on-disk account.
        if remote_pk_hex != expected_remote_pk_hex {
            vector_core::log_warn!(
                "[bunker-reauth] identity mismatch: expected {} got {}",
                &expected_remote_pk_hex[..16.min(expected_remote_pk_hex.len())],
                &remote_pk_hex[..16.min(remote_pk_hex.len())]
            );
            let _ = handle_for_task.emit("bunker_reauthorize_failed",
                serde_json::json!({ "error":
                    "Signer returned a different identity. Re-authorize is only for the original account — logout to switch accounts."
                }));
            let _ = nc.shutdown().await;
            return;
        }

        // Account-swap defense: re-check before persisting the new URL so
        // a swap during the long await window doesn't write into the wrong
        // account's DB.
        if !session.is_valid() {
            vector_core::log_warn!("[bunker-reauth] session changed during pairing — aborting");
            let _ = nc.shutdown().await;
            return;
        }

        if let Err(e) = vector_core::db::set_bunker_url(&storage_url).await {
            vector_core::log_warn!("[bunker-reauth] set_bunker_url failed: {}", e);
            let _ = handle_for_task.emit("bunker_reauthorize_failed",
                serde_json::json!({ "error": format!("Failed to persist bunker URL: {}", e) }));
            let _ = nc.shutdown().await;
            return;
        }

        if !session.is_valid() {
            let _ = nc.shutdown().await;
            return;
        }

        let remote_npub = match remote_pk.to_bech32() {
            Ok(n) => n,
            Err(e) => {
                let _ = handle_for_task.emit("bunker_reauthorize_failed",
                    serde_json::json!({ "error": format!("npub encode: {}", e) }));
                let _ = nc.shutdown().await;
                return;
            }
        };

        // Confirmed: new pairing is healthy and matches the existing identity.
        // Two install paths:
        //   - In-app reauth (Settings, etc.): NOSTR_CLIENT is alive with
        //     relays + subscriptions. `set_signer` hot-swaps the signer in
        //     place — pool + subs stay intact, no reconnect storm.
        //   - Boot-time reauth (login_from_stored_key returned Err before
        //     installing the Client): the session bootstrap (relays, sync,
        //     listeners, profile load) never ran. A fresh Client without
        //     bootstrap leaves the UI blank, so we fire `session_reload`
        //     and let the boot path replay cleanly with the signer now
        //     online. The encryption key stays in-process across the
        //     webview reload so the user re-enters their PIN only once.
        let was_boot_reauth = vector_core::nostr_client().is_none();

        let old_signer = vector_core::take_bunker_signer();
        vector_core::set_bunker_signer(nc);

        let new_inner = match vector_core::bunker_signer() {
            Some(b) => b,
            None => {
                vector_core::log_warn!("[bunker-reauth] new signer slot drained mid-swap");
                return;
            }
        };
        let watched = vector_core::WatchedBunkerSigner::new(new_inner);

        if was_boot_reauth {
            // Don't install the Client — session_reload will rebuild it
            // through the normal boot path. Installing here would leave a
            // half-initialised Client that the boot path would refuse to
            // replace (the concurrent-install guard in login_from_stored_key
            // skips when NOSTR_CLIENT is already Some).
        } else if let Some(client) = vector_core::nostr_client() {
            client.set_signer(watched).await;
        }

        // Drain the old NostrConnect in the background — its relay pool can
        // take a moment to release sockets, and the success event shouldn't
        // wait on that.
        if let Some(old) = old_signer {
            tokio::spawn(async move { let _ = old.shutdown().await; });
        }

        if !was_boot_reauth {
            set_my_public_key(remote_pk);
            vector_core::set_signer_kind(vector_core::SignerKind::Bunker);
        }

        if !session.is_valid() { return; }

        let _ = account_manager::set_current_account(remote_npub.clone());

        vector_core::set_bunker_state(vector_core::BunkerConnectionState::Online);
        // Stash the npub for `get_pending_reauth_result` so a frontend that
        // reloaded between the success and its own listener registration
        // can still recover on its next bunker form mount.
        *PENDING_REAUTH_RESULT.lock().unwrap() = Some(remote_npub.clone());

        if was_boot_reauth {
            // Drain bunker state so the reloaded boot path starts clean —
            // the NostrConnect we installed gets replaced by attempt_bunker_login
            // during the next login_from_stored_key.
            if let Some(b) = vector_core::take_bunker_signer() {
                tokio::spawn(async move { let _ = b.shutdown().await; });
            }
            vector_core::set_bunker_state(vector_core::BunkerConnectionState::Idle);
            let _ = handle_for_task.emit("session_reload", ());
        } else {
            let _ = handle_for_task.emit("bunker_reauthorize_succeeded",
                serde_json::json!({ "npub": remote_npub }));
        }
        vector_core::log_debug!("[bunker-reauth] succeeded (boot={})", was_boot_reauth);
    });

    Ok(uri_string)
}

/// Records the npub of the most recent successful `bunker_reauthorize_succeeded`
/// emission. Frontend polls via `get_pending_reauth_result` to recover when a
/// page reload (or webview hot-reload) races the success event and the
/// in-memory listener misses it. Cleared on read.
static PENDING_REAUTH_RESULT: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

#[tauri::command]
pub fn get_pending_reauth_result() -> Option<String> {
    PENDING_REAUTH_RESULT.lock().unwrap().take()
}

/// Read-only view of the active account's bunker pairing. Used by the
/// Security settings panel to render "Connected via Remote Signer" plus
/// the relay list. Returns `Ok(None)` for local accounts so the panel can
/// hide the row entirely without branching on a Result error path.
#[derive(serde::Serialize, Clone)]
pub struct BunkerStatusInfo {
    pub remote_pubkey_hex: String,
    pub remote_npub: String,
}

#[tauri::command]
pub async fn get_bunker_status() -> Result<Option<BunkerStatusInfo>, String> {
    let signer_type = vector_core::db::get_signer_type()
        .unwrap_or_else(|_| "local".to_string());
    if signer_type != "bunker" {
        return Ok(None);
    }

    let remote_pubkey_hex = vector_core::db::get_bunker_remote_pubkey()
        .ok().flatten()
        .ok_or("Bunker account missing cached remote pubkey")?;
    let remote_pk = PublicKey::from_hex(&remote_pubkey_hex)
        .map_err(|e| format!("Invalid bunker remote pubkey on disk: {}", e))?;
    let remote_npub = remote_pk.to_bech32()
        .map_err(|e| format!("Bech32 error: {}", e))?;

    Ok(Some(BunkerStatusInfo {
        remote_pubkey_hex,
        remote_npub,
    }))
}

/// Frontend-callable: drain any half-staged bunker session. Called by the
/// Back button on the bunker screen so a user who bails out doesn't leak
/// an installed NOSTR_CLIENT into the next login attempt. No-op when no
/// staged session exists, so safe to fire unconditionally.
#[tauri::command]
pub async fn cancel_bunker_session() -> Result<(), String> {
    // Only drain when there's no committed account — if the user is fully
    // logged in (e.g. they hit Back from an active-account context), we
    // mustn't tear down their session.
    if account_manager::get_current_account().is_ok() {
        return Ok(());
    }
    clear_pending_bunker_session().await;
    Ok(())
}

/// Drain a half-staged bunker session — the in-memory state installed by
/// `connect_bunker` / `start_nostrconnect_session` before the encryption
/// flow commits the account. Called from both the orphan-cleanup at the
/// top of those commands AND from the back-button path, so a user who
/// bails out of the bunker screen doesn't leak a NOSTR_CLIENT into the
/// next attempt.
async fn clear_pending_bunker_session() {
    use zeroize::Zeroize;
    // Defensive re-check: a concurrent `setup_encryption` / `skip_encryption`
    // could have committed the account between the public guard and here.
    // Refusing to drain a fully-committed session means a TOCTOU race
    // window can't accidentally tear down an active login.
    if account_manager::get_current_account().is_ok() {
        return;
    }
    if let Some(b) = vector_core::drain_bunker_state() {
        let _ = b.shutdown().await;
    }
    MY_SECRET_KEY.clear(&[&crate::ENCRYPTION_KEY]);
    crate::ENCRYPTION_KEY.clear(&[&MY_SECRET_KEY]);
    vector_core::clear_my_public_key();
    vector_core::clear_pending_bunker_setup();
    {
        let mut g = PENDING_NSEC.lock().unwrap();
        if let Some(ref mut s) = *g { s.zeroize(); }
        *g = None;
    }
    {
        let mut g = MNEMONIC_SEED.lock().unwrap();
        if let Some(ref mut s) = *g { s.zeroize(); }
        *g = None;
    }
    let _ = account_manager::clear_pending_account();
    if let Some(client) = vector_core::take_nostr_client() {
        let _ = client.shutdown().await;
    }
    crate::state::set_encryption_enabled(false);
}

/// Connect a NIP-46 remote bunker as the active account (paste flow).
///
/// Stages the bunker session and hands off to the encryption-choice flow
/// (PIN / Password / Skip) the same way local-account login does. The
/// settings commit happens in `setup_encryption` / `skip_encryption` once
/// the user picks a security mode — keeping bunker users on the same
/// post-login UX as everyone else.
///
/// Steps:
///   1. Validate the URL.
///   2. Generate a fresh NIP-46 *client* keypair (used only to RPC the
///      bunker — never the user's identity).
///   3. Bootstrap via `attempt_bunker_login`, which connects to the bunker's
///      relays and discovers the remote signer's pubkey (the user's actual
///      identity). This is the slow step — counts as the user tapping
///      "approve" in their signer app.
///   4. Stage the client nsec + bunker_url + remote_pubkey in process
///      memory (`PENDING_NSEC`, `PENDING_BUNKER_SETUP`) for the encryption
///      flow to consume.
///   5. Initialise the per-account DB keyed by the remote pubkey and install
///      session state (Client with bunker signer, MY_SECRET_KEY, MY_PUBLIC_KEY,
///      STATE profile). Active-account marker is NOT written yet — the
///      encryption-flow commit does that.
#[tauri::command]
pub async fn connect_bunker<R: Runtime>(
    handle: AppHandle<R>,
    bunker_url: String,
) -> Result<LoginResult, String> {
    use zeroize::Zeroizing;

    // Refuse if any account is mid-encryption-migration. Without this guard,
    // a bunker setup would tear down the DB pool out from under the open
    // migration transaction.
    account_manager::refuse_if_migration_in_progress("connect bunker")?;

    // Orphan-cleanup: same fix as start_nostrconnect_session — a previous
    // attempt may have staged a session that never committed. Always safe
    // to drain because the staged state isn't on disk.
    if nostr_client().is_some() && account_manager::get_current_account().is_err() {
        clear_pending_bunker_session().await;
    }

    // Already-logged-in idempotency: if the same bunker URL is being
    // re-submitted (e.g. dev hot-reload), no-op back with the existing npub.
    // A *different* bunker URL on an active session is a programming error;
    // the user should `logout` first.
    if let Some(_client) = nostr_client() {
        let existing_npub = crate::my_public_key()
            .ok_or("Public key not initialized")?
            .to_bech32()
            .map_err(|e| format!("Bech32 error: {}", e))?;
        // Compare the *remote signer pubkey* (stored in settings) to what the
        // caller passed. If we can't read it (e.g. account locked), fall back
        // to refusing the re-connect rather than risking a swap.
        let stored = vector_core::db::get_bunker_remote_pubkey().ok().flatten();
        if let Some(prev_remote_hex) = stored {
            if let Ok(new_remote_hex) = vector_core::parse_bunker_remote_pubkey(&bunker_url) {
                // Both sides are forced lowercase to defend against any
                // call-site that ever stores a mixed-case hex form.
                if new_remote_hex.to_ascii_lowercase() == prev_remote_hex.to_ascii_lowercase() {
                    return Ok(LoginResult { public: existing_npub, existing: false });
                }
            }
        }
        return Err("Already logged in. Logout first to switch bunkers.".into());
    }

    // Generate the NIP-46 client keypair. This is *not* the user's identity
    // — it's a transport keypair that signs RPC requests to the bunker. The
    // bunker authenticates the device by this pubkey.
    //
    // `client_nsec` is held in a Zeroizing<String> so the plaintext bech32
    // representation gets scrubbed on Drop — it lives across multi-second
    // network awaits, and without zeroize protection the residue would
    // outlive the function on the heap.
    let client_keys = Keys::generate();
    let client_nsec = Zeroizing::new(
        client_keys.secret_key().to_bech32()
            .map_err(|e| format!("Failed to bech32 client nsec: {}", e))?
    );
    // Wrapped so the NIP-46 client secret scrubs on Drop along every early-
    // return path (timeout, parse failure, rollback, same-npub collision).
    let client_secret_bytes: Zeroizing<[u8; 32]> = Zeroizing::new(
        client_keys.secret_key().to_secret_bytes()
    );

    // Bootstrap. Blocks until the bunker either confirms our connection +
    // returns the remote pubkey, or the configured timeout expires. The
    // 60s timeout matches NIP-46 examples; bunkers backed by Amber on
    // mobile sometimes take 5–10s for the user to tap "approve."
    let remote_pk = vector_core::attempt_bunker_login(
        &bunker_url,
        client_keys.clone(),
        std::time::Duration::from_secs(60),
    ).await?;

    let remote_npub = remote_pk.to_bech32()
        .map_err(|e| format!("Failed to bech32 remote pubkey: {}", e))?;
    let remote_pk_hex = remote_pk.to_hex();

    // Existing-account collision: same bunker identity already on disk means
    // the user is trying to add an account they already have. Tear down the
    // just-paired client keypair (it would otherwise overwrite the existing
    // account's pkey row at commit time) and swap into the existing account
    // via session_reload. The stored client keypair stays intact; if the
    // signer has revoked permissions for it, the existing account's
    // Re-authorize flow recovers.
    if let Ok(accounts) = account_manager::list_accounts(&handle) {
        if accounts.iter().any(|n| n == &remote_npub) {
            if let Some(b) = vector_core::drain_bunker_state() {
                let _ = b.shutdown().await;
            }
            let _ = vector_core::db::write_active_account_file(&remote_npub);
            let _ = handle.emit("session_reload", ());
            return Ok(LoginResult { public: remote_npub, existing: true });
        }
    }

    // Stage the bunker session. Active-account marker is NOT written here —
    // `setup_encryption` / `skip_encryption` is responsible for the atomic
    // settings commit + marker swap. All side-effects below are reversed by
    // the rollback envelope on any failure, so a half-built session never
    // survives past this command's return.
    let setup_result: Result<(), String> = async {
        account_manager::set_pending_account(remote_npub.clone())?;
        crate::commands::tor::stop_and_join_if_running().await;
        account_manager::init_profile_database(&handle, &remote_npub).await?;

        // Stage credentials for the encryption flow.
        *PENDING_NSEC.lock().unwrap() = Some(String::clone(&client_nsec));
        vector_core::set_pending_bunker_setup(bunker_url.clone(), remote_pk_hex.clone());

        // Install live session state (no DB commit yet).
        MY_SECRET_KEY.set(*client_secret_bytes, &[&crate::ENCRYPTION_KEY]);
        set_my_public_key(remote_pk);
        vector_core::set_signer_kind(vector_core::SignerKind::Bunker);

        let bunker = vector_core::bunker_signer()
            .ok_or_else(|| "Internal error: bunker signer slot empty after prewarm".to_string())?;
        let client = Client::builder()
            .signer(vector_core::WatchedBunkerSigner::new(bunker))
            .opts(vector_core::nostr_client_options())
            .monitor(Monitor::new(1024))
            .build();
        {
            let mut slot = NOSTR_CLIENT.write().unwrap();
            if slot.is_some() {
                vector_core::log_warn!("[Bunker Login] NOSTR_CLIENT was set concurrently; reusing existing instance.");
            } else {
                *slot = Some(client);
            }
        }

        let mut profile = Profile::new();
        profile.flags.set_mine(true);
        STATE.lock().await.insert_or_replace_profile(&remote_npub, profile);

        if let Err(e) = crate::commands::tor::sync_to_active_account().await {
            vector_core::log_warn!("[Bunker Login] Tor start for new account failed: {}", e);
        }

        vector_core::blossom_servers::refresh_cache();
        Ok(())
    }.await;

    if let Err(e) = setup_result {
        // Rollback: drain live signer, clear vaults, clear pending state,
        // tear down the Client we may have installed. Any subset may
        // already be in the unset state — clears are no-ops on empty.
        if let Some(b) = vector_core::drain_bunker_state() {
            let _ = b.shutdown().await;
        }
        MY_SECRET_KEY.clear(&[&crate::ENCRYPTION_KEY]);
        vector_core::clear_my_public_key();
        vector_core::clear_pending_bunker_setup();
        { use zeroize::Zeroize; let mut g = PENDING_NSEC.lock().unwrap();
          if let Some(ref mut s) = *g { s.zeroize(); } *g = None; }
        let _ = account_manager::clear_pending_account();
        if let Err(tor_err) = crate::commands::tor::sync_to_active_account().await {
            vector_core::log_warn!("[Bunker Login] Tor restore after rollback failed: {}", tor_err);
        }
        if let Some(client) = vector_core::take_nostr_client() {
            let _ = client.shutdown().await;
        }
        return Err(format!("Bunker setup failed: {}", e));
    }

    // MLS keypackage bootstrap is deferred to the encryption-flow commit
    // (setup_encryption / skip_encryption). regenerate_device_keypackage
    // writes to the active-account DB, but we haven't set the active marker
    // yet — the commit step does that and will spawn the keypackage publish
    // after marker write.

    Ok(LoginResult { public: remote_npub, existing: false })
}

/// Start a client-initiated NIP-46 session.
///
/// This is the QR / "Paste from Clipboard" flow in Amber. Returns a
/// `nostrconnect://<client_pubkey>?relay=...&metadata=...` URI string
/// immediately; the frontend renders it as a QR + a copy-to-clipboard
/// button, and the user takes that URL to their signer app to approve.
///
/// The bunker bootstrap runs in a background task — when the signer
/// connects back, we stage the session the same way `connect_bunker`'s
/// synchronous bootstrap does and emit a `bunker_session_staged` event
/// with the resolved npub. Frontend listens for that event and routes
/// the user into the encryption-choice flow.
///
/// On failure, emits `bunker_session_failed` with the error string.
#[tauri::command]
pub async fn start_nostrconnect_session<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<String, String> {
    use zeroize::Zeroizing;

    account_manager::refuse_if_migration_in_progress("connect bunker")?;

    // Orphan-cleanup: a previous attempt may have staged a bunker session
    // (NOSTR_CLIENT installed, MY_SECRET_KEY set, PENDING_BUNKER_SETUP
    // populated) but never committed it via the encryption flow — for
    // example if the frontend reloaded mid-setup. CURRENT_ACCOUNT empty
    // while NOSTR_CLIENT is set is the diagnostic, and it's always safe to
    // drain because nothing on disk references the staged state.
    if nostr_client().is_some() && account_manager::get_current_account().is_err() {
        clear_pending_bunker_session().await;
    }

    if nostr_client().is_some() {
        return Err("Already logged in. Logout first to switch accounts.".into());
    }

    // Build the client-initiated URI from our trusted relays. Multi-relay
    // by design — single-relay would mean any one relay outage locks the
    // user out of reconnecting to their own account.
    let relays: Vec<RelayUrl> = vector_core::state::TRUSTED_RELAYS.iter()
        .filter_map(|s| RelayUrl::parse(*s).ok())
        .collect();
    if relays.is_empty() {
        return Err("No trusted relays configured".into());
    }

    let client_keys = Keys::generate();
    // Wrapped so the NIP-46 client secret scrubs on Drop along every bg-task
    // early-return path (bunker_uri fail, get_public_key fail, collision,
    // staging Err, npub-encode Err).
    let client_secret_bytes: Zeroizing<[u8; 32]> = Zeroizing::new(
        client_keys.secret_key().to_secret_bytes()
    );
    let client_nsec = Zeroizing::new(
        client_keys.secret_key().to_bech32()
            .map_err(|e| format!("Failed to bech32 client nsec: {}", e))?
    );

    // Build NostrConnect against the Client URI. Long timeout (2 min) so
    // a user who walks away mid-pair doesn't get an instant failure — they
    // can come back and approve in Amber for a while before we give up.
    let (nc, uri_string) = vector_core::build_nostrconnect_session(
        client_keys.clone(),
        relays,
        std::time::Duration::from_secs(120),
    )?;

    // Install signer + mark Connecting. The background task will flip this
    // to Online (success) or Offline (failure) once the signer responds.
    vector_core::set_bunker_signer(nc);
    vector_core::set_bunker_state(vector_core::BunkerConnectionState::Connecting);

    // Background bootstrap. Frontend will see the URI return immediately,
    // render QR + copy button, and wait for `bunker_session_staged`.
    let handle_for_task = handle.clone();
    let uri_for_log = uri_string.clone();
    let session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        vector_core::log_debug!("[bunker] start_nostrconnect_session: background task spawned");
        vector_core::log_debug!("[bunker] nostrconnect URI: {}", uri_for_log);
        let signer = match vector_core::bunker_signer() {
            Some(s) => s,
            None => {
                vector_core::log_warn!("[bunker] signer slot empty before bunker_uri() — drained by reset?");
                return;
            }
        };

        vector_core::log_debug!("[bunker] awaiting signer.bunker_uri() (resolves once Amber Ack'd the connect)…");
        // First, await the bunker URI. This only blocks on the connect Ack
        // and gives us the canonical bunker:// URL for storage. The
        // `remote_signer_public_key` field here is the SIGNER's device
        // pubkey (the Nostr keypair Amber uses to RPC us) — for single-
        // pairing signers like Amber this is NOT the user's identity, so
        // we can't shortcut and use it for MY_PUBLIC_KEY.
        let bunker_uri = match signer.bunker_uri().await {
            Ok(uri) => {
                vector_core::log_debug!("[bunker] bunker_uri() resolved");
                uri
            }
            Err(e) => {
                vector_core::log_warn!("[bunker] bunker_uri() failed: {}", e);
                vector_core::set_bunker_state(vector_core::BunkerConnectionState::Offline);
                let _ = handle_for_task.emit("bunker_session_failed",
                    serde_json::json!({ "error": e.to_string() }));
                if let Some(b) = vector_core::take_bunker_signer() {
                    let _ = b.shutdown().await;
                }
                return;
            }
        };
        let storage_url = bunker_uri.to_string();

        // NOW request the user's actual identity pubkey via a NIP-46
        // `GetPublicKey` RPC. In Amber's "Manually approve each" mode this
        // pops an approval prompt (in-app or via Android notification); the
        // user has to approve once before we proceed. Tell the frontend so
        // it can update the status text from "Waiting for signer…" to
        // "Check your signer app to approve" — silent hangs here gave the
        // appearance of "stuck on Waiting".
        let _ = handle_for_task.emit("bunker_awaiting_approval",
            serde_json::json!({}));
        vector_core::log_debug!("[bunker] awaiting signer.get_public_key() (Amber may prompt in Manual mode)…");
        let remote_pk = match signer.get_public_key().await {
            Ok(pk) => {
                vector_core::log_debug!("[bunker] get_public_key() resolved → user pubkey discovered");
                pk
            }
            Err(e) => {
                vector_core::log_warn!("[bunker] get_public_key() failed: {}", e);
                vector_core::set_bunker_state(vector_core::BunkerConnectionState::Offline);
                let _ = handle_for_task.emit("bunker_session_failed",
                    serde_json::json!({ "error": format!(
                        "Signer didn't return your pubkey. If you're in Manual mode, check your signer app for an approval prompt. ({})",
                        e
                    )}));
                if let Some(b) = vector_core::take_bunker_signer() {
                    let _ = b.shutdown().await;
                }
                return;
            }
        };

        // Same staging block as connect_bunker — extract any further if
        // a third entry point ever appears. Wraps the side-effects so a
        // failure here can roll back cleanly.
        let remote_npub = match remote_pk.to_bech32() {
            Ok(n) => n,
            Err(e) => {
                let _ = handle_for_task.emit("bunker_session_failed",
                    serde_json::json!({ "error": format!("npub encode: {}", e) }));
                return;
            }
        };
        let remote_pk_hex = remote_pk.to_hex();

        // Existing-account collision (see connect_bunker). The session_reload
        // path performs the swap; the just-paired client keypair is discarded
        // so the existing account's stored pkey row stays intact.
        if let Ok(accounts) = account_manager::list_accounts(&handle_for_task) {
            if accounts.iter().any(|n| n == &remote_npub) {
                if let Some(b) = vector_core::drain_bunker_state() {
                    let _ = b.shutdown().await;
                }
                let _ = vector_core::db::write_active_account_file(&remote_npub);
                let _ = handle_for_task.emit("session_reload", ());
                return;
            }
        }

        let stage_result: Result<(), String> = async {
            // Bail before any side effect if the user swapped accounts
            // during the long pairing wait.
            if !session.is_valid() {
                return Err("Session changed during pairing".to_string());
            }
            account_manager::set_pending_account(remote_npub.clone())?;
            crate::commands::tor::stop_and_join_if_running().await;
            if !session.is_valid() {
                return Err("Session changed during pairing".to_string());
            }
            account_manager::init_profile_database(&handle_for_task, &remote_npub).await?;

            if !session.is_valid() {
                return Err("Session changed during pairing".to_string());
            }
            *PENDING_NSEC.lock().unwrap() = Some(String::clone(&client_nsec));
            vector_core::set_pending_bunker_setup(storage_url, remote_pk_hex);

            MY_SECRET_KEY.set(*client_secret_bytes, &[&crate::ENCRYPTION_KEY]);
            set_my_public_key(remote_pk);
            vector_core::set_signer_kind(vector_core::SignerKind::Bunker);

            let bunker = vector_core::bunker_signer()
                .ok_or_else(|| "Bunker signer slot drained mid-stage".to_string())?;
            let client = Client::builder()
                .signer(vector_core::WatchedBunkerSigner::new(bunker))
                .opts(vector_core::nostr_client_options())
                .monitor(Monitor::new(1024))
                .build();
            { let mut slot = NOSTR_CLIENT.write().unwrap();
              if slot.is_none() { *slot = Some(client); } }

            if !session.is_valid() {
                return Err("Session changed during pairing".to_string());
            }
            let mut profile = Profile::new();
            profile.flags.set_mine(true);
            STATE.lock().await.insert_or_replace_profile(&remote_npub, profile);
            let _ = crate::commands::tor::sync_to_active_account().await;
            vector_core::blossom_servers::refresh_cache();
            Ok(())
        }.await;

        if let Err(e) = stage_result {
            if let Some(b) = vector_core::drain_bunker_state() {
                let _ = b.shutdown().await;
            }
            MY_SECRET_KEY.clear(&[&crate::ENCRYPTION_KEY]);
            vector_core::clear_my_public_key();
            vector_core::clear_pending_bunker_setup();
            { use zeroize::Zeroize; let mut g = PENDING_NSEC.lock().unwrap();
              if let Some(ref mut s) = *g { s.zeroize(); } *g = None; }
            let _ = account_manager::clear_pending_account();
            if let Some(client) = vector_core::take_nostr_client() {
                let _ = client.shutdown().await;
            }
            let _ = handle_for_task.emit("bunker_session_failed",
                serde_json::json!({ "error": e }));
            return;
        }

        vector_core::set_bunker_state(vector_core::BunkerConnectionState::Online);
        let _ = handle_for_task.emit("bunker_session_staged",
            serde_json::json!({ "npub": remote_npub }));
    });

    Ok(uri_string)
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
    Ok(LoginResult { public: npub, existing: false })
}

/// Export account keys (nsec and seed phrase if available).
///
/// Refuses bunker accounts: `pkey` for those holds the NIP-46 *client*
/// keypair, not the user's identity nsec. The identity key lives on the
/// remote signer and is intentionally inaccessible to Vector. Exporting
/// the client key under an "nsec" label would mislead the user into
/// importing it elsewhere as their identity and losing access.
#[tauri::command]
pub async fn export_keys() -> Result<serde_json::Value, String> {
    if vector_core::is_bunker() {
        return Err("This is a Remote Signer account. The identity key lives on your signer; Vector only holds the device pairing key, which is not your account.".into());
    }
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

    // Decrypt if password provided. For both local and bunker accounts the
    // `pkey` slot holds the same shape: a bech32 nsec, optionally encrypted
    // at rest. For local accounts that nsec is the user's identity; for
    // bunker accounts it's the NIP-46 client keypair. We derive + install
    // ENCRYPTION_KEY here so the bunker_url decryption below (a separate
    // settings read) doesn't have to redo Argon2id.
    let mut nsec = if let Some(pwd) = password {
        let key_bytes = crypto::hash_pass(pwd.clone()).await;
        crate::ENCRYPTION_KEY.set(key_bytes, &[&MY_SECRET_KEY]);
        crypto::internal_decrypt(stored_pkey, Some(pwd)).await
            .map_err(|_| "Incorrect password".to_string())?
    } else {
        stored_pkey
    };

    // Initialize Client with the key, then zeroize the plaintext nsec
    let keys = Keys::parse(&nsec).map_err(|_| "Invalid stored key".to_string())?;
    nsec.zeroize();

    let client_public_key = keys.public_key;
    MY_SECRET_KEY.store_from_keys(&keys, &[&crate::ENCRYPTION_KEY]);

    // Branch: bunker-signer accounts re-bootstrap the NIP-46 connection
    // BEFORE building the Nostr Client, so the Client gets installed with
    // the right signer (NostrConnect, not GuardedSigner). `keys` here is
    // the client keypair, not the user's identity — the user's identity
    // is the remote signer's pubkey, persisted as `bunker_remote_pubkey`.
    //
    // INVARIANT: by the time this function runs, the DB pool has already
    // been pointed at the active-account marker's npub — `boot_select_account`
    // (in `account_manager.rs`) calls `set_current_account` + `init_database`
    // before this command executes. If that ever changes, `get_signer_type`
    // could read from the wrong account's DB and silently misroute the login.
    let signer_type = vector_core::db::get_signer_type().unwrap_or_else(|_| "local".to_string());
    let is_bunker_account = signer_type == "bunker";

    let public_key = if is_bunker_account {
        let bunker_url = vector_core::db::get_bunker_url().await
            .map_err(|e| format!("Failed to read bunker_url: {}", e))?
            .ok_or("Bunker account missing bunker_url")?;
        // Hard requirement: if the bunker is unreachable at boot, the user
        // can't sign anything anyway — falling through with the cached
        // pubkey but no live NostrConnect leaves `client.signer()` returning
        // a Client whose signer slot has been initialised against a None
        // NostrConnect, which errors on the first send with a cryptic
        // "Bunker signer not installed after prewarm". Better to surface
        // the offline state immediately so the user knows to wake their
        // signer and retry.
        // Boot timeout is short: we're re-connecting to an already-paired
        // signer that the user previously approved, so the round-trip
        // doesn't involve any human approval. 15s catches sluggish relay
        // routing without leaving the user staring at a "loading" screen.
        let remote_pk = vector_core::attempt_bunker_login(
            &bunker_url,
            keys.clone(),
            std::time::Duration::from_secs(15),
        ).await.map_err(|e| {
            format!("Remote signer unreachable — please ensure your signer app is online and retry. ({})", e)
        })?;
        // Identity-swap guard. If the signer returns a pubkey that differs
        // from the one Vector has on disk for this account, the user has
        // flipped identity on the signer side and reconnecting would silently
        // mix two accounts' data. Refuse rather than install the wrong key.
        // Mirrors the reauth flow's check; closes the boot-side hole.
        let expected_remote_pk_hex = vector_core::db::get_bunker_remote_pubkey()
            .ok().flatten()
            .ok_or("Bunker account missing cached remote pubkey")?
            .to_ascii_lowercase();
        if remote_pk.to_hex().to_ascii_lowercase() != expected_remote_pk_hex {
            if let Some(b) = vector_core::take_bunker_signer() {
                let _ = b.shutdown().await;
            }
            vector_core::set_bunker_state(vector_core::BunkerConnectionState::Idle);
            return Err(
                "Remote signer returned a different identity than this account. \
                 Either re-authorize from Settings, or logout and re-add the account."
                    .into()
            );
        }
        vector_core::set_signer_kind(vector_core::SignerKind::Bunker);
        remote_pk
    } else {
        client_public_key
    };
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

    // Signer dispatch: bunker accounts wire the live NostrConnect handle
    // (installed by attempt_bunker_login above) into the Client; local
    // accounts use GuardedSigner over MY_SECRET_KEY as before.
    let client = if is_bunker_account {
        let bunker = vector_core::bunker_signer()
            .ok_or("Bunker signer not installed after prewarm")?;
        Client::builder()
            .signer(vector_core::WatchedBunkerSigner::new(bunker))
            .opts(vector_core::nostr_client_options())
            .monitor(Monitor::new(1024))
            .build()
    } else {
        Client::builder()
            .signer(vector_core::GuardedSigner::new(public_key))
            .opts(vector_core::nostr_client_options())
            .monitor(Monitor::new(1024))
            .build()
    };
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
        // Seed BLOSSOM_SERVERS from local prefs; kind-10063 merge runs
        // in fetch_messages after Quick Sync.
        vector_core::blossom_servers::refresh_cache();
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

    // Snapshot session generation up-front. Argon2id takes hundreds of ms;
    // a concurrent `swap_session` in that window would land the commit in
    // the wrong account's DB. Re-validated before every write.
    let session = vector_core::state::SessionGuard::capture();

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

    // Re-check session immediately before commit. If a swap fired during the
    // Argon2id awaits above, the DB pool now points at a different account
    // and committing would corrupt it.
    if !session.is_valid() {
        return Err("Account changed during setup. Please try again.".into());
    }

    // Branch on signer kind. For bunker accounts (staged by `connect_bunker`
    // or `start_nostrconnect_session`), commit the bunker rows instead of
    // the local ones — `pkey` holds the client keypair, and `bunker_url` +
    // `bunker_remote_pubkey` get written under the same transaction. The
    // bunker URL is encrypted with the same explicit-password path as the
    // pkey so on-disk encryption coverage is uniform.
    if vector_core::signer_kind() == vector_core::SignerKind::Bunker {
        let (url, remote_pk_hex) = vector_core::pending_bunker_setup()
            .ok_or("Bunker setup state missing — re-run Connect Remote Signer")?;
        let encrypted_url =
            crypto::internal_encrypt(url, Some((*password).clone())).await;
        if !session.is_valid() {
            return Err("Account changed during setup. Please try again.".into());
        }
        vector_core::db::commit_bunker_account_setup(
            &encrypted,
            true,
            Some(&security_type),
            &encrypted_url,
            &remote_pk_hex,
        )?;
        vector_core::clear_pending_bunker_setup();
    } else {
        // pkey + encryption_enabled + security_type + seed in one tx. Err
        // rolls back, leaving the new DB with no setup rows so retry is clean.
        vector_core::db::settings::commit_account_setup(
            &encrypted,
            true,
            Some(&security_type),
            encrypted_seed.as_deref(),
        )?;
    }

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
    vector_core::blossom_servers::refresh_cache();

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

    // Snapshot session generation up-front. `maybe_encrypt` for the seed
    // is async; `init_profile_database` is async. Re-validated before commit
    // so a concurrent `swap_session` can't land the rows in a foreign DB.
    let session = vector_core::state::SessionGuard::capture();

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

    // Re-check session immediately before commit. If a swap fired during the
    // awaits above, the DB pool now points at a different account.
    if !session.is_valid() {
        return Err("Account changed during setup. Please try again.".into());
    }

    // Bunker branch — same shape as setup_encryption, just plaintext rows.
    if vector_core::signer_kind() == vector_core::SignerKind::Bunker {
        let (url, remote_pk_hex) = vector_core::pending_bunker_setup()
            .ok_or("Bunker setup state missing — re-run Connect Remote Signer")?;
        vector_core::db::commit_bunker_account_setup(
            &nsec,
            false,
            None,
            &url,
            &remote_pk_hex,
        )?;
        vector_core::clear_pending_bunker_setup();
    } else {
        vector_core::db::settings::commit_account_setup(
            &nsec,
            false,
            None,
            encrypted_seed.as_deref(),
        )?;
    }

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
    vector_core::blossom_servers::refresh_cache();

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
