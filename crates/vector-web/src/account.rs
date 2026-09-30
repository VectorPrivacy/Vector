//! Account lifecycle: boot probe, import, create, skip-encryption, unlock.

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::db;
use vector_core::state::{self, MNEMONIC_SEED, PENDING_NSEC};
use vector_core::{Profile, SignerKind, MY_SECRET_KEY, STATE};
use zeroize::{Zeroize, Zeroizing};

use crate::emitter;

/// Accounts created but not yet committed (the PIN step is still ahead).
static PENDING_ACCOUNT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The npub of a keypair generated this session, armed for its one empty kind-0.
static FRESH_ACCOUNT_NPUB: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Publish an empty kind-0 tagged `client: vector` for a keypair generated
/// moments ago, so NIP-89 readers (Magnitude's upload gate) recognise it.
/// Never for an imported or unlocked identity: a kind-0 replaces what exists.
fn stamp_fresh_account_profile() {
    let armed = FRESH_ACCOUNT_NPUB.lock().unwrap().take();
    let Some(client) = state::nostr_client() else { return };
    let current = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok());
    if armed.is_none() || armed != current {
        return;
    }
    db::spawn_bound(async move {
        let builder = EventBuilder::new(Kind::Metadata, "{}").tag(Tag::custom("client", vec!["vector"]));
        if let Ok(event) = vector_core::sign_builder(builder).await {
            let relays = state::active_trusted_relays().await;
            if let Err(e) = client.send_event(&event).to(relays).await {
                vector_core::log_warn!("[Account] new-account profile stamp failed: {e}");
            }
        }
    });
}

/// Not account creation: an abandoned creation must not stamp an empty kind-0
/// over an existing identity's profile.
pub(crate) fn disarm_fresh_account_stamp() {
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = None;
}

/// Hold an account's storage until the security step commits it.
pub(crate) fn stage_pending(npub: String) {
    *PENDING_ACCOUNT.lock().unwrap() = Some(npub);
}

/// Whether an account is staged and not yet committed.
pub(crate) fn has_pending() -> bool {
    PENDING_ACCOUNT.lock().unwrap().is_some()
}

/// Open the staged account's storage and make it the active one.
pub(crate) fn take_pending_into_current() -> Result<(), String> {
    if let Some(npub) = PENDING_ACCOUNT.lock().unwrap().take() {
        db::init_database(&npub)?;
        db::set_current_account(npub)?;
    }
    Ok(())
}

/// The shared tail of every account commit.
pub(crate) fn finish_commit() {
    touch_last_active();
    vector_core::blossom_servers::refresh_cache();
    broadcast_pending_invite_if_any();
    stamp_fresh_account_profile();
}

/// Forget an account that was started but never committed.
pub(crate) fn clear_pending() {
    *PENDING_ACCOUNT.lock().unwrap() = None;
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = None;
}

/// Publish the invite accepted before setup, once there is a client to send it.
fn broadcast_pending_invite_if_any() {
    let Some(invite) = state::pending_invite() else { return };
    let Some(client) = state::nostr_client() else { return };
    state::clear_pending_invite();
    db::spawn_bound(async move {
        let builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite_accepted")
            .tag(Tag::custom("l", vec!["vector"]))
            .tag(Tag::custom("d", vec![invite.invite_code.as_str()]))
            .tag(Tag::public_key(invite.inviter_pubkey));
        match vector_core::sign_builder(builder).await {
            Ok(event) => {
                if let Err(e) = client.send_event(&event).to(state::active_trusted_relays().await).await {
                    vector_core::log_warn!("[Account] invite acceptance broadcast failed: {e}");
                }
            }
            Err(e) => vector_core::log_warn!("[Account] invite acceptance signing failed: {e}"),
        }
    });
}

pub(crate) fn install_client() {
    let client = vector_core::nostr_client_builder().monitor(Monitor::new(1024)).build();
    state::set_nostr_client_if_absent(client);
}

pub(crate) async fn insert_own_profile(npub: &str) {
    let mut profile = Profile::new();
    profile.flags.set_mine(true);
    STATE.lock().await.insert_or_replace_profile(npub, profile);
}

fn store_identity(keys: &Keys) {
    MY_SECRET_KEY.store_from_keys(keys, &[&vector_core::ENCRYPTION_KEY]);
    state::set_my_public_key(keys.public_key());
}

