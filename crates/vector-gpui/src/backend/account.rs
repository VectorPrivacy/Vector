//! Account lifecycle: boot probe, import, create, encryption setup, unlock and connect.

use std::sync::Mutex;

use nostr_sdk::prelude::*;
use vector_core::db;
use vector_core::state::{self, MNEMONIC_SEED, PENDING_NSEC};
use vector_core::{MY_SECRET_KEY, Profile, STATE, SignerKind};
use zeroize::{Zeroize, Zeroizing};

/// An account created or imported, held until its security step commits it.
static PENDING_ACCOUNT: Mutex<Option<String>> = Mutex::new(None);
/// The npub of a keypair generated this session, armed for its one empty kind-0.
static FRESH_ACCOUNT_NPUB: Mutex<Option<String>> = Mutex::new(None);

/// What boot finds on disk.
#[derive(Debug, Clone)]
pub struct BootInfo {
    pub account: Option<String>,
    pub encrypted: bool,
    /// "pin" or "password".
    pub security_type: String,
    pub signer: String,
}

#[derive(Debug, Clone)]
pub struct Account {
    pub npub: String,
    pub name: Option<String>,
    pub avatar: Option<String>,
    pub encrypted: bool,
    pub last_active: i64,
}

pub enum Imported {
    /// A new identity, staged until its security step.
    Staged(String),
    /// An account already on this device: it is now the active one, to unlock.
    Existing(String),
}

/// An account on disk is real once its setup committed: a stored key, or a keyless signer's
/// identity. One abandoned before that holds neither.
fn is_committed(npub: &str) -> bool {
    if !db::get_accounts()
        .unwrap_or_default()
        .iter()
        .any(|n| n == npub)
    {
        return false;
    }
    let Ok(path) = db::account_dir(npub).map(|d| d.join("vector.db")) else {
        return false;
    };
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    conn.query_row(
        "SELECT 1 FROM settings WHERE key IN ('pkey', 'nip55_user_pubkey') LIMIT 1",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// Select the remembered account, or arm the welcome screen's network when there is none.
pub fn boot() -> BootInfo {
    let account = match db::get_current_account() {
        Ok(npub) => Some(npub),
        Err(_) => match db::read_active_account_file().ok().flatten() {
            Some(npub) if is_committed(&npub) => (db::set_current_account(npub.clone()).is_ok()
                && db::init_database(&npub).is_ok())
            .then_some(npub),
            Some(_) => {
                let _ = db::clear_active_account_file();
                None
            }
            // One committed account and no marker: it is the one.
            None => {
                let committed: Vec<String> = db::get_accounts()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|n| is_committed(n))
                    .collect();
                match committed.as_slice() {
                    [only] => (db::set_current_account(only.clone()).is_ok()
                        && db::init_database(only).is_ok())
                    .then(|| only.clone()),
                    _ => None,
                }
            }
        },
    };
    let Some(account) = account else {
        vector_core::transport::prelogin::apply_at_boot();
        return BootInfo {
            account: None,
            encrypted: false,
            security_type: "pin".into(),
            signer: "local".into(),
        };
    };
    state::init_encryption_enabled();
    let encrypted = state::is_encryption_enabled_fast();
    let security_type = if encrypted {
        db::get_sql_setting("security_type".into())
            .ok()
            .flatten()
            .unwrap_or_else(|| "pin".into())
    } else {
        "pin".into()
    };
    let signer = db::get_signer_type().unwrap_or_else(|_| "local".into());
    BootInfo {
        account: Some(account),
        encrypted,
        security_type,
        signer,
    }
}

/// Every committed account, most recently used first.
pub fn accounts() -> Vec<Account> {
    let mut out: Vec<Account> = db::get_accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|n| is_committed(n))
        .map(|n| metadata(&n))
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.last_active));
    out
}

fn metadata(npub: &str) -> Account {
    let mut meta = Account {
        npub: npub.to_string(),
        name: None,
        avatar: None,
        encrypted: false,
        last_active: 0,
    };
    let Ok(path) = db::account_dir(npub).map(|d| d.join("vector.db")) else {
        return meta;
    };
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return meta;
    };
    if let Ok((nickname, display, name, avatar_cached)) = conn.query_row(
        "SELECT nickname, display_name, name, avatar_cached FROM profiles WHERE npub = ?1",
        [npub],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        },
    ) {
        meta.name = [nickname, display, name]
            .into_iter()
            .find(|s| !s.is_empty());
        meta.avatar = (!avatar_cached.is_empty()).then_some(avatar_cached);
    }
    let setting = |key: &str| {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .ok()
    };
    meta.encrypted = state::resolve_encryption_enabled(
        setting("encryption_enabled").as_deref(),
        setting("security_type").as_deref(),
    );
    meta.last_active = setting("last_active")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    meta
}

