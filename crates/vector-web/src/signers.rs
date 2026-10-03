//! External signers: NIP-46 bunkers over relays, and NIP-07 browser
//! extensions, which live in the page and are reached through it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::signer::BoxedFuture;
use vector_core::{db, state, SignerKind, MY_SECRET_KEY};
use zeroize::Zeroizing;

use crate::account;
use crate::commands::Args;
use crate::emitter;

/// Stored as `nip55_signer_package` for a NIP-07 account.
pub const NIP07_SIGNER_NAME: &str = "browser-extension";

// ============================================================================
// NIP-07: requests out to the page, answers back through `nip07_reply`
// ============================================================================

type Reply = tokio::sync::oneshot::Sender<Result<Value, String>>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static WAITING: Mutex<Option<HashMap<u64, Reply>>> = Mutex::new(None);

struct PageNip07;

impl vector_core::Nip07Backend for PageNip07 {
    fn request(&self, method: &'static str, params: Value) -> BoxedFuture<'static, Result<Value, String>> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        WAITING.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, tx);
        // The page times the extension out, so every request is answered.
        emitter::emit("nip07_request", &json!({ "id": id, "method": method, "params": params }));
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err("browser signer went away".into())) })
    }
}

pub fn install() {
    vector_core::set_nip07_backend(Box::new(PageNip07));
}

fn nip07_reply(a: &Args) -> Result<Value, String> {
    let id = a.get("id").and_then(Value::as_u64).ok_or("missing argument `id`")?;
    let waiter = WAITING.lock().unwrap().as_mut().and_then(|m| m.remove(&id));
    if let Some(tx) = waiter {
        let result = if a.bool("ok") == Some(true) {
            Ok(a.get("value").cloned().unwrap_or(Value::Null))
        } else {
            Err(a.opt_str("error").unwrap_or_else(|| "browser signer refused".into()))
        };
        let _ = tx.send(result);
    }
    Ok(Value::Null)
}

/// An imported identity already on this device reopens instead of re-adding.
async fn switch_if_known(npub: &str) -> bool {
    if crate::account::is_committed(npub) {
        let _ = db::write_active_account_file(npub);
        crate::storage::reload().await;
        return true;
    }
    false
}

async fn stage_keyless(pk: PublicKey, kind: SignerKind, signer_name: &str) -> Result<String, String> {
    let npub = pk.to_bech32().map_err(|e| e.to_string())?;
    vector_core::set_pending_nip55_setup(pk.to_hex(), signer_name.to_string());
    state::set_my_public_key(pk);
    vector_core::set_signer_kind(kind);
    account::install_client();
    account::insert_own_profile(&npub).await;
    account::stage_pending(npub.clone());
    Ok(npub)
}

async fn login_with_nip07() -> Result<Value, String> {
    account::disarm_fresh_account_stamp();
    let pk = vector_core::nip07_get_public_key().await?;
    let npub = pk.to_bech32().map_err(|e| e.to_string())?;
    if state::nostr_client().is_some() {
        if vector_core::my_public_key() == Some(pk) {
            return Ok(json!({ "public": npub, "existing": false }));
        }
        return Err("Already logged in. Logout first to add another account.".into());
    }
    if switch_if_known(&npub).await {
        return Ok(json!({ "public": npub, "existing": true }));
    }
    let npub = stage_keyless(pk, SignerKind::Nip07, NIP07_SIGNER_NAME).await?;
    Ok(json!({ "public": npub, "existing": false }))
}

async fn reauthorize_nip07() -> Result<Value, String> {
    if vector_core::signer_kind() != SignerKind::Nip07 {
        return Err("This is not a browser-signer account.".into());
    }
    let expected = db::get_nip55_user_pubkey()?.ok_or("Browser-signer account missing its identity")?;
    let pk = vector_core::nip07_get_public_key().await?;
    if !pk.to_hex().eq_ignore_ascii_case(&expected) {
        return Err("Your extension is signed in as a different identity. Switch it back, or logout to use that one.".into());
    }
    to_npub(pk)
}

