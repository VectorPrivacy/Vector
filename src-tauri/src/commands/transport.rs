//! The network lifecycle: one switch for every kind, in failsafe order, and the commands for
//! the generic transport.
//!
//! Tightening (into Tor, I2P, …) refuses in memory first, then persists, then starts the
//! kind; loosening (into Clearnet) persists first and stops the kind before anything may go
//! direct. Every step runs under one lifecycle lock, for the live session only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use vector_core::transport::aliases::{self, AliasEntry, AliasSource};
use vector_core::transport::prelogin::{self, PreloginChoice};
use vector_core::transport::status::{self, RouteView, TransportStateView};
use vector_core::transport::{self, host, i2p_config, Kind, StartCtx};

/// One start or stop at a time across the welcome screen and the account paths.
static LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// An account has booted in this session: the welcome screen's choice no longer applies.
static ACCOUNT_BOOTED: AtomicBool = AtomicBool::new(false);

/// A relay list or dialog never asks for more routes than this at once.
const MAX_ROUTES: usize = 512;

pub fn set_account_booted(booted: bool) {
    ACCOUNT_BOOTED.store(booted, Ordering::Release);
}

pub fn account_booted() -> bool {
    ACCOUNT_BOOTED.load(Ordering::Acquire)
}

/// A command bound to an account that is no longer on screen acts on nothing.
pub fn ensure_live() -> Result<(), String> {
    if vector_core::db::current_session_id() == vector_core::db::live_session_id() {
        Ok(())
    } else {
        Err("The account changed. Try again.".into())
    }
}

/// Is the running instance the welcome screen's (install-level dirs) rather than an account's?
pub fn running_prelogin() -> bool {
    host::active().is_some_and(|a| a.started_prelogin())
}

fn parse_kind(kind: &str) -> Result<Kind, String> {
    let k = Kind::parse(kind).ok_or_else(|| format!("Unknown network: {kind}"))?;
    if !k.compiled() {
        return Err(status::not_in_build(k).text);
    }
    Ok(k)
}

/// The inputs a kind's start takes: Tor its directories and bridges, others their config. Only
/// for `owner` while it is the live session: the dirs and config are the live account's.
async fn start_ctx(kind: Kind, prelogin: bool, owner: u64) -> Result<StartCtx, String> {
    if owner != vector_core::db::live_session_id() {
        return Err("The account changed. Try again.".into());
    }
    match kind {
        #[cfg(feature = "tor")]
        Kind::Tor => {
            let (dirs, bridges) = if prelogin {
                (vector_core::tor::prelogin_dirs()?, Vec::new())
            } else {
                (super::tor::tor_data_dirs().await?, super::tor::read_saved_bridges())
            };
            Ok(StartCtx {
                owner,
                started_prelogin: prelogin,
                dirs: Some(dirs),
                config: std::sync::Arc::new(vector_core::tor::TorStartConfig { bridges }),
            })
        }
        _ => Ok(StartCtx { owner, started_prelogin: prelogin, dirs: None, config: prelogin::config_for(kind) }),
    }
}

/// Start `kind` for `owner` (the session the caller acts for) and install it. Refused unless
/// `owner` is live; a start that lands once it no longer is shuts itself down. `prelogin` is the
/// welcome screen's generation the start was wanted at: another pick drops it mid-flight, so a
/// network nobody chose stops contacting anything. Ok(false): superseded, nothing installed.
async fn start_kind(kind: Kind, prelogin: Option<u64>, owner: u64) -> Result<bool, String> {
    let factory = transport::kinds::factory(kind).ok_or_else(|| status::not_in_build(kind).text)?;
    let ctx = start_ctx(kind, prelogin.is_some(), owner).await?;
    let inst = match prelogin {
        Some(generation) => tokio::select! {
            started = factory.start(ctx) => started?,
            _ = prelogin::superseded(generation) => return Ok(false),
        },
        None => factory.start(ctx).await?,
    };
    if owner != vector_core::db::live_session_id() {
        inst.shutdown().await;
        return Err("The account changed. Try again.".into());
    }
    // Landed after the choice moved on: installing it would only cut the sockets of the network
    // in use, twice.
    if transport::preference() != Some(kind) || prelogin.is_some_and(|g| g != prelogin::generation()) {
        inst.shutdown().await;
        return Ok(false);
    }
    host::activate(inst, owner, prelogin.is_some())?;
    Ok(true)
}

/// Stop the installed instance; a pre-login one is joined so its directories are free.
async fn stop_active() {
    let join = running_prelogin();
    host::deactivate(join).await;
}

/// An instance of `kind` owned by the live session that can serve the config it would start
/// with now (I2P: the same router).
fn running_compatible(kind: Kind) -> bool {
    let live = vector_core::db::live_session_id();
    host::active().is_some_and(|a| {
        a.kind == kind
            && a.owner() == live
            && transport::kinds::factory(kind).is_some_and(|f| f.compatible(a.transport().as_ref(), &prelogin::config_for(kind)))
    })
}

/// The view of the live session, emitted and returned by every command here.
fn view() -> TransportStateView {
    status::view()
}

fn emit_view() {
    vector_core::traits::emit_event("transport_state", &view());
}

/// Match the installed instance to the live account's preference: keep one it owns and can
/// serve, replace anything else. A kind that reports ready later is waited for (bounded) so the
/// first sync runs on a connected network.
pub async fn sync_to_active_account() -> Result<(), String> {
    start_for_active_account().await?;
    wait_for_active_network().await;
    Ok(())
}

/// [`sync_to_active_account`] without the wait: a kind that starts in the background (I2P)
/// builds its tunnels while sign-in does its own work.
pub async fn start_for_active_account() -> Result<(), String> {
    {
        let _serial = LIFECYCLE.lock().await;
        // The account on screen when the lock was taken: a swap during the stop below must not
        // get this start.
        let owner = vector_core::db::live_session_id();
        match transport::preference() {
            None => {
                host::deactivate(true).await;
                return Err("The network preference isn't loaded.".into());
            }
            Some(Kind::Clearnet) => {
                if host::active().is_some() {
                    stop_active().await;
                }
            }
            Some(k) => {
                if !running_compatible(k) {
                    stop_active().await;
                    start_kind(k, None, owner).await?;
                }
            }
        }
    }
    vector_core::net::forget_clients();
    Ok(())
}

/// Wait for the live account's network when it is still coming up, within its start budget.
pub async fn wait_for_active_network() {
    let starting = transport::preference().is_some_and(|k| k != Kind::Clearnet)
        && host::active().is_some_and(|a| !a.transport().ready());
    if starting {
        wait_until_ready(&vector_core::db::live_session()).await;
    }
}

/// How long sign-in waits on a router that isn't answering at all.
const ROUTER_GRACE: Duration = Duration::from_secs(10);

/// Hold sign-in until the network is up, within its start budget. A router that turned Vector
/// down, or that stays silent, needs the user rather than more time: sign-in then continues
/// blocked, and the Ready listener reconnects once it answers.
async fn wait_until_ready(session: &vector_core::db::Session) {
    let start = std::time::Instant::now();
    let deadline = start + transport::budget(transport::Op::Startup, Duration::ZERO);
    let mut silent_since: Option<std::time::Instant> = None;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if transport::wait_ready(left.min(Duration::from_secs(2))).await.is_ok() || left.is_zero() {
            return;
        }
        if session.stopped() || !session.is_live() {
            return;
        }
        let code = transport::preference().and_then(status::reason_for).map(|r| r.code);
        match code.as_deref() {
            Some(c) if c.starts_with("sam_") => return,
            Some("router_unreachable") => {
                if silent_since.get_or_insert_with(std::time::Instant::now).elapsed() >= ROUTER_GRACE {
                    return;
                }
            }
            _ => silent_since = None,
        }
    }
}