/// An account on disk is real once its setup committed: a stored key, or a
/// keyless signer's identity. One abandoned before that holds neither.
pub(crate) fn is_committed(npub: &str) -> bool {
    if !db::get_accounts().unwrap_or_default().iter().any(|n| n == npub) {
        return false;
    }
    let Ok(path) = db::account_dir(npub).map(|d| d.join("vector.db")) else { return false };
    let Ok(conn) = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    conn.query_row("SELECT 1 FROM settings WHERE key IN ('pkey', 'nip55_user_pubkey') LIMIT 1", [], |_| Ok(()))
        .is_ok()
}

pub fn get_encryption_and_key() -> Result<Value, String> {
    let has_account = if db::get_current_account().is_ok() {
        true
    } else if let Some(npub) = db::read_active_account_file().ok().flatten() {
        if is_committed(&npub) {
            db::set_current_account(npub.clone()).is_ok() && db::init_database(&npub).is_ok()
        } else {
            let _ = db::clear_active_account_file();
            false
        }
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

fn account_metadata(npub: &str) -> Value {
    let mut meta = json!({
        "npub": npub, "display_name": null, "avatar_url": null,
        "avatar_cached": null, "has_encryption": false, "last_active": null,
    });
    let Ok(path) = db::account_dir(npub).map(|d| d.join("vector.db")) else { return meta };
    let Ok(conn) = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return meta;
    };
    if let Ok((nickname, display, name, avatar)) = conn.query_row(
        "SELECT nickname, display_name, name, avatar FROM profiles WHERE npub = ?1",
        [npub],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)),
    ) {
        meta["display_name"] = json!([nickname, display, name].into_iter().find(|s| !s.is_empty()));
        if !avatar.is_empty() {
            meta["avatar_url"] = json!(avatar);
        }
    }
    let setting = |key: &str| {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0)).ok()
    };
    meta["has_encryption"] =
        json!(state::resolve_encryption_enabled(setting("encryption_enabled").as_deref(), setting("security_type").as_deref()));
    meta["last_active"] = json!(setting("last_active").and_then(|v| v.parse::<i64>().ok()));
    meta
}

pub fn list_accounts_with_metadata() -> Value {
    let mut accounts: Vec<Value> = db::get_accounts()
        .unwrap_or_default()
        .iter()
        .filter(|n| is_committed(n))
        .map(|n| account_metadata(n))
        .collect();
    accounts.sort_by_key(|m| std::cmp::Reverse(m["last_active"].as_i64().unwrap_or(0)));
    Value::Array(accounts)
}

fn touch_last_active() {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = db::set_sql_setting("last_active".into(), now.to_string());
}

pub async fn login(mut import_key: String) -> Result<Value, String> {
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = None;
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
    if is_committed(&npub) {
        let _ = db::write_active_account_file(&npub);
        crate::storage::reload().await;
        return Ok(json!({ "public": npub, "existing": true }));
    }

    *PENDING_NSEC.lock().unwrap() = Some(keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    store_identity(&keys);
    drop(keys);
    install_client();
    insert_own_profile(&npub).await;
    stage_pending(npub.clone());
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
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = Some(npub.clone());
    Ok(json!({ "public": npub, "existing": false }))
}

/// Commit the pending account: its key sealed under `password`, or plaintext without one.
async fn commit_pending_account(password: Option<&str>, security_type: Option<&str>) -> Result<(), String> {
    if password.is_some_and(|p| p.trim().is_empty()) {
        return Err("Password must not be empty.".into());
    }
    if matches!(vector_core::signer_kind(), SignerKind::Nip55 | SignerKind::Nip07) {
        return crate::signers::commit_keyless(password, security_type).await;
    }
    let nsec = Zeroizing::new(PENDING_NSEC.lock().unwrap().clone().ok_or("No pending key — call create_account or login first")?);
    let seed = MNEMONIC_SEED.lock().unwrap().clone().map(Zeroizing::new);

    // Sealing with the password derives the key once and leaves it in the vault,
    // where the seed and every later at-rest write pick it up.
    let stored_key = match password {
        Some(pwd) => {
            let sealed = vector_core::crypto::maybe_encrypt_inner(nsec.to_string(), Some(pwd.to_string())).await;
            state::set_encryption_enabled(true);
            sealed
        }
        None => nsec.to_string(),
    };
    let stored_seed = match seed.as_ref() {
        Some(s) => Some(vector_core::crypto::maybe_encrypt(s.to_string()).await),
        None => None,
    };

    take_pending_into_current()?;

    // A bunker account's key is its client keypair; the pairing rides the same commit.
    if let Some((url, remote_hex)) = vector_core::pending_bunker_setup().filter(|_| vector_core::is_bunker()) {
        let stored_url = vector_core::crypto::maybe_encrypt(url).await;
        db::commit_bunker_account_setup(&stored_key, password.is_some(), security_type, &stored_url, &remote_hex, None)?;
        vector_core::clear_pending_bunker_setup();
    } else {
        db::settings::commit_account_setup(&stored_key, password.is_some(), security_type, stored_seed.as_deref(), None)?;
    }

    for slot in [&PENDING_NSEC, &MNEMONIC_SEED] {
        let mut guard = slot.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            s.zeroize();
        }
        *guard = None;
    }

    state::set_encryption_enabled(password.is_some());
    finish_commit();
    Ok(())
}