fn touch_last_active() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = db::set_sql_setting("last_active".into(), now.to_string());
}

fn install_client() {
    let client = vector_core::nostr_client_builder()
        .monitor(Monitor::new(1024))
        .build();
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

/// Sign in with an nsec or a BIP-39 seed phrase.
pub async fn import(mut key: String) -> Result<Imported, String> {
    let keys = if key.starts_with("nsec") {
        let parsed = Keys::parse(&key).map_err(|_| "Invalid nsec".to_string());
        key.zeroize();
        parsed?
    } else {
        let phrase = Zeroizing::new(std::mem::take(&mut key));
        Keys::from_mnemonic(phrase.trim(), Some(""))
            .map_err(|_| "Invalid Seed Phrase".to_string())?
    };
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = None;
    let npub = keys.public_key().to_bech32().map_err(|e| e.to_string())?;
    if is_committed(&npub) {
        db::write_active_account_file(&npub)?;
        return Ok(Imported::Existing(npub));
    }
    if state::nostr_client().is_some() {
        return Err("Another account is already signed in.".into());
    }
    *PENDING_NSEC.lock().unwrap() = Some(keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    if let Some(stale) = MNEMONIC_SEED.lock().unwrap().as_mut() {
        stale.zeroize();
    }
    *MNEMONIC_SEED.lock().unwrap() = None;
    store_identity(&keys);
    drop(keys);
    install_client();
    insert_own_profile(&npub).await;
    *PENDING_ACCOUNT.lock().unwrap() = Some(npub.clone());
    Ok(Imported::Staged(npub))
}

/// A new identity from a fresh 12-word phrase.
pub async fn create() -> Result<String, String> {
    let mnemonic = bip39::Mnemonic::generate(12).map_err(|e| e.to_string())?;
    let phrase = mnemonic.to_string();
    let keys = Keys::from_mnemonic(phrase.as_str(), None).map_err(|e| e.to_string())?;
    *PENDING_NSEC.lock().unwrap() = Some(keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    store_identity(&keys);
    let npub = keys.public_key().to_bech32().map_err(|e| e.to_string())?;
    drop(keys);
    install_client();
    insert_own_profile(&npub).await;
    *MNEMONIC_SEED.lock().unwrap() = Some(phrase);
    *PENDING_ACCOUNT.lock().unwrap() = Some(npub.clone());
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = Some(npub.clone());
    Ok(npub)
}

/// Commit the pending account: its key sealed under `password`, or kept plain without one.
pub async fn commit(
    password: Option<Zeroizing<String>>,
    security_type: &str,
) -> Result<(), String> {
    if password.as_ref().is_some_and(|p| p.trim().is_empty()) {
        return Err("Password must not be empty.".into());
    }
    let nsec = Zeroizing::new(
        PENDING_NSEC
            .lock()
            .unwrap()
            .clone()
            .ok_or("No pending key")?,
    );
    let seed = MNEMONIC_SEED.lock().unwrap().clone().map(Zeroizing::new);

    // One derivation seals the key and the seed; the vault and the flag change only once the
    // commit has landed, so a failed commit leaves nothing behind for a retry.
    let kdf = password.as_ref().map(|_| vector_core::crypto::Kdf::fresh());
    let key = match (&password, &kdf) {
        (Some(pwd), Some(kdf)) => Some(Zeroizing::new(
            vector_core::crypto::derive_key(pwd.as_str(), kdf).await,
        )),
        _ => None,
    };
    let seal = |plain: &str| -> Result<String, String> {
        match &key {
            Some(k) => vector_core::crypto::encrypt_with_key(plain, k),
            None => Ok(plain.to_string()),
        }
    };
    let stored_key = seal(&nsec)?;
    let stored_seed = seed.as_deref().map(|s| seal(s)).transpose()?;
    let kdf_row = kdf.as_ref().and_then(|k| k.descriptor());

    let pending = PENDING_ACCOUNT.lock().unwrap().take();
    if let Some(npub) = pending {
        // The welcome screen's network carries on into the new account's first session.
        if !super::transport::running_prelogin() {
            super::transport::stop_and_join_if_running().await;
        }
        if let Err(e) = db::init_database(&npub) {
            let _ = super::transport::sync_to_active_account().await;
            return Err(e);
        }
        db::set_current_account(npub.clone())?;
        db::write_active_account_file(&npub)?;
        if let Err(e) = super::transport::sync_to_active_account().await {
            vector_core::log_warn!("[Account] network start for new account failed: {e}");
        }
    }
    let security = password.as_ref().map(|_| security_type);
    db::settings::commit_account_setup(
        &stored_key,
        password.is_some(),
        security,
        stored_seed.as_deref(),
        None,
        kdf_row.as_deref(),
    )?;
    if let Some(k) = &key {
        vector_core::ENCRYPTION_KEY.set(**k, &[&MY_SECRET_KEY]);
    }
    for slot in [&PENDING_NSEC, &MNEMONIC_SEED] {
        let mut guard = slot.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            s.zeroize();
        }
        *guard = None;
    }
    state::set_encryption_enabled(password.is_some());
    touch_last_active();
    vector_core::blossom_servers::refresh_cache();
    stamp_fresh_account_profile();
    Ok(())
}

/// Publish an empty kind-0 tagged `client: vector` for a keypair generated moments ago, so
/// NIP-89 readers recognise it. Never for an imported or unlocked identity.
fn stamp_fresh_account_profile() {
    let armed = FRESH_ACCOUNT_NPUB.lock().unwrap().take();
    let Some(client) = state::nostr_client() else {
        return;
    };
    let current = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok());
    if armed.is_none() || armed != current {
        return;
    }
    db::spawn_bound(async move {
        let builder =
            EventBuilder::new(Kind::Metadata, "{}").tag(Tag::custom("client", vec!["vector"]));
        if let Ok(event) = vector_core::sign_builder(builder).await {
            let relays = state::active_trusted_relays().await;
            if let Err(e) = client.send_event(&event).to(relays).await {
                vector_core::log_warn!("[Account] new-account profile stamp failed: {e}");
            }
        }
    });
}

/// Forget an account that was started but never committed.
pub fn abandon_pending() {
    *PENDING_ACCOUNT.lock().unwrap() = None;
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = None;
}

/// Unlock the stored account: its key opened under `password` when it is encrypted.
pub async fn unlock(password: Option<Zeroizing<String>>) -> Result<String, String> {
    *FRESH_ACCOUNT_NPUB.lock().unwrap() = None;
    state::init_encryption_enabled();
    if let Some(pk) = vector_core::my_public_key().filter(|_| state::nostr_client().is_some()) {
        return pk.to_bech32().map_err(|e| e.to_string());
    }
    {
        use vector_core::transport::{self, kinds, prefs, prelogin, Kind};
        let effective = prelogin::effective(&prefs::stored_kind());
        transport::set_preference(effective);
        // A welcome-screen instance is kept only where its kind adopts on unlock (Tor never does:
        // its guards are install-level).
        let adopt = transport::host::active().is_some_and(|a| {
            a.started_prelogin()
                && Some(a.kind) == effective
                && kinds::factory(a.kind).is_some_and(|f| {
                    f.adopt_on_unlock() && f.compatible(a.transport().as_ref(), &prelogin::config_for(a.kind))
                })
        });
        if !adopt {
            super::transport::stop_prelogin_service().await;
        }
        // Started now, awaited once the key has opened: a wrong password never waits on tunnels.
        if effective.is_some_and(|k| k != Kind::Clearnet)
            && let Err(e) = super::transport::start_for_active_account().await
        {
            vector_core::log_warn!("[Login] network auto-start failed: {e}");
        }
    }

    let kind =
        SignerKind::from_setting_str(&db::get_signer_type().unwrap_or_else(|_| "local".into()));
    if !matches!(kind, SignerKind::Local) {
        return Err(
            "This account signs with an external signer, which this client cannot use yet.".into(),
        );
    }
    let stored = db::get_pkey()?.ok_or("No private key found")?;
    let mut nsec = match &password {
        Some(pwd) => {
            // The vault takes the key only once it has opened the stored key.
            let kdf = vector_core::crypto::Kdf::of_account()?;
            let key = Zeroizing::new(vector_core::crypto::derive_key(pwd.as_str(), &kdf).await);
            let nsec = vector_core::crypto::decrypt_with_key(&stored, &key)
                .map_err(|_| "Incorrect password".to_string())?;
            vector_core::crypto::install_unlocked_key(&key, &kdf)?;
            match db::at_rest::upgrade_after_unlock(pwd, &key).await {
                Ok(true) => {
                    let _ = db::at_rest::vacuum_now();
                }
                Ok(false) => {}
                Err(e) => vector_core::log_warn!("[Encryption] Salt upgrade deferred: {e}"),
            }
            nsec
        }
        None => stored,
    };
    let keys = Keys::parse(&nsec).map_err(|_| "Invalid stored key".to_string())?;
    nsec.zeroize();
    store_identity(&keys);
    drop(keys);
    super::transport::wait_for_active_network().await;
    install_client();

    let npub = vector_core::my_public_key()
        .ok_or("no identity")?
        .to_bech32()
        .map_err(|e| e.to_string())?;
    insert_own_profile(&npub).await;
    db::init_database(&npub)?;
    db::set_current_account(npub.clone())?;
    touch_last_active();
    state::init_encryption_enabled();
    if let Err(e) = db::at_rest::backfill_at_rest() {
        vector_core::log_warn!("[Login] community at-rest backfill deferred: {e}");
    }
    vector_core::blossom_servers::refresh_cache();
    Ok(npub)
}

/// Add the configured and discovery relays, then connect. False if already connected.
pub async fn connect() -> bool {
    let Some(client) = state::nostr_client() else {
        return false;
    };
    if !client.relays().await.is_empty() {
        return false;
    }
    super::network::add_configured_relays(&client).await;
    super::network::start_relay_monitor(&client);
    db::spawn_bound(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        super::network::reconcile_dm_relay_list().await;
    });
    true
}