/// Stop whatever runs, returning at once; the instance finishes its shutdown on its own.
#[allow(dead_code)]
pub fn stop_if_running() {
    if let Some(a) = host::uninstall() {
        // spawn-detached: finishes the uninstalled instance's own shutdown.
        drop(vector_core::transport::spawn_on(async move { a.transport().shutdown().await }));
    }
}

/// Stop whatever runs and wait until the bridge holds none of it (logout wipes its dirs next).
pub async fn stop_and_join_if_running() {
    host::deactivate(true).await;
}

/// Android background teardown: Tor restarts per pause; other kinds keep their router sessions
/// for the life of the process, so a resume finds them up.
#[allow(dead_code)]
pub async fn stop_tor_if_running() {
    if host::active().is_some_and(|a| a.kind == Kind::Tor) {
        host::deactivate(true).await;
    }
}

/// Stop the welcome screen's instance, waiting out a start still in flight so it can't land
/// after an account's own has taken over.
pub async fn stop_prelogin_service() {
    prelogin::cancel_start();
    let _serial = LIFECYCLE.lock().await;
    if running_prelogin() {
        host::deactivate(true).await;
    }
}

/// Cycle the main client's relays onto the current circuits (New circuit, bridges, circuit mode).
#[cfg_attr(not(feature = "tor"), allow(dead_code))]
pub async fn switch_relay_transport() {
    transport::cycle::cycle_all(transport::cycle::CycleScope::Circuits).await;
}

/// The stored kind of the account picked on the unlock screen, or nothing when none is picked.
fn picked_account_stored() -> transport::prefs::StoredKind {
    if vector_core::db::get_current_account().is_ok() {
        transport::prefs::stored_kind()
    } else {
        transport::prefs::StoredKind::Absent
    }
}

/// What the welcome screen picked for `kind` before: the armed choice, else the remembered one.
fn remembered_opts(kind: Kind) -> Option<Value> {
    prelogin::armed()
        .filter(|c| c.kind == kind)
        .or_else(|| prelogin::marker().ok().flatten().filter(|c| c.kind == kind))
        .map(|c| c.opts)
}

/// A welcome-screen choice with only the options its kind reads; ones left out keep the earlier
/// pick. SAM credentials stay in memory until an account commits; the marker never holds them.
pub fn prelogin_choice(kind: &str, opts: Option<&Value>) -> Result<PreloginChoice, String> {
    let k = parse_kind(kind)?;
    let mut choice = PreloginChoice::new(k);
    if k == Kind::I2p {
        let port = match opts.and_then(|o| o.get("sam_port")).filter(|v| !v.is_null()) {
            Some(v) => Some(i2p_config::validate_port(v.as_i64().unwrap_or(0))?),
            None => remembered_opts(k)
                .and_then(|o| o.get("sam_port").and_then(Value::as_i64))
                .and_then(|p| i2p_config::validate_port(p).ok()),
        };
        if let Some(p) = port {
            choice.opts = serde_json::json!({ "sam_port": p });
        }
        let given = opts.filter(|o| o.get("sam_user").is_some() || o.get("sam_password").is_some());
        choice.auth = match given {
            Some(o) => {
                let text = |key: &str| o.get(key).and_then(Value::as_str);
                i2p_config::validate_sam_auth(text("sam_user"), text("sam_password"))?
                    .map(|(user, password)| prelogin::Credentials { user, password: password.into() })
            }
            // Left out: the ones given before, for the same router only.
            None => prelogin::armed()
                .filter(|c| c.kind == k && c.opts.get("sam_port").and_then(Value::as_u64) == port.map(u64::from))
                .and_then(|c| c.auth),
        };
    }
    Ok(choice)
}

/// Add Profile's choice: the account being left lends its SAM login to a new account on the same
/// router, unless the choice brings its own.
pub fn add_account_choice(kind: &str, opts: Option<&Value>) -> Result<PreloginChoice, String> {
    let brings_own = opts.is_some_and(|o| o.get("sam_user").is_some() || o.get("sam_password").is_some());
    if Kind::parse(kind) != Some(Kind::I2p) || brings_own {
        return prelogin_choice(kind, opts);
    }
    let saved = prelogin::config_for(Kind::I2p);
    let Some(cfg) = saved.downcast_ref::<i2p_config::I2pConfig>() else { return prelogin_choice(kind, opts) };
    let (Some(user), Some(password)) = (cfg.sam_user.as_ref(), cfg.sam_password.as_ref()) else { return prelogin_choice(kind, opts) };
    let chosen = opts.and_then(|o| o.get("sam_port")).and_then(Value::as_u64);
    if chosen.is_some_and(|p| p != u64::from(cfg.sam_port)) {
        return prelogin_choice(kind, opts);
    }
    let mut choice = prelogin_choice(kind, Some(&serde_json::json!({ "sam_port": cfg.sam_port })))?;
    choice.auth = Some(prelogin::Credentials { user: user.clone(), password: password.clone() });
    Ok(choice)
}

/// Match the transport to the welcome screen's choice while no account is signed in.
pub async fn apply_prelogin(choice: PreloginChoice) -> Result<(), String> {
    // Another network: whatever starts for the last pick stops now, before this queues behind it.
    if choice.kind != Kind::Clearnet && prelogin::armed().is_some_and(|a| a.kind != choice.kind) {
        prelogin::cancel_start();
    }
    // Read before queueing on the lock: a cancel issued while this waits must still count.
    let generation = prelogin::generation();
    if choice.kind == Kind::Clearnet {
        prelogin::disarm();
        prelogin::cancel_start();
        #[cfg(feature = "tor")]
        vector_core::tor::clear_last_bootstrap_error();
        // Leaving takes effect now, never behind a stalled start: with the preference off the
        // starting kind nothing can use it, and a censored network must not hold the user here.
        transport::set_preference(prelogin::effective(&picked_account_stored()));
        match LIFECYCLE.try_lock() {
            Ok(_serial) => {
                if running_prelogin() {
                    host::deactivate(true).await;
                }
            }
            // A start in flight stops itself on the moved generation; this clears one that
            // landed just before the cancel.
            Err(_) => {
                vector_core::db::spawn_bound(async {
                    let _serial = LIFECYCLE.lock().await;
                    if running_prelogin() && host::active().map(|a| a.kind) != transport::preference() {
                        host::deactivate(true).await;
                    }
                });
            }
        }
        return Ok(());
    }
    let kind = choice.kind;
    prelogin::arm(choice);
    let effective = prelogin::effective(&picked_account_stored());
    transport::set_preference(effective);
    if effective != Some(kind) {
        // The picked account stored another kind: its own starts at unlock.
        let _serial = LIFECYCLE.lock().await;
        if host::active().is_some_and(|a| a.started_prelogin() && a.kind != effective.unwrap_or(Kind::Clearnet)) {
            host::deactivate(true).await;
        }
        return Ok(());
    }
    #[cfg(feature = "tor")]
    if kind == Kind::Tor {
        // No account yet, so no saved circuit mode: the default.
        vector_core::tor::set_multi_circuit(true);
    }
    let _serial = LIFECYCLE.lock().await;
    if prelogin::generation() != generation {
        return Ok(());
    }
    if running_compatible(kind) {
        return Ok(());
    }
    let owner = vector_core::db::live_session_id();
    if host::active().is_some() {
        host::deactivate(true).await;
    }
    start_kind(kind, Some(generation), owner).await?;
    Ok(())
}

/// The welcome screen's choice: remembered for the install unless `remember` is false (Add
/// Profile keeps its own), then applied.
pub async fn set_prelogin(kind: &str, remember: Option<bool>, opts: Option<&Value>) -> Result<(), String> {
    if account_booted() {
        return Err("The network can only be changed here before signing in.".into());
    }
    if vector_core::signer::pairing_pending() {
        return Err("Finish or cancel the signer connection first.".into());
    }
    let choice = prelogin_choice(kind, opts)?;
    if remember != Some(false) {
        prelogin::set_marker(Some(&choice))?;
    }
    let applied = apply_prelogin(choice).await;
    emit_view();
    applied
}