pub async fn skip_encryption() -> Result<(), String> {
    commit_pending_account(None, None).await
}

pub async fn setup_encryption(password: String, security_type: String) -> Result<(), String> {
    let password = Zeroizing::new(password);
    commit_pending_account(Some(&password), Some(&security_type)).await
}

/// Unlock the stored account: a local key, a bunker's client keypair, or a
/// keyless signer's recorded identity.
pub async fn login_from_stored_key(password: Option<String>) -> Result<String, String> {
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = None;
    state::init_encryption_enabled();

    if state::nostr_client().is_some() {
        let in_memory = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok());
        let marked = db::read_active_account_file().ok().flatten();
        // A key left from an abandoned add-account must not sign for the marked account.
        if in_memory.is_some() && in_memory == marked {
            return in_memory.ok_or_else(|| "Public key not initialized".into());
        }
        crate::storage::reload().await;
        return Err("Switching accounts".into());
    }

    let kind = SignerKind::from_setting_str(&db::get_signer_type().unwrap_or_else(|_| "local".into()));
    let public_key = match kind {
        SignerKind::Nip07 => crate::signers::unlock_keyless(kind, password).await?,
        SignerKind::Nip55 => return Err("Offline (Amber) signers are only available on Android.".into()),
        SignerKind::Local | SignerKind::Bunker => {
            let stored = db::get_pkey()?.ok_or("No private key found")?;
            let mut nsec = if let Some(pwd) = password {
                vector_core::crypto::maybe_decrypt_inner(stored, Some(pwd))
                    .await
                    .map_err(|_| "Incorrect password".to_string())?
            } else {
                stored
            };
            let keys = Keys::parse(&nsec).map_err(|_| "Invalid stored key".to_string())?;
            nsec.zeroize();
            MY_SECRET_KEY.store_from_keys(&keys, &[&vector_core::ENCRYPTION_KEY]);
            if kind == SignerKind::Bunker {
                crate::signers::unlock_bunker(&keys).await?
            } else {
                keys.public_key()
            }
        }
    };
    state::set_my_public_key(public_key);
    install_client();

    let npub = vector_core::my_public_key().ok_or("no identity")?.to_bech32().map_err(|e| e.to_string())?;
    insert_own_profile(&npub).await;
    match db::init_database(&npub).and_then(|_| db::set_current_account(npub.clone())) {
        Ok(()) => {
            touch_last_active();
            state::init_encryption_enabled();
            vector_core::blossom_servers::refresh_cache();
        }
        Err(e) => emitter::emit("loading_error", &e),
    }
    Ok(npub)
}

/// Add the configured and discovery relays, then connect. False if already connected.
pub async fn connect() -> bool {
    let Some(client) = state::nostr_client() else { return false };
    if !client.relays().await.is_empty() {
        return false;
    }
    crate::network_ops::add_configured_relays(&client).await;
    crate::network_ops::start_relay_monitor(&client);
    db::spawn_bound(async {
        vector_core::rt::time::sleep(std::time::Duration::from_millis(500)).await;
        crate::network_ops::reconcile_dm_relay_list().await;
    });
    true
}