/// Make `npub` the account the next boot opens.
pub fn set_active(npub: &str) -> Result<(), String> {
    db::write_active_account_file(npub)
}

/// How many accounts this device may add up to: the soft cap of its highest tier.
pub fn account_cap() -> usize {
    [3, 6, 9, usize::MAX][vector_core::badges::max_account_tier().min(3) as usize]
}

static RESET_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Tear the live account down to a fresh boot: its client, database, keys and transfers.
pub async fn reset() {
    let _serial = RESET_LOCK.lock().await;
    super::attachments::cancel_all();
    super::messaging::cancel_all_uploads();
    crate::miniapp::end_for_account_swap().await;
    vector_core::VectorCore.swap_session().await;
    super::market::reset().await;
    if let Some(bunker) = vector_core::drain_bunker_state() {
        let _ = bunker.shutdown().await;
    }
    state::clear_pending_bunker_setup();
    vector_core::transport::prelogin::cancel_start();
    super::transport::stop_and_join_if_running().await;
    super::transport::set_account_booted(false);
    db::clear_current_account_in_memory();
    abandon_pending();
    state::set_encryption_enabled(false);
    state::open_processing_gate();
}

/// Switch to another account on this device: the next boot opens it.
pub async fn switch(npub: String) -> Result<(), String> {
    super::security::refuse_if_migrating()?;
    if !accounts().iter().any(|a| a.npub == npub) {
        return Err(format!("Unknown account: {npub}"));
    }
    set_active(&npub)?;
    reset().await;
    Ok(())
}

/// Remove an account and everything it keeps on this device. True when it was the live one;
/// the marker then points at the most recent account left, if any.
pub async fn delete(npub: String) -> Result<bool, String> {
    super::security::refuse_if_migrating()?;
    if !accounts().iter().any(|a| a.npub == npub) {
        return Err(format!("Unknown account: {npub}"));
    }
    let was_active = db::get_current_account().is_ok_and(|a| a == npub);
    if was_active {
        let _ = db::clear_active_account_file();
        reset().await;
    } else if db::read_active_account_file()?.is_some_and(|m| m == npub) {
        let _ = db::clear_active_account_file();
    }
    let dir = db::account_dir(&npub)?;
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("Failed to remove account directory: {e}"));
        }
        _ => {}
    }
    // Counted on disk, not validated: a locked database must not read as no account at all.
    if db::list_account_npubs()
        .map(|v| v.is_empty())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_dir_all(db::get_download_dir());
    }
    if was_active {
        for next in accounts() {
            if set_active(&next.npub).is_ok() {
                break;
            }
        }
    }
    Ok(was_active)
}