/// Leaving Add Profile after its commit: the account being returned to keeps its own choice.
pub async fn prelogin_abandon() {
    prelogin::disarm();
    stop_prelogin_service().await;
}

/// The live network state. Cheap; safe to poll.
#[tauri::command]
pub fn transport_get_state() -> TransportStateView {
    view()
}

/// Switch the live account's network. Tor awaits its bootstrap; other kinds return once started.
#[tauri::command]
pub async fn transport_set(kind: String, keep_realtime: Option<bool>) -> Result<TransportStateView, String> {
    vector_core::db::scoped_result(set_kind(kind, keep_realtime)).await
}

async fn set_kind(kind: String, keep_realtime: Option<bool>) -> Result<TransportStateView, String> {
    let k = parse_kind(&kind)?;
    let _serial = LIFECYCLE.lock().await;
    ensure_live()?;
    if status::call_active() {
        return Err("End the call to change networks.".into());
    }
    let live = vector_core::db::live_session_id();
    let current = transport::preference();
    if current == Some(k) && (k == Kind::Clearnet || host::active().is_some_and(|a| a.kind == k && a.owner() == live)) {
        return Ok(view());
    }

    if k == Kind::Clearnet {
        // Loosening: nothing changes unless the choice is saved, and the old kind stops before
        // anything may go direct.
        transport::prefs::persist_kind(Kind::Clearnet)?;
        stop_active().await;
        // The preference below is the live session's: never another account's.
        ensure_live()?;
        #[cfg(feature = "tor")]
        vector_core::tor::clear_last_bootstrap_error();
        transport::set_preference(Some(Kind::Clearnet));
        transport::cycle::cycle_all(transport::cycle::CycleScope::Switch).await;
        transport::cycle::cycle_all(transport::cycle::CycleScope::Kick).await;
        emit_view();
        return Ok(view());
    }

    // Tightening: refuse in memory before anything slow, so no socket outlives the decision.
    transport::set_preference(Some(k));
    // NIP-46 clients dial outside the guarded sockets the bump just closed: shut them before
    // anything below can sign.
    vector_core::signer::suspend_bunker().await;
    let saved = transport::prefs::persist_kind(k);
    if keep_realtime != Some(true) {
        if let Some(app) = crate::TAURI_APP.get() {
            crate::miniapps::commands::end_realtime(app).await;
        }
    }
    transport::cycle::cycle_all(transport::cycle::CycleScope::Switch).await;
    stop_active().await;
    let started = start_kind(k, None, live).await;
    emit_view();
    started?;
    saved.map_err(|_| "Couldn't save the network choice. It applies until Vector restarts.".to_string())?;
    Ok(view())
}

/// The user accepted that calls and multiplayer Mini Apps connect outside the network in use,
/// for this account and network.
#[tauri::command]
pub fn transport_allow_realtime() -> TransportStateView {
    transport::realtime::grant();
    view()
}

/// The welcome screen's network: only before sign-in. I2P takes `{ "sam_port": 7656 }`.
#[tauri::command]
pub async fn transport_set_prelogin(kind: String, remember: Option<bool>, mut opts: Option<Value>) -> Result<TransportStateView, String> {
    let set = set_prelogin(&kind, remember, opts.as_ref()).await;
    if let Some(o) = opts.as_mut() {
        transport::prefs::scrub_json(o);
    }
    set?;
    Ok(view())
}

/// Leaving the welcome screen's choice behind (Add Profile backed out after its commit).
#[tauri::command]
pub async fn transport_prelogin_abandon() {
    prelogin_abandon().await;
}

/// Try the chosen network again now. `newIdentity`: unlinkable from here on (I2P: new
/// addresses; Tor: new circuits).
#[tauri::command]
pub async fn transport_retry(new_identity: Option<bool>) -> Result<TransportStateView, String> {
    vector_core::db::scoped_result(retry(new_identity.unwrap_or(false))).await
}

async fn retry(new_identity: bool) -> Result<TransportStateView, String> {
    let owner = vector_core::db::current_session_id();
    let welcome = {
        let _serial = LIFECYCLE.lock().await;
        ensure_live()?;
        let Some(k) = transport::preference().filter(|k| *k != Kind::Clearnet) else { return Ok(view()) };
        if !k.compiled() {
            return Err(status::not_in_build(k).text);
        }
        let live = vector_core::db::live_session_id();
        match host::active().filter(|a| a.kind == k && a.owner() == live) {
            Some(a) => {
                let t = a.transport().clone();
                if !new_identity {
                    kick(k);
                } else if k == Kind::Tor {
                    t.new_identity().await;
                    switch_relay_transport().await;
                } else {
                    // spawn-detached: the instance's own router work, on the runtime that owns its sockets; awaited.
                    let _ = transport::spawn_on(async move { t.new_identity().await }).await;
                }
                None
            }
            // Nothing running for the chosen kind (a failed start): start it the way it began.
            None => {
                let signed_in = vector_core::db::get_current_account().is_ok() && vector_core::state::my_public_key().is_some();
                match prelogin::armed().filter(|c| c.kind == k && !account_booted() && !signed_in) {
                    Some(choice) => Some(choice),
                    None => {
                        stop_active().await;
                        start_kind(k, None, owner).await?;
                        None
                    }
                }
            }
        }
    };
    if let Some(choice) = welcome {
        apply_prelogin(choice).await?;
    }
    emit_view();
    Ok(view())
}

/// Wake a kind that is waiting between attempts.
fn kick(kind: Kind) {
    #[cfg(feature = "i2p")]
    if kind == Kind::I2p {
        if let Some(t) = vector_core::i2p::active() {
            t.retry_now();
        }
    }
    #[cfg(not(feature = "i2p"))]
    let _ = kind;
}

/// How each URL is reached on the live account's network right now.
#[tauri::command]
pub fn transport_get_routes(urls: Vec<String>) -> Vec<RouteView> {
    urls.iter().take(MAX_ROUTES).map(|u| transport::route_view(u)).collect()
}

/// Every server the account gave an address on another network.
#[tauri::command]
pub fn transport_get_aliases() -> Vec<AliasEntry> {
    aliases::table().entries()
}

/// Set (or with no address, remove) a server's address on `kind`. A new I2P address is checked
/// as soon as I2P is up.
#[tauri::command]
pub async fn transport_set_alias(host: String, kind: String, address: Option<String>) -> Result<Option<AliasEntry>, String> {
    vector_core::db::scoped_result(set_alias(host, kind, address)).await
}

async fn set_alias(host: String, kind: String, address: Option<String>) -> Result<Option<AliasEntry>, String> {
    let k = Kind::parse(&kind).ok_or_else(|| format!("Unknown network: {kind}"))?;
    let address = address.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    let entry = {
        let _serial = LIFECYCLE.lock().await;
        ensure_live()?;
        aliases::set(&host, k, address.as_deref(), AliasSource::User)?
    };
    #[cfg(feature = "i2p")]
    if entry.is_some() && k == Kind::I2p {
        vector_core::i2p::probe::check_unchecked();
    }
    emit_view();
    revive_relays();
    Ok(entry)
}

/// Check a server's I2P address now: a TLS handshake for the server's own name over I2P.
#[tauri::command]
pub async fn transport_check_alias(host: String) -> Result<AliasEntry, String> {
    vector_core::db::scoped_result(check_alias(host)).await
}

async fn check_alias(host: String) -> Result<AliasEntry, String> {
    ensure_live()?;
    #[cfg(feature = "i2p")]
    {
        let session = vector_core::db::current_session();
        // spawn-detached: pinned to the caller's account by with_session and awaited; runs where the router sockets live.
        let checked = transport::spawn_on(vector_core::db::with_session(session, async move {
            vector_core::i2p::probe::check_alias(&host).await
        }))
        .await
        .map_err(|e| e.to_string())?;
        emit_view();
        checked
    }
    #[cfg(not(feature = "i2p"))]
    {
        let _ = host;
        Err("Connect to I2P to check this address.".into())
    }
}

