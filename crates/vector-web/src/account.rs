//! Account lifecycle: boot probe, import, create, skip-encryption, unlock.

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::db;
use vector_core::state::{self, MNEMONIC_SEED, PENDING_NSEC};
use vector_core::{ClientRelayExt, Profile, MY_SECRET_KEY, STATE};
use zeroize::{Zeroize, Zeroizing};

use crate::emitter;

/// Accounts created but not yet committed (the PIN step is still ahead).
static PENDING_ACCOUNT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn install_client() {
    let client = vector_core::nostr_client_builder().monitor(Monitor::new(1024)).build();
    state::set_nostr_client_if_absent(client);
}

async fn insert_own_profile(npub: &str) {
    let mut profile = Profile::new();
    profile.flags.set_mine(true);
    STATE.lock().await.insert_or_replace_profile(npub, profile);
}

fn store_identity(keys: &Keys) {
    MY_SECRET_KEY.store_from_keys(keys, &[&vector_core::ENCRYPTION_KEY]);
    state::set_my_public_key(keys.public_key());
}

pub fn get_encryption_and_key() -> Result<Value, String> {
    let has_account = if db::get_current_account().is_ok() {
        true
    } else if let Some(npub) = db::read_active_account_file().ok().flatten() {
        db::set_current_account(npub.clone()).is_ok() && db::init_database(&npub).is_ok()
    } else {
        false
    };
    if !has_account {
        return Ok(json!({ "account_exists": false, "enabled": false, "security_type": "pin", "signer_type": "local" }));
    }
    state::init_encryption_enabled();
    let enabled = state::is_encryption_enabled_fast();
    let security_type = if enabled {
        db::get_sql_setting("security_type".into()).ok().flatten().unwrap_or_else(|| "pin".into())
    } else {
        "pin".into()
    };
    let signer_type = db::get_signer_type().unwrap_or_else(|_| "local".into());
    Ok(json!({ "account_exists": true, "enabled": enabled, "security_type": security_type, "signer_type": signer_type }))
}

pub fn get_encryption_status() -> Value {
    state::init_encryption_enabled();
    let enabled = state::is_encryption_enabled_fast();
    let security_type = if enabled {
        db::get_sql_setting("security_type".into()).ok().flatten().unwrap_or_else(|| "pin".into())
    } else {
        "pin".into()
    };
    json!({ "enabled": enabled, "account_exists": db::get_current_account().is_ok(), "security_type": security_type })
}

pub fn list_accounts_with_metadata() -> Value {
    let accounts = db::get_accounts().unwrap_or_default();
    Value::Array(
        accounts
            .into_iter()
            .map(|npub| {
                json!({
                    "npub": npub,
                    "display_name": null,
                    "avatar_url": null,
                    "avatar_cached": null,
                    "has_encryption": false,
                    "last_active": 0,
                })
            })
            .collect(),
    )
}