fn get_nip07_status() -> Result<Value, String> {
    if db::get_signer_type().unwrap_or_default() != SignerKind::Nip07.as_setting_str() {
        return Ok(Value::Null);
    }
    let hex = db::get_nip55_user_pubkey()?.ok_or("Browser-signer account missing its identity")?;
    let pk = PublicKey::from_hex(&hex).map_err(|e| e.to_string())?;
    Ok(json!({ "user_pubkey_hex": hex, "user_npub": pk.to_bech32().map_err(|e| e.to_string())? }))
}

fn to_npub(pk: PublicKey) -> Result<Value, String> {
    Ok(json!(pk.to_bech32().map_err(|e| e.to_string())?))
}

// ============================================================================
// Keyless commit and unlock (NIP-07; NIP-55 shares the rows)
// ============================================================================

/// Commit a staged keyless account. The PIN protects only the local database;
/// with nothing sealed to reject a wrong one, a canary does.
pub async fn commit_keyless(password: Option<&str>, security_type: Option<&str>) -> Result<(), String> {
    let (pk_hex, signer_name) =
        vector_core::pending_nip55_setup().ok_or("Signer setup state missing. Please sign in again.")?;
    let kdf = password.map(|_| vector_core::crypto::Kdf::fresh());
    let canary = match (password, &kdf) {
        (Some(pwd), Some(kdf)) => {
            let mut key = vector_core::crypto::derive_key(pwd, kdf).await;
            let sealed = vector_core::crypto::encrypt_with_key(db::at_rest::NIP55_PIN_CANARY, &key);
            if sealed.is_ok() {
                vector_core::ENCRYPTION_KEY.set(key, &[&MY_SECRET_KEY]);
            }
            zeroize::Zeroize::zeroize(&mut key);
            Some(sealed?)
        }
        _ => None,
    };
    account::take_pending_into_current()?;
    db::commit_external_signer_setup(
        vector_core::signer_kind(),
        &pk_hex,
        &signer_name,
        password.is_some(),
        security_type,
        None,
        canary.as_deref(),
        kdf.as_ref().and_then(|k| k.descriptor()).as_deref(),
    )?;
    vector_core::clear_pending_nip55_setup();
    state::set_encryption_enabled(password.is_some());
    account::finish_commit();
    Ok(())
}

/// Move a legacy-salted account onto its own salt right after a verified unlock, then vacuum so
/// nothing under the legacy key survives in free pages. Only on persistent storage: a session or
/// memory store can't rewrite itself crash-safely, and keeps nothing worth protecting.
pub(crate) async fn upgrade_after_unlock(password: &str, key: &[u8; 32]) {
    if crate::storage::Storage::current() != crate::storage::Storage::Persistent {
        return;
    }
    match db::at_rest::upgrade_after_unlock(password, key).await {
        Ok(true) => {
            if let Err(e) = db::at_rest::vacuum_now() {
                vector_core::log_warn!("[Encryption] Post-upgrade vacuum deferred: {e}");
            }
        }
        Ok(false) => {}
        Err(e) => vector_core::log_warn!("[Encryption] Salt upgrade deferred: {e}"),
    }
}

/// Unlock a keyless account: verify the PIN against the canary, then adopt the
/// stored identity.
pub async fn unlock_keyless(kind: SignerKind, password: Option<String>) -> Result<PublicKey, String> {
    let mut unlocked = None;
    if let Some(pwd) = password {
        let kdf = vector_core::crypto::Kdf::of_account()?;
        let key = zeroize::Zeroizing::new(vector_core::crypto::derive_key(&pwd, &kdf).await);
        vector_core::crypto::install_unlocked_key(&key, &kdf)?;
        unlocked = Some((zeroize::Zeroizing::new(pwd), key));
    }
    if vector_core::ENCRYPTION_KEY.has_key() {
        if let Ok(Some(stored)) = db::get_sql_setting("nip55_pin_check".into()) {
            let ok = matches!(vector_core::crypto::maybe_decrypt(stored).await,
                Ok(plain) if plain == db::at_rest::NIP55_PIN_CANARY);
            if !ok {
                vector_core::ENCRYPTION_KEY.clear(&[&MY_SECRET_KEY]);
                return Err("Incorrect password".into());
            }
        }
    }
    // A legacy-salted account moves onto its own salt before anything writes; the upgrade
    // itself refuses a key nothing here can confirm.
    if let Some((pwd, key)) = unlocked {
        upgrade_after_unlock(&pwd, &key).await;
    }
    let hex = db::get_nip55_user_pubkey()?.ok_or("Signer account missing its identity")?;
    let pk = PublicKey::parse(&hex).map_err(|_| "Stored signer identity is invalid".to_string())?;
    vector_core::set_signer_kind(kind);
    Ok(pk)
}