/// Ask one of the user's own relays for its I2P address (NIP-11), then check it. Nothing is
/// saved: the user decides.
#[tauri::command]
pub async fn transport_find_alias(url: String) -> Result<Value, String> {
    vector_core::db::scoped_result(find_alias(url)).await
}

async fn find_alias(url: String) -> Result<Value, String> {
    ensure_live()?;
    if !super::relays::is_own_relay(&url).await {
        return Err("Vector looks up I2P addresses only for your own relays.".into());
    }
    #[cfg(feature = "i2p")]
    {
        let session = vector_core::db::current_session();
        // spawn-detached: pinned to the caller's account by with_session and awaited; runs where the router sockets live.
        let found = transport::spawn_on(vector_core::db::with_session(session, async move {
            vector_core::i2p::probe::find_alias(&url).await
        }))
        .await
        .map_err(|e| e.to_string())??;
        serde_json::to_value(found).map_err(|e| e.to_string())
    }
    #[cfg(not(feature = "i2p"))]
    {
        let _ = url;
        Err("Connect to I2P to check this address.".into())
    }
}

/// Save the I2P router's port and credentials, then move a running I2P onto that router.
/// `auth`: `None` keeps the saved credentials, `Some(None)` clears them.
pub(crate) async fn set_i2p_router(port: i64, auth: Option<Option<(String, String)>>) -> Result<TransportStateView, String> {
    let _serial = LIFECYCLE.lock().await;
    ensure_live()?;
    i2p_config::set_router(port, auth)?;
    let restarted = restart_if_incompatible(Kind::I2p).await;
    emit_view();
    restarted?;
    Ok(view())
}

/// Under the lifecycle lock: a running instance of the live account's `kind` that no longer
/// serves its config is replaced.
async fn restart_if_incompatible(kind: Kind) -> Result<(), String> {
    if transport::preference() != Some(kind) || running_compatible(kind) {
        return Ok(());
    }
    let live = vector_core::db::live_session_id();
    if !host::active().is_some_and(|a| a.kind == kind && a.owner() == live) {
        return Ok(());
    }
    stop_active().await;
    start_kind(kind, None, live).await.map(|_| ())
}

/// I2P-Only on (`Off`) or off (`Allow`). Turning it on cuts every outproxy connection first.
pub(crate) async fn set_i2p_exit(mode: transport::ExitPolicy) -> Result<TransportStateView, String> {
    let saved = {
        let _serial = LIFECYCLE.lock().await;
        ensure_live()?;
        i2p_config::set_exit(mode)
    };
    emit_view();
    saved?;
    if mode == transport::ExitPolicy::Allow {
        revive_relays();
    }
    Ok(view())
}

/// Save the outproxy list (`None` restores the built-in one). Removing or turning one off cuts
/// its connections first.
pub(crate) async fn set_i2p_outproxies(list: Option<Vec<i2p_config::OutproxyInput>>) -> Result<i2p_config::I2pConfigView, String> {
    let saved = {
        let _serial = LIFECYCLE.lock().await;
        ensure_live()?;
        i2p_config::set_outproxies(list)
    };
    emit_view();
    if saved.is_ok() {
        revive_relays();
    }
    saved
}

/// A routing change may have opened relays the old rules refused, which no revival pass tries
/// while refused: kick them now, off the lock.
fn revive_relays() {
    if matches!(transport::state(), transport::TransportState::Active { .. }) {
        // spawn-detached: revives the live session's relays; reads no other account state.
        tauri::async_runtime::spawn(transport::cycle::cycle_all(transport::cycle::CycleScope::Kick));
    }
}