pub async fn login(mut import_key: String) -> Result<Value, String> {
    if state::nostr_client().is_some() {
        let keys = Keys::parse(&import_key).map_err(|_| "Invalid key — could not parse".to_string())?;
        import_key.zeroize();
        let current = vector_core::my_public_key().ok_or("Public key not initialized")?;
        if current == keys.public_key() {
            return Ok(json!({ "public": current.to_bech32().map_err(|e| e.to_string())?, "existing": false }));
        }
        return Err("An existing Nostr Client instance exists, but a second incompatible key import was requested.".into());
    }

    let keys = if import_key.starts_with("nsec") {
        let parsed = Keys::parse(&import_key).map_err(|_| "Invalid nsec".to_string());
        import_key.zeroize();
        parsed?
    } else {
        let phrase = Zeroizing::new(std::mem::take(&mut import_key));
        Keys::from_mnemonic(phrase.as_str(), Some("")).map_err(|_| "Invalid Seed Phrase".to_string())?
    };

    let npub = keys.public_key().to_bech32().map_err(|e| e.to_string())?;
    if db::get_accounts().unwrap_or_default().iter().any(|n| n == &npub) {
        let _ = db::write_active_account_file(&npub);
        emitter::emit("session_reload", &());
        return Ok(json!({ "public": npub, "existing": true }));
    }

    *PENDING_NSEC.lock().unwrap() = Some(keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    store_identity(&keys);
    drop(keys);
    install_client();
    insert_own_profile(&npub).await;

    if let Err(e) = db::init_database(&npub).and_then(|_| db::set_current_account(npub.clone())) {
        emitter::emit("loading_error", &e);
    }
    Ok(json!({ "public": npub, "existing": false }))
}

pub async fn create_account() -> Result<Value, String> {
    let mnemonic = bip39::Mnemonic::generate(12).map_err(|e| e.to_string())?;
    let phrase = mnemonic.to_string();
    let keys = Keys::from_mnemonic(phrase.as_str(), None).map_err(|e| e.to_string())?;

    *PENDING_NSEC.lock().unwrap() = Some(keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    store_identity(&keys);
    drop(keys);
    install_client();

    let npub = vector_core::my_public_key().ok_or("no identity")?.to_bech32().map_err(|e| e.to_string())?;
    insert_own_profile(&npub).await;
    *MNEMONIC_SEED.lock().unwrap() = Some(phrase);
    *PENDING_ACCOUNT.lock().unwrap() = Some(npub.clone());
    Ok(json!({ "public": npub, "existing": false }))
}

/// Commit the pending account with its key stored in plaintext.
pub async fn skip_encryption() -> Result<(), String> {
    let nsec = Zeroizing::new(PENDING_NSEC.lock().unwrap().clone().ok_or("No pending key — call create_account or login first")?);
    let seed = MNEMONIC_SEED.lock().unwrap().clone().map(Zeroizing::new);

    if let Some(npub) = PENDING_ACCOUNT.lock().unwrap().take() {
        db::init_database(&npub)?;
        db::set_current_account(npub)?;
    }

    db::settings::commit_account_setup(&nsec, false, None, seed.as_deref().map(|s| s.as_str()), None)?;

    if let Some(s) = PENDING_NSEC.lock().unwrap().as_mut() {
        s.zeroize();
    }
    *PENDING_NSEC.lock().unwrap() = None;
    if let Some(s) = MNEMONIC_SEED.lock().unwrap().as_mut() {
        s.zeroize();
    }
    *MNEMONIC_SEED.lock().unwrap() = None;

    state::set_encryption_enabled(false);
    vector_core::blossom_servers::refresh_cache();
    Ok(())
}

/// Unlock the stored account. Local keys only: bunker and NIP-55 need
/// transports the browser doesn't have yet.
pub async fn login_from_stored_key(password: Option<String>) -> Result<String, String> {
    state::init_encryption_enabled();

    if state::nostr_client().is_some() {
        return vector_core::my_public_key()
            .and_then(|pk| pk.to_bech32().ok())
            .ok_or_else(|| "Public key not initialized".to_string());
    }

    let signer_type = db::get_signer_type().unwrap_or_else(|_| "local".into());
    if signer_type != "local" {
        return Err(format!("{signer_type} signers are not available on Vector Web yet"));
    }

    let stored = db::get_pkey()?.ok_or("No private key found")?;
    let mut nsec = if let Some(pwd) = password {
        let key = vector_core::crypto::hash_pass(&pwd).await;
        vector_core::ENCRYPTION_KEY.set(key, &[&MY_SECRET_KEY]);
        vector_core::crypto::maybe_decrypt_inner(stored, Some(pwd))
            .await
            .map_err(|_| "Incorrect password".to_string())?
    } else {
        stored
    };
    let keys = Keys::parse(&nsec).map_err(|_| "Invalid stored key".to_string())?;
    nsec.zeroize();
    store_identity(&keys);
    drop(keys);
    install_client();

    let npub = vector_core::my_public_key().ok_or("no identity")?.to_bech32().map_err(|e| e.to_string())?;
    insert_own_profile(&npub).await;
    match db::init_database(&npub).and_then(|_| db::set_current_account(npub.clone())) {
        Ok(()) => {
            state::init_encryption_enabled();
            vector_core::blossom_servers::refresh_cache();
        }
        Err(e) => emitter::emit("loading_error", &e),
    }
    Ok(npub)
}

/// Add the default and discovery relays, then connect. False if already connected.
pub async fn connect() -> bool {
    let Some(client) = state::nostr_client() else { return false };
    if !client.relays().await.is_empty() {
        return false;
    }
    for url in state::TRUSTED_RELAYS {
        if let Err(e) = client
            .add_managed_relay(*url)
            .capabilities(RelayCapabilities::READ | RelayCapabilities::WRITE)
            .await
        {
            vector_core::log_warn!("[Relay] add {url} failed: {e}");
        }
    }
    for url in state::discovery_relay_iter() {
        if state::TRUSTED_RELAYS.iter().any(|t| t.trim_end_matches('/') == url.trim_end_matches('/')) {
            continue;
        }
        let _ = client
            .add_managed_relay(url)
            .capabilities(vector_core::discovery_relay_capabilities())
            .await;
    }
    client.connect().await;
    true
}