// ============================================================================
// NIP-46 bunkers
// ============================================================================

static PENDING_REAUTH_RESULT: Mutex<Option<String>> = Mutex::new(None);

/// The current QR pairing attempt. A superseded or cancelled attempt's task
/// must not touch the signer slot, which belongs to its successor.
static PAIRING: AtomicU64 = AtomicU64::new(0);

fn trusted_relays() -> Result<Vec<RelayUrl>, String> {
    let relays: Vec<RelayUrl> = state::TRUSTED_RELAYS.iter().filter_map(|s| RelayUrl::parse(s).ok()).collect();
    if relays.is_empty() {
        return Err("No trusted relays configured".into());
    }
    Ok(relays)
}

/// Install a paired bunker as the session being set up. The client keypair
/// waits in `PENDING_NSEC` for the security step to seal it.
async fn stage_bunker(client_keys: &Keys, storage_url: String, remote_pk: PublicKey) -> Result<String, String> {
    let npub = remote_pk.to_bech32().map_err(|e| e.to_string())?;
    let client_nsec = Zeroizing::new(client_keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    *state::PENDING_NSEC.lock().unwrap() = Some(String::clone(&client_nsec));
    vector_core::set_pending_bunker_setup(storage_url, remote_pk.to_hex());
    MY_SECRET_KEY.store_from_keys(client_keys, &[&vector_core::ENCRYPTION_KEY]);
    state::set_my_public_key(remote_pk);
    vector_core::set_signer_kind(SignerKind::Bunker);
    account::install_client();
    account::insert_own_profile(&npub).await;
    account::stage_pending(npub.clone());
    Ok(npub)
}

async fn drop_bunker() {
    if let Some(b) = vector_core::drain_bunker_state() {
        let _ = b.shutdown().await;
    }
}

async fn connect_bunker(bunker_url: String) -> Result<Value, String> {
    account::disarm_fresh_account_stamp();
    if state::nostr_client().is_some() {
        let stored = db::get_bunker_remote_pubkey().ok().flatten();
        let asked = vector_core::parse_bunker_remote_pubkey(&bunker_url).ok();
        if let (Some(prev), Some(new), Some(me)) = (stored, asked, vector_core::my_public_key()) {
            if prev.eq_ignore_ascii_case(&new) {
                return Ok(json!({ "public": me.to_bech32().map_err(|e| e.to_string())?, "existing": false }));
            }
        }
        return Err("Already logged in. Logout first to switch bunkers.".into());
    }
    let client_keys = Keys::generate();
    let remote_pk = vector_core::attempt_bunker_login(&bunker_url, client_keys.clone(), web_time::Duration::from_secs(60)).await?;
    let npub = remote_pk.to_bech32().map_err(|e| e.to_string())?;
    if switch_if_known(&npub).await {
        drop_bunker().await;
        return Ok(json!({ "public": npub, "existing": true }));
    }
    let npub = stage_bunker(&client_keys, bunker_url, remote_pk).await?;
    Ok(json!({ "public": npub, "existing": false }))
}

/// The QR flow: returns the `nostrconnect://` link at once, then stages the
/// account when the signer answers (`bunker_session_staged`).
async fn start_nostrconnect_session() -> Result<Value, String> {
    account::disarm_fresh_account_stamp();
    if state::nostr_client().is_some() {
        return Err("Already logged in. Logout first to switch accounts.".into());
    }
    let client_keys = Keys::generate();
    let (nc, uri) = vector_core::build_nostrconnect_session(client_keys.clone(), trusted_relays()?, web_time::Duration::from_secs(120))?;
    drop_bunker().await;
    let attempt = PAIRING.fetch_add(1, Ordering::AcqRel) + 1;
    vector_core::set_bunker_signer(nc.clone());
    vector_core::set_bunker_state(vector_core::BunkerConnectionState::Connecting);

    db::spawn_bound(async move {
        let live = || PAIRING.load(Ordering::Acquire) == attempt;
        let fail = |e: String| emitter::emit("bunker_session_failed", &json!({ "error": e }));
        let storage_url = match nc.bunker_uri().await {
            Ok(u) => u.to_string(),
            Err(_) if !live() => return,
            Err(e) => {
                vector_core::set_bunker_state(vector_core::BunkerConnectionState::Offline);
                drop_bunker().await;
                return fail(e.to_string());
            }
        };
        emitter::emit("bunker_awaiting_approval", &json!({}));
        let remote_pk = match nc.get_public_key_async().await {
            Ok(pk) => pk,
            Err(_) if !live() => return,
            Err(e) => {
                vector_core::set_bunker_state(vector_core::BunkerConnectionState::Offline);
                drop_bunker().await;
                return fail(format!("Signer didn't return your pubkey. Check your signer app for an approval prompt. ({e})"));
            }
        };
        // Clones don't share the pairing, so the slot takes the paired instance,
        // unless the attempt was abandoned meanwhile.
        if !live() {
            let _ = nc.shutdown().await;
            return;
        }
        vector_core::set_bunker_signer(nc);
        let Ok(npub) = remote_pk.to_bech32();
        if switch_if_known(&npub).await {
            drop_bunker().await;
            return;
        }
        match stage_bunker(&client_keys, storage_url, remote_pk).await {
            Ok(npub) => {
                vector_core::set_bunker_state(vector_core::BunkerConnectionState::Online);
                emitter::emit("bunker_session_staged", &json!({ "npub": npub }));
            }
            Err(e) => {
                drop_bunker().await;
                fail(e);
            }
        }
    });
    Ok(json!(uri))
}

/// Re-pair a committed bunker account whose signer dropped its permissions.
/// The live signer keeps working until the new pairing proves the same identity.
async fn reauthorize_bunker() -> Result<Value, String> {
    let client_keys = MY_SECRET_KEY
        .to_keys()
        .ok_or("No client keypair loaded. Please return to the login screen and try again.")?;
    if vector_core::signer_kind() != SignerKind::Bunker && db::get_signer_type().unwrap_or_default() != "bunker" {
        return Err("This account is not a remote-signer account.".into());
    }
    let expected = db::get_bunker_remote_pubkey()
        .ok()
        .flatten()
        .ok_or("Bunker account missing cached remote pubkey")?
        .to_ascii_lowercase();
    let (nc, uri) = vector_core::build_nostrconnect_session(client_keys, trusted_relays()?, web_time::Duration::from_secs(120))?;
    let session = db::current_session();

    db::spawn_bound(async move {
        let fail = |e: String| emitter::emit("bunker_reauthorize_failed", &json!({ "error": e }));
        let storage_url = match nc.bunker_uri().await {
            Ok(u) => u.to_string(),
            Err(e) => {
                let _ = nc.shutdown().await;
                return fail(e.to_string());
            }
        };
        emitter::emit("bunker_awaiting_approval", &json!({}));
        let remote_pk = match nc.get_public_key_async().await {
            Ok(pk) => pk,
            Err(e) => {
                let _ = nc.shutdown().await;
                return fail(format!("Signer didn't return your pubkey. Check the signer app for an approval prompt. ({e})"));
            }
        };
        if remote_pk.to_hex().to_ascii_lowercase() != expected {
            let _ = nc.shutdown().await;
            return fail("Signer returned a different identity. Re-authorize is only for the original account; logout to switch accounts.".into());
        }
        if !session.is_live() {
            let _ = nc.shutdown().await;
            return;
        }
        if let Err(e) = db::set_bunker_url(&storage_url).await {
            let _ = nc.shutdown().await;
            return fail(format!("Failed to persist bunker URL: {e}"));
        }
        let Ok(npub) = remote_pk.to_bech32();
        let was_boot = state::nostr_client().is_none();
        if let Some(old) = vector_core::take_bunker_signer() {
            let _ = old.shutdown().await;
        }
        if was_boot {
            // Unlock never finished: boot again with the signer answering.
            let _ = nc.shutdown().await;
            vector_core::set_bunker_state(vector_core::BunkerConnectionState::Idle);
            crate::storage::reload().await;
            return;
        }
        vector_core::set_bunker_signer(nc);
        vector_core::set_signer_kind(SignerKind::Bunker);
        vector_core::set_bunker_state(vector_core::BunkerConnectionState::Online);
        *PENDING_REAUTH_RESULT.lock().unwrap() = Some(npub.clone());
        emitter::emit("bunker_reauthorize_succeeded", &json!({ "npub": npub }));
    });
    Ok(json!(uri))
}

fn get_bunker_status() -> Result<Value, String> {
    if db::get_signer_type().unwrap_or_default() != "bunker" {
        return Ok(Value::Null);
    }
    let hex = db::get_bunker_remote_pubkey()?.ok_or("Bunker account missing cached remote pubkey")?;
    let pk = PublicKey::from_hex(&hex).map_err(|e| e.to_string())?;
    Ok(json!({ "remote_pubkey_hex": hex, "remote_npub": pk.to_bech32().map_err(|e| e.to_string())? }))
}

/// Back out of a signer login that never committed. A committed account is left alone.
async fn cancel_session() -> Result<Value, String> {
    PAIRING.fetch_add(1, Ordering::AcqRel);
    if !account::has_pending() {
        // A pairing still waiting on the signer; a signed-in session keeps its signer.
        if state::nostr_client().is_none() {
            drop_bunker().await;
        }
        return Ok(Value::Null);
    }
    drop_bunker().await;
    MY_SECRET_KEY.clear(&[&vector_core::ENCRYPTION_KEY]);
    vector_core::ENCRYPTION_KEY.clear(&[&MY_SECRET_KEY]);
    vector_core::clear_my_public_key();
    vector_core::clear_pending_bunker_setup();
    vector_core::clear_pending_nip55_setup();
    vector_core::set_signer_kind(SignerKind::Local);
    {
        let mut g = state::PENDING_NSEC.lock().unwrap();
        if let Some(s) = g.as_mut() {
            zeroize::Zeroize::zeroize(s);
        }
        *g = None;
    }
    account::clear_pending();
    if let Some(client) = vector_core::take_nostr_client() {
        let _ = client.shutdown().await;
    }
    state::set_encryption_enabled(false);
    Ok(Value::Null)
}

/// Unlock a bunker account: bring the paired signer back and check it is still us.
pub async fn unlock_bunker(client_keys: &Keys) -> Result<PublicKey, String> {
    let url = db::get_bunker_url()
        .await
        .map_err(|e| format!("Failed to read bunker_url: {e}"))?
        .ok_or("Bunker account missing bunker_url")?;
    let remote_pk = vector_core::attempt_bunker_login(&url, client_keys.clone(), web_time::Duration::from_secs(15))
        .await
        .map_err(|e| format!("Remote signer unreachable. Please ensure your signer app is online and retry. ({e})"))?;
    let expected = db::get_bunker_remote_pubkey().ok().flatten().ok_or("Bunker account missing cached remote pubkey")?;
    if !remote_pk.to_hex().eq_ignore_ascii_case(&expected) {
        drop_bunker().await;
        return Err("Remote signer returned a different identity than this account. Either re-authorize from Settings, or logout and re-add the account.".into());
    }
    vector_core::set_signer_kind(SignerKind::Bunker);
    Ok(remote_pk)
}

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        Some(match cmd {
            "nip07_reply" => nip07_reply(a),
            "login_with_nip07" => login_with_nip07().await,
            "reauthorize_nip07" => reauthorize_nip07().await,
            "get_nip07_status" => get_nip07_status(),
            "connect_bunker" => match a.str("bunkerUrl") {
                Ok(url) => connect_bunker(url).await,
                Err(e) => Err(e),
            },
            "start_nostrconnect_session" => start_nostrconnect_session().await,
            "reauthorize_bunker" => reauthorize_bunker().await,
            "get_pending_reauth_result" => Ok(json!(PENDING_REAUTH_RESULT.lock().unwrap().take())),
            "get_bunker_status" => get_bunker_status(),
            "cancel_bunker_session" | "cancel_nip07_session" => cancel_session().await,
            "get_nip55_status" => Ok(Value::Null),
            "is_external_signer_installed" => Ok(json!(false)),
            _ => return None,
        })
    })
}