/// Revive relays whenever the live account's network becomes usable: its instance reports ready,
/// or a blocked account lands on Clearnet (its network loaded after a client was built). Runs on
/// the app's runtime: relay tasks are spawned on the runtime that connects them.
pub fn spawn_ready_listener() {
    fn usable() -> bool {
        matches!(transport::state(), transport::TransportState::Clearnet | transport::TransportState::Active { .. })
    }
    // spawn-detached: process-lifetime listener; each event is checked against the live session.
    tauri::async_runtime::spawn(async {
        let mut rx = host::subscribe();
        let mut was_usable = usable();
        loop {
            let kick = match rx.recv().await {
                Ok(host::TransportEvent::Ready { instance, owner, .. }) => {
                    host::active().is_some_and(|a| a.id() == instance) && owner == vector_core::db::live_session_id()
                }
                Ok(_) => usable() && !was_usable,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => usable() && !was_usable,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            was_usable = usable();
            if kick {
                // spawn-detached: revives the live session's relays; reads no other account state.
                tauri::async_runtime::spawn(transport::cycle::cycle_all(transport::cycle::CycleScope::Kick));
            }
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    #[cfg(feature = "i2p")]
    use vector_core::transport::Egress;
    use vector_core::transport::{status, BoxedStream, ConnectError, Dest, Dialed, Lane, Route, RouteCtx, Transport};

    /// The transport slot, the live session and the welcome screen's choice are process-wide:
    /// every test that moves them holds this.
    pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A ready instance of any kind that dials nothing and counts its shutdowns.
    struct Idle {
        kind: Kind,
        shutdowns: AtomicUsize,
    }

    impl Idle {
        fn new(kind: Kind) -> Arc<Self> {
            Arc::new(Idle { kind, shutdowns: AtomicUsize::new(0) })
        }
    }

    #[async_trait::async_trait]
    impl Transport for Idle {
        fn kind(&self) -> Kind {
            self.kind
        }
        fn route(&self, dest: &Dest, port: u16, ctx: &RouteCtx) -> Route {
            transport::route::route_tor(dest, port, ctx)
        }
        async fn dial(&self, _route: &Route, _lane: Lane) -> Result<(BoxedStream, Dialed), ConnectError> {
            Err(ConnectError::Unreachable("idle".into()))
        }
        fn ready(&self) -> bool {
            true
        }
        fn kind_status(&self) -> status::KindStatus {
            status::KindStatus::ready(serde_json::Value::Null)
        }
        async fn new_identity(&self) {}
        async fn shutdown(&self) {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
        fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
            self
        }
    }

    fn data_dir() -> &'static std::path::Path {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let dir = DIR.get_or_init(|| tempfile::tempdir().expect("test data dir")).path();
        vector_core::db::set_app_data_dir(dir.to_path_buf());
        dir
    }

    /// A fresh account, current and open.
    fn account() -> String {
        use nostr_sdk::prelude::ToBech32;
        let root = data_dir();
        let npub = nostr_sdk::prelude::Keys::generate().public_key().to_bech32().unwrap();
        std::fs::create_dir_all(root.join(&npub)).unwrap();
        vector_core::db::set_current_account(npub.clone()).unwrap();
        vector_core::db::init_database(&npub).unwrap();
        npub
    }

    /// The welcome screen with no account picked.
    fn no_account() {
        data_dir();
        vector_core::db::close_database();
        vector_core::db::clear_current_account_in_memory();
    }

    /// A loopback port nothing listens on: an I2P router that is not running.
    #[cfg(feature = "i2p")]
    fn dead_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    async fn clean() {
        host::deactivate(true).await;
        prelogin::disarm();
        set_account_booted(false);
        let _ = prelogin::set_marker(None);
        transport::set_preference(Some(Kind::Clearnet));
    }

    /// Samples the egress decision on another thread until stopped; counts every Direct seen while
    /// the preference read just before it was not Clearnet.
    #[cfg(feature = "i2p")]
    struct Watch {
        stop: Arc<AtomicBool>,
        leaks: Arc<AtomicUsize>,
        samples: Arc<AtomicUsize>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(feature = "i2p")]
    impl Watch {
        fn start(owner: u64) -> Self {
            let (stop, leaks, samples) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let (s, l, n) = (stop.clone(), leaks.clone(), samples.clone());
            let thread = std::thread::spawn(move || {
                while !s.load(Ordering::Acquire) {
                    let before = transport::preference();
                    let e = transport::egress(owner, Lane::Account, "nos.lol", 443);
                    if before != Some(Kind::Clearnet) && e == Egress::Direct {
                        l.fetch_add(1, Ordering::SeqCst);
                    }
                    n.fetch_add(1, Ordering::Relaxed);
                }
            });
            Watch { stop, leaks, samples, thread: Some(thread) }
        }

        fn finish(mut self) -> (usize, usize) {
            self.stop.store(true, Ordering::Release);
            let _ = self.thread.take().map(|t| t.join());
            (self.leaks.load(Ordering::SeqCst), self.samples.load(Ordering::SeqCst))
        }
    }

    #[cfg(feature = "i2p")]
    async fn until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(feature = "i2p")]
    fn active_i2p_port() -> Option<u16> {
        vector_core::i2p::active().map(|t| t.sam_port())
    }

    /// Switching never opens a direct socket: into I2P the old network stops after the new
    /// choice already refuses; out to Clearnet the instance is gone before anything goes direct.
    #[cfg(feature = "i2p")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn switching_never_goes_direct() {
        let _serial = TEST_LOCK.lock().await;
        let _acct = account();
        clean().await;
        let owner = vector_core::db::live_session_id();
        let port = dead_port();
        i2p_config::set_router(port as i64, Some(None)).unwrap();

        // Clearnet → I2P with no router: chosen, saved, installed, and blocked with the reason.
        let watch = Watch::start(owner);
        let view = set_kind("i2p".into(), Some(true)).await.expect("I2P on");
        let (leaks, samples) = watch.finish();
        assert_eq!(leaks, 0, "a Direct egress while I2P was chosen ({samples} samples)");
        assert_eq!(view.kind, "i2p");
        assert!(view.blocked);
        assert_eq!(transport::prefs::stored_kind(), transport::prefs::StoredKind::Kind(Kind::I2p));
        let a = host::active().expect("installed");
        assert_eq!((a.kind, a.owner(), a.started_prelogin()), (Kind::I2p, owner, false));
        assert_eq!(active_i2p_port(), Some(port));
        until("router_unreachable", || status::reason_for(Kind::I2p).is_some_and(|r| r.code == "router_unreachable")).await;
        let text = format!("Can't reach your I2P router at 127.0.0.1:{port}.");
        for host in ["nos.lol", "nostrajmjieip3dqgeefsgpydy3bbshe3o32z65dwkssl7qxkn5a.b32.i2p", "1.2.3.4"] {
            assert_eq!(transport::egress(owner, Lane::Account, host, 443), Egress::Refuse(ConnectError::Blocked(text.clone())), "{host}");
        }
        assert_eq!(transport::egress(owner, Lane::Shared, "x.onion", 443), Egress::Refuse(ConnectError::Refused(transport::Refusal::NotReachable { network: Kind::Tor })));

        // Same choice again: nothing restarts. Sign-in keeps the account's own instance (Android
        // resume), and its wait gives up on a silent router instead of holding sign-in.
        let id = a.id();
        assert_eq!(set_kind("i2p".into(), None).await.unwrap().kind, "i2p");
        let t0 = std::time::Instant::now();
        sync_to_active_account().await.unwrap();
        assert!(t0.elapsed() < ROUTER_GRACE + Duration::from_secs(5), "waited {:?}", t0.elapsed());
        assert_eq!(host::active().map(|a| a.id()), Some(id), "kept across sync");

        // Retry wakes the keeper, a new identity needs no restart; with nothing running it starts again.
        retry(false).await.unwrap();
        retry(true).await.unwrap();
        assert_eq!(host::active().map(|a| a.id()), Some(id));
        host::deactivate(true).await;
        retry(false).await.unwrap();
        assert!(host::active().is_some_and(|a| a.kind == Kind::I2p && a.id() != id), "a failed start is started again");

        // Another router: the running instance moves onto it; keeping the credentials keeps it.
        let id = host::active().unwrap().id();
        let port2 = dead_port();
        set_i2p_router(port2 as i64, None).await.unwrap();
        assert_ne!(host::active().map(|a| a.id()), Some(id));
        assert_eq!(active_i2p_port(), Some(port2));
        let id = host::active().unwrap().id();
        set_i2p_router(port2 as i64, None).await.unwrap();
        assert_eq!(host::active().map(|a| a.id()), Some(id), "same router: no restart");

        // Tor → I2P: Tor stops only after I2P is already the choice; nothing in between is direct.
        host::deactivate(true).await;
        transport::set_preference(Some(Kind::Tor));
        let tor = Idle::new(Kind::Tor);
        host::activate(tor.clone(), owner, false).unwrap();
        assert!(matches!(transport::egress(owner, Lane::Account, "nos.lol", 443), Egress::Proxy(_)));
        let watch = Watch::start(owner);
        set_kind("i2p".into(), Some(true)).await.unwrap();
        let (leaks, samples) = watch.finish();
        assert_eq!(leaks, 0, "a Direct egress during Tor → I2P ({samples} samples)");
        assert_eq!(tor.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(host::active().map(|a| a.kind), Some(Kind::I2p));

        // I2P → Clearnet: saved first, the instance gone before the first direct decision.
        let stop = Arc::new(AtomicBool::new(false));
        let early = Arc::new(AtomicUsize::new(0));
        let (s, e) = (stop.clone(), early.clone());
        let watcher = std::thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                if transport::egress(owner, Lane::Account, "nos.lol", 443) == Egress::Direct && host::active().is_some() {
                    e.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        let view = set_kind("clearnet".into(), Some(true)).await.unwrap();
        stop.store(true, Ordering::Release);
        watcher.join().unwrap();
        assert_eq!(early.load(Ordering::SeqCst), 0, "direct while an instance was still installed");
        assert_eq!(view.kind, "clearnet");
        assert!(host::active().is_none());
        assert_eq!(transport::egress(owner, Lane::Account, "nos.lol", 443), Egress::Direct);
        assert_eq!(transport::prefs::stored_kind(), transport::prefs::StoredKind::Kind(Kind::Clearnet));
        clean().await;
    }

    /// The welcome screen's choice: only raises, restarts only on a real change, never goes direct
    /// while a router is missing, and the instance it starts is the one the account keeps.
    #[cfg(feature = "i2p")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prelogin_choice_rules() {
        let _serial = TEST_LOCK.lock().await;
        no_account();
        clean().await;
        let owner = vector_core::db::live_session_id();
        let port = dead_port();
        let opts = serde_json::json!({ "sam_port": port });

        // Add Profile: armed without rewriting the install's choice; blocked, never direct.
        set_prelogin("i2p", Some(false), Some(&opts)).await.unwrap();
        assert_eq!(prelogin::marker().unwrap(), None);
        assert_eq!(prelogin::armed(), Some(PreloginChoice::with_opts(Kind::I2p, opts.clone())));
        assert_eq!(transport::preference(), Some(Kind::I2p));
        let a = host::active().expect("a pre-login instance");
        assert_eq!((a.kind, a.owner(), a.started_prelogin()), (Kind::I2p, owner, true));
        assert_eq!(active_i2p_port(), Some(port));
        assert_eq!(crate::commands::i2p::i2p_get_config().sam_port, port, "the login screen shows the router it picked");
        assert!(matches!(transport::egress(owner, Lane::Account, "nos.lol", 443), Egress::Refuse(_)));

        // Re-applied on every show without options: no restart, no epoch move.
        let (id, epoch) = (a.id(), transport::epoch());
        set_prelogin("i2p", Some(false), None).await.unwrap();
        assert_eq!(host::active().map(|a| a.id()), Some(id));
        assert_eq!(transport::epoch(), epoch);
        assert_eq!(prelogin::armed().unwrap().opts, opts, "options left out keep the ones picked");

        // Another port restarts it; remembering writes only the port.
        let port2 = dead_port();
        let opts2 = serde_json::json!({ "sam_port": port2, "sam_user": "vec", "sam_password": "never-stored", "exit": "off" });
        set_prelogin("i2p", None, Some(&opts2)).await.unwrap();
        assert_ne!(host::active().map(|a| a.id()), Some(id));
        assert_eq!(active_i2p_port(), Some(port2));
        assert_eq!(
            prelogin::marker().unwrap(),
            Some(PreloginChoice::with_opts(Kind::I2p, serde_json::json!({ "sam_port": port2 }))),
            "credentials and the exit policy never reach the marker"
        );
        let marker_file = std::fs::read_to_string(vector_core::db::get_app_data_dir().unwrap().join("transport_prelogin")).unwrap();
        assert!(!marker_file.contains("never-stored") && !marker_file.contains("vec"), "{marker_file}");
        let armed = prelogin::armed().unwrap();
        let auth = armed.auth.as_ref().expect("the credentials stay in memory");
        assert_eq!((auth.user.as_str(), &*auth.password), ("vec", "never-stored"));
        assert_eq!(armed.opts, serde_json::json!({ "sam_port": port2 }), "credentials and the exit policy are never options");
        assert!(!format!("{armed:?}").contains("never-stored"), "never printed");
        let cfg = prelogin::config_for(Kind::I2p);
        let cfg = cfg.downcast_ref::<i2p_config::I2pConfig>().unwrap();
        assert_eq!((cfg.sam_user.as_deref(), cfg.sam_password.as_deref()), (Some("vec"), Some("never-stored")), "the pre-login start uses them");
        set_prelogin("i2p", None, None).await.unwrap();
        assert_eq!(prelogin::armed().unwrap().auth.map(|a| a.user), Some("vec".into()), "a re-show keeps them for the same router");
        assert_eq!(
            set_prelogin("i2p", None, Some(&serde_json::json!({ "sam_user": "vec" }))).await.unwrap_err(),
            "Enter both a username and a password."
        );
        set_prelogin("i2p", None, Some(&serde_json::json!({ "sam_port": port2, "sam_user": null, "sam_password": null }))).await.unwrap();
        assert!(prelogin::armed().unwrap().auth.is_none(), "null clears them");

        // Bad input changes nothing.
        let before = host::active().map(|a| a.id());
        assert_eq!(set_prelogin("i2p", None, Some(&serde_json::json!({ "sam_port": 0 }))).await.unwrap_err(), "Enter a port from 1 to 65535.");
        assert_eq!(set_prelogin("nym", None, None).await.unwrap_err(), "Unknown network: nym");
        assert_eq!(host::active().map(|a| a.id()), before);

        // Picking an account that chose Tor: the welcome screen never overrides it.
        let _tor_acct = account();
        transport::prefs::persist_kind(Kind::Tor).unwrap();
        set_prelogin("i2p", Some(false), Some(&opts)).await.unwrap();
        assert_eq!(transport::preference(), Some(Kind::Tor));
        assert!(host::active().is_none(), "the welcome screen's I2P stops; the account's Tor starts at unlock");
        assert!(matches!(transport::egress(vector_core::db::live_session_id(), Lane::Account, "nos.lol", 443), Egress::Refuse(_)));

        // An account with no choice of its own inherits it; the start already runs the port the
        // commit saves, so the instance survives sign-in.
        clean().await;
        no_account();
        set_prelogin("i2p", Some(false), Some(&opts)).await.unwrap();
        let id = host::active().unwrap().id();
        let _new_acct = account();
        assert_eq!(transport::preference(), Some(Kind::I2p), "hydrated raised to the armed choice");
        start_for_active_account().await.unwrap();
        assert_eq!(host::active().map(|a| a.id()), Some(id), "sign-up adopts the welcome screen's instance");
        prelogin::commit();
        assert_eq!(transport::prefs::stored_kind(), transport::prefs::StoredKind::Kind(Kind::I2p));
        assert_eq!(i2p_config::current().sam_port, port);
        start_for_active_account().await.unwrap();
        assert_eq!(host::active().map(|a| a.id()), Some(id), "and keeps it after the commit");

        // Clearnet: disarmed, stopped, direct, forgotten.
        clean().await;
        no_account();
        set_prelogin("i2p", None, Some(&opts)).await.unwrap();
        set_prelogin("clearnet", None, None).await.unwrap();
        assert!(prelogin::armed().is_none());
        assert!(host::active().is_none());
        assert_eq!(prelogin::marker().unwrap(), None);
        assert_eq!(transport::egress(vector_core::db::live_session_id(), Lane::Account, "nos.lol", 443), Egress::Direct);

        // Only before sign-in.
        set_account_booted(true);
        assert_eq!(set_prelogin("clearnet", None, None).await.unwrap_err(), "The network can only be changed here before signing in.");
        clean().await;
    }

    /// Leaving a network on the welcome screen takes effect at once, even while a start holds the
    /// lifecycle lock (a Tor bootstrap stalled on a censored network); the start that lands
    /// afterwards is taken down.
    #[tokio::test]
    async fn clearnet_on_the_welcome_screen_never_waits_on_a_stalled_start() {
        let _serial = TEST_LOCK.lock().await;
        clean().await;
        no_account();
        prelogin::arm(PreloginChoice::new(Kind::Tor));
        transport::set_preference(Some(Kind::Tor));
        let start_in_flight = LIFECYCLE.lock().await;
        let t0 = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(2), apply_prelogin(PreloginChoice::new(Kind::Clearnet)))
            .await
            .expect("returns without the lock")
            .unwrap();
        assert!(t0.elapsed() < Duration::from_secs(1));
        assert_eq!(transport::preference(), Some(Kind::Clearnet), "loosened at once");
        assert!(prelogin::armed().is_none());
        host::activate(Idle::new(Kind::Tor), vector_core::db::live_session_id(), true).unwrap();
        drop(start_in_flight);
        for _ in 0..100 {
            if host::active().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(host::active().is_none(), "a pre-login instance nobody wants is stopped");
        clean().await;
    }

    /// Picking another network on the welcome screen stops the start in flight before queueing
    /// behind it (a Tor bootstrap must not keep contacting the network once I2P is chosen), and a
    /// start that lands after the choice moved on is never installed, so the network in use keeps
    /// its sockets.
    #[cfg(feature = "i2p")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn another_pick_drops_the_start_in_flight() {
        let _serial = TEST_LOCK.lock().await;
        clean().await;
        no_account();
        prelogin::arm(PreloginChoice::new(Kind::Tor));
        transport::set_preference(Some(Kind::Tor));
        let wanted = prelogin::generation();
        let start_in_flight = LIFECYCLE.lock().await;
        let mut dropped = Box::pin(prelogin::superseded(wanted));
        assert!(futures_util::poll!(dropped.as_mut()).is_pending(), "still wanted");
        let opts = serde_json::json!({ "sam_port": dead_port() });
        let pick = tokio::spawn(apply_prelogin(prelogin_choice("i2p", Some(&opts)).unwrap()));
        tokio::time::timeout(Duration::from_secs(2), dropped).await.expect("the Tor start is told to stop at once");
        assert_ne!(prelogin::generation(), wanted);
        drop(start_in_flight);
        tokio::time::timeout(Duration::from_secs(10), pick).await.unwrap().unwrap().unwrap();
        assert_eq!(host::active().map(|a| (a.kind, a.started_prelogin())), Some((Kind::I2p, true)));
        // The same pick again never cancels its own start.
        let before = prelogin::generation();
        set_prelogin("i2p", Some(false), None).await.unwrap();
        assert_eq!(prelogin::generation(), before);

        // A start that lands once the welcome screen moved on, or once another network is chosen,
        // is shut down without touching the epoch.
        host::deactivate(true).await;
        let owner = vector_core::db::live_session_id();
        let epoch = transport::epoch();
        let stale = prelogin::generation().wrapping_sub(1);
        assert_eq!(start_kind(Kind::I2p, Some(stale), owner).await, Ok(false));
        transport::set_preference(Some(Kind::Clearnet));
        let epoch_clearnet = transport::epoch();
        assert_ne!(epoch, epoch_clearnet);
        assert_eq!(start_kind(Kind::I2p, None, owner).await, Ok(false), "not the network in use");
        assert!(host::active().is_none());
        assert_eq!(transport::epoch(), epoch_clearnet, "the direct sockets were never cut");
        clean().await;
    }

    /// Add Profile: the account being left lends its SAM login to a new account on its router,
    /// typed and in memory only; another router or a login of its own takes none.
    #[tokio::test]
    async fn add_profile_lends_the_sam_login_for_the_same_router() {
        let _serial = TEST_LOCK.lock().await;
        let _acct = account();
        clean().await;
        i2p_config::set_router(7700, Some(Some(("vec".into(), "s3cret".into())))).unwrap();
        let c = add_account_choice("i2p", None).unwrap();
        assert_eq!(c.opts, serde_json::json!({ "sam_port": 7700 }));
        let auth = c.auth.expect("lent");
        assert_eq!((auth.user.as_str(), &*auth.password), ("vec", "s3cret"));
        assert!(add_account_choice("i2p", Some(&serde_json::json!({ "sam_port": 7701 }))).unwrap().auth.is_none(), "another router");
        let own = add_account_choice("i2p", Some(&serde_json::json!({ "sam_user": "me", "sam_password": "pw" }))).unwrap();
        assert_eq!(own.auth.map(|a| a.user), Some("me".into()), "its own login wins");
        assert!(add_account_choice("clearnet", None).unwrap().auth.is_none());
        i2p_config::set_router(7656, Some(None)).unwrap();
        clean().await;
    }

    #[test]
    fn add_account_kind_rules() {
        use crate::account_manager::add_account_kind as pick;
        assert_eq!(pick(Some("i2p"), Some(true), Some(Kind::Tor)), "i2p", "kind wins");
        assert_eq!(pick(None, Some(true), None), "tor");
        assert_eq!(pick(None, Some(false), Some(Kind::Tor)), "clearnet");
        assert_eq!(pick(None, Some(false), Some(Kind::I2p)), "i2p", "not Tor keeps another remembered network");
        assert_eq!(pick(None, None, Some(Kind::I2p)), "i2p");
        assert_eq!(pick(None, None, None), "clearnet");
    }

    #[tokio::test]
    async fn aliases_and_lookups() {
        let _serial = TEST_LOCK.lock().await;
        let _acct = account();
        clean().await;
        let b32 = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst.b32.i2p";
        let e = set_alias("wss://Relay.Example.com/".into(), "i2p".into(), Some(format!(" {b32} "))).await.unwrap().unwrap();
        assert_eq!(e.host, "relay.example.com");
        assert_eq!(e.twins.get(&Kind::I2p).map(String::as_str), Some(b32));
        assert_eq!(transport_get_aliases(), vec![e]);
        assert_eq!(set_alias("relay.example.com".into(), "i2p".into(), Some("not-i2p.com".into())).await.unwrap_err(), "Enter a .b32.i2p address.");
        assert_eq!(set_alias("relay.example.com".into(), "nym".into(), None).await.unwrap_err(), "Unknown network: nym");
        assert_eq!(check_alias("relay.example.com".into()).await.unwrap_err(), "Connect to I2P to check this address.");
        assert_eq!(find_alias("wss://stranger.example".into()).await.unwrap_err(), "Vector looks up I2P addresses only for your own relays.");
        assert_eq!(set_alias("relay.example.com".into(), "i2p".into(), Some(String::new())).await.unwrap(), None, "an empty address removes it");
        assert!(transport_get_aliases().is_empty());

        let routes = transport_get_routes(vec!["wss://nos.lol".into(), format!("ws://{b32}"), "https://x.onion/".into()]);
        let classes: Vec<&str> = routes.iter().map(|r| r.class.as_str()).collect();
        assert_eq!(classes, ["direct", "refused", "refused"]);
        assert_eq!(routes[1].text, "This server is only reachable over I2P.");
        assert_eq!(transport_get_routes(vec!["wss://a.com".into(); 2000]).len(), MAX_ROUTES);
        clean().await;
    }

    /// The shapes the frontend reads: field names exactly as the contract lists them.
    #[tokio::test]
    async fn views_keep_their_shapes() {
        let _serial = TEST_LOCK.lock().await;
        let keys = |v: serde_json::Value| -> Vec<String> {
            let mut k: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
            k.sort();
            k
        };
        let sorted = |k: &[&str]| -> Vec<String> {
            let mut k: Vec<String> = k.iter().map(|s| s.to_string()).collect();
            k.sort();
            k
        };
        assert_eq!(
            keys(serde_json::to_value(view()).unwrap()),
            sorted(&[
                "kind", "label", "supported", "phase", "ready", "blocked", "progress", "reason", "retry_in", "since", "prelogin",
                "prelogin_armed", "realtime_allowed", "realtime_active", "call_active", "config_seq", "steps", "detail"
            ])
        );
        assert_eq!(
            keys(serde_json::to_value(transport::route_view("wss://nos.lol")).unwrap()),
            sorted(&["url", "host", "kind", "class", "via", "text", "last_error"])
        );
        let entry = AliasEntry {
            host: "nos.lol".into(),
            twins: [(Kind::I2p, "x.b32.i2p".to_string())].into_iter().collect(),
            source: AliasSource::User,
            check: Default::default(),
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(keys(v.clone()), sorted(&["host", "twins", "source", "check"]));
        assert_eq!(v["twins"], serde_json::json!({ "i2p": "x.b32.i2p" }));
        assert_eq!(v["check"], serde_json::json!({ "state": "unchecked", "at": null, "text": null }));

        let cfg = i2p_config::I2pConfig { sam_user: Some("u".into()), sam_password: Some("hunter2".into()), ..Default::default() };
        let v = serde_json::to_value(i2p_config::view_of(&cfg)).unwrap();
        assert_eq!(keys(v.clone()), sorted(&["sam_port", "sam_user", "sam_auth", "exit", "outproxies", "defaults", "customized"]));
        assert_eq!((v["sam_auth"].as_bool(), v["exit"].as_str()), (Some(true), Some("allow")));
        assert!(!v.to_string().contains("hunter2"), "the SAM password never leaves core");
        assert_eq!(
            keys(v["outproxies"][0].clone()),
            sorted(&["id", "name", "address", "port", "enabled", "builtin"])
        );
        assert_eq!(
            keys(serde_json::to_value(crate::commands::i2p::RouterTestView { ok: true, version: Some("3.3".into()), text: "Found a router. SAM 3.3.".into() }).unwrap()),
            sorted(&["ok", "version", "text"])
        );
        #[cfg(feature = "i2p")]
        assert_eq!(
            keys(serde_json::to_value(vector_core::i2p::probe::FindAlias { found: None, check: None, text: "x".into() }).unwrap()),
            sorted(&["found", "check", "text"])
        );
    }

    /// An account made on the welcome screen saves its network in the step that makes it durable:
    /// a crash or reload before its first sync must not reopen it on Clearnet.
    #[test]
    fn every_account_commit_saves_the_network_with_it() {
        let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/account.rs")).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        let commits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("commit_account_setup(") || l.contains("commit_bunker_account_setup(") || l.contains("commit_nip55_account_setup("))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(commits.len(), 6, "the account commits this test knows");
        for i in commits {
            let after = lines[i..(i + 32).min(lines.len())].join("\n");
            assert!(after.contains("transport::prelogin::commit()"), "no network commit after line {}", i + 1);
        }
    }

    /// Every transport, I2P and Tor command is registered, allowed and answered on Vector Web.
    #[test]
    fn every_network_command_has_its_acl_triad() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let lib = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let caps = std::fs::read_to_string(root.join("capabilities/default.json")).unwrap();
        let web = std::fs::read_to_string(root.join("../crates/vector-web/src/commands.rs")).unwrap();
        let registered: Vec<&str> = lib
            .lines()
            .filter_map(|l| {
                let l = l.trim().trim_end_matches(',');
                l.strip_prefix("commands::transport::")
                    .or_else(|| l.strip_prefix("commands::i2p::"))
                    .or_else(|| l.strip_prefix("commands::tor::"))
            })
            .filter(|c| c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
            .collect();
        let expected = [
            "transport_get_state", "transport_set", "transport_allow_realtime", "transport_set_prelogin", "transport_prelogin_abandon",
            "transport_retry", "transport_get_routes", "transport_get_aliases", "transport_set_alias", "transport_check_alias",
            "transport_find_alias", "i2p_get_config", "i2p_set_router", "i2p_test_router", "i2p_set_exit", "i2p_set_outproxies",
        ];
        for c in expected {
            assert!(registered.contains(&c), "{c} is not in invoke_handler");
        }
        for c in &registered {
            let h = c.replace('_', "-");
            assert!(caps.contains(&format!("\"allow-{h}\"")), "{c}: no allow-{h} in default.json");
            let toml = std::fs::read_to_string(root.join(format!("permissions/autogenerated/{c}.toml")))
                .unwrap_or_else(|_| panic!("{c}: no permission TOML"));
            for line in [
                format!("identifier = \"allow-{h}\""),
                format!("identifier = \"deny-{h}\""),
                format!("commands.allow = [\"{c}\"]"),
                format!("commands.deny = [\"{c}\"]"),
            ] {
                assert!(toml.contains(&line), "{c}: {line}");
            }
            if !c.starts_with("tor_") {
                assert!(web.contains(&format!("\"{c}\"")), "{c}: no Vector Web arm");
            }
        }
    }

    /// The Tor toggle's backend path against the live network: on, connected, off, direct.
    /// `VECTOR_TOR_LIVE=1 cargo test --lib commands::transport -- --ignored --nocapture`
    #[cfg(feature = "tor")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn transport_set_tor_live() {
        if std::env::var("VECTOR_TOR_LIVE").ok().as_deref() != Some("1") {
            return;
        }
        let _serial = TEST_LOCK.lock().await;
        let _acct = account();
        clean().await;
        assert_eq!(transport::state(), transport::TransportState::Clearnet);

        let t0 = std::time::Instant::now();
        let view = set_kind("tor".into(), Some(true)).await.expect("Tor on");
        println!("[tor-live] transport_set(tor): {:.2}s, phase {:?}", t0.elapsed().as_secs_f64(), view.phase);
        assert_eq!(transport::state(), transport::TransportState::Active { kind: Kind::Tor });
        assert_eq!(transport::prefs::stored_kind(), transport::prefs::StoredKind::Kind(Kind::Tor), "saved");
        let legacy = super::super::tor::tor_get_state();
        assert!(legacy.enabled && legacy.running && legacy.status == "connected", "the old Tor card reads it: {legacy:?}");

        let t0 = std::time::Instant::now();
        set_kind("clearnet".into(), Some(true)).await.expect("Tor off");
        println!("[tor-live] transport_set(clearnet): {:.2}s", t0.elapsed().as_secs_f64());
        assert_eq!(transport::state(), transport::TransportState::Clearnet);
        assert!(host::active().is_none());
        let legacy = super::super::tor::tor_get_state();
        assert!(!legacy.enabled && !legacy.running, "{legacy:?}");
        clean().await;
    }

    /// The switch against the client-only router: Clearnet → I2P → new address → Clearnet, with
    /// both sessions gone at the end.
    /// `VECTOR_I2P_LIVE=1 cargo test --lib commands::transport -- --ignored --nocapture`
    #[cfg(feature = "i2p")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn transport_set_i2p_live() {
        if std::env::var("VECTOR_I2P_LIVE").ok().as_deref() != Some("1") {
            return;
        }
        let _serial = TEST_LOCK.lock().await;
        let _acct = account();
        clean().await;
        let owner = vector_core::db::live_session_id();
        i2p_config::set_router(i2p_config::DEFAULT_SAM_PORT as i64, Some(None)).unwrap();

        let t0 = std::time::Instant::now();
        let view = set_kind("i2p".into(), Some(true)).await.expect("I2P on");
        println!("[i2p-live] transport_set(i2p) returned in {:.2}s, phase {:?}", t0.elapsed().as_secs_f64(), view.phase);
        wait_for_active_network().await;
        println!("[i2p-live] ready after {:.2}s", t0.elapsed().as_secs_f64());
        assert_eq!(transport::state(), transport::TransportState::Active { kind: Kind::I2p });
        assert!(matches!(transport::egress(owner, Lane::Account, "nos.lol", 443), Egress::Proxy(_)));
        assert_eq!(transport::route_view("wss://nos.lol").class, "exit");
        let probe = vector_core::i2p::active().unwrap().session_probe().expect("sessions up");

        let t0 = std::time::Instant::now();
        retry(true).await.expect("new address");
        wait_for_active_network().await;
        println!("[i2p-live] new address ready after {:.2}s", t0.elapsed().as_secs_f64());
        assert_eq!(transport::state(), transport::TransportState::Active { kind: Kind::I2p });
        let gone = probe.check().await;
        assert!(gone.iter().all(|l| matches!(l, vector_core::i2p::sam::Liveness::Gone)), "old sessions closed: {gone:?}");
        let probe = vector_core::i2p::active().unwrap().session_probe().expect("new sessions up");

        let t0 = std::time::Instant::now();
        set_kind("clearnet".into(), Some(true)).await.expect("I2P off");
        println!("[i2p-live] transport_set(clearnet): {:.2}s", t0.elapsed().as_secs_f64());
        assert_eq!(transport::egress(owner, Lane::Account, "nos.lol", 443), Egress::Direct);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let gone = probe.check().await;
        assert!(gone.iter().all(|l| matches!(l, vector_core::i2p::sam::Liveness::Gone)), "sessions closed: {gone:?}");
        clean().await;
    }

    #[tokio::test]
    async fn lifecycle_refuses_a_stale_caller() {
        let _serial = TEST_LOCK.lock().await;
        clean().await;
        let a = vector_core::db::live_session();
        vector_core::db::close_database();
        transport::set_preference(Some(Kind::Tor));
        let b = vector_core::db::live_session_id();
        let inst = host::activate(Idle::new(Kind::Tor), b, false).unwrap();

        for kind in ["clearnet", "tor"] {
            let r = vector_core::db::with_session(a.clone(), set_kind(kind.into(), Some(true))).await;
            assert_eq!(r.err().as_deref(), Some("The account changed. Try again."), "{kind}");
        }
        for r in [
            vector_core::db::with_session(a.clone(), retry(true)).await.err(),
            vector_core::db::with_session(a.clone(), set_i2p_router(7656, None)).await.err(),
            vector_core::db::with_session(a.clone(), set_i2p_exit(transport::ExitPolicy::Off)).await.err(),
            vector_core::db::with_session(a.clone(), set_alias("nos.lol".into(), "i2p".into(), None)).await.err(),
        ] {
            assert_eq!(r.as_deref(), Some("The account changed. Try again."));
        }
        // A start that follows an await never runs for whoever became live meanwhile.
        for kind in transport::supported().into_iter().filter(|k| *k != Kind::Clearnet) {
            assert_eq!(start_kind(kind, None, a.id()).await.err().as_deref(), Some("The account changed. Try again."), "{kind:?}");
        }
        let still = host::active().expect("B's instance stays");
        assert_eq!(still.id(), inst.id());
        assert_eq!(still.owner(), b);
        assert_eq!(transport::preference(), Some(Kind::Tor), "and B's network with it");
        assert!(vector_core::db::with_session(a, async { ensure_live() }).await.is_err());
        assert!(ensure_live().is_ok());
        clean().await;
    }
}
