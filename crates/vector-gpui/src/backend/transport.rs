//! The network lifecycle: one switch for every kind, in failsafe order, with Tor's and I2P's own
//! settings. The desktop client's `commands/transport.rs`, `tor.rs` and `i2p.rs`.
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
            let engine = prelogin::config_for(Kind::Tor)
                .downcast_ref::<transport::tor_config::TorConfig>()
                .cloned()
                .unwrap_or_default();
            if engine.external {
                return Ok(StartCtx {
                    owner,
                    started_prelogin: prelogin,
                    dirs: None,
                    config: std::sync::Arc::new(vector_core::tor::TorStartConfig { bridges: Vec::new(), engine }),
                });
            }
            let (dirs, bridges) = if prelogin {
                (vector_core::tor::prelogin_dirs()?, Vec::new())
            } else {
                (tor_data_dirs().await?, read_saved_bridges())
            };
            Ok(StartCtx {
                owner,
                started_prelogin: prelogin,
                dirs: Some(dirs),
                config: std::sync::Arc::new(vector_core::tor::TorStartConfig { bridges, engine }),
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
pub fn transport_get_state() -> TransportStateView {
    view()
}

/// Switch the live account's network. Tor awaits its bootstrap; other kinds return once started.
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
        vector_media::miniapps::apps().realtime.end_all_for_switch().await;
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
pub fn transport_allow_realtime() -> TransportStateView {
    transport::realtime::grant();
    view()
}

/// The welcome screen's network: only before sign-in. I2P takes `{ "sam_port": 7656 }`.
pub async fn transport_set_prelogin(kind: String, remember: Option<bool>, mut opts: Option<Value>) -> Result<TransportStateView, String> {
    let set = set_prelogin(&kind, remember, opts.as_ref()).await;
    if let Some(o) = opts.as_mut() {
        transport::prefs::scrub_json(o);
    }
    set?;
    Ok(view())
}

/// Leaving the welcome screen's choice behind (Add Profile backed out after its commit).
pub async fn transport_prelogin_abandon() {
    prelogin_abandon().await;
}

/// Try the chosen network again now. `newIdentity`: unlinkable from here on (I2P: new
/// addresses; Tor: new circuits).
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
pub fn transport_get_routes(urls: Vec<String>) -> Vec<RouteView> {
    urls.iter().take(MAX_ROUTES).map(|u| transport::route_view(u)).collect()
}

/// Every server the account gave an address on another network.
pub fn transport_get_aliases() -> Vec<AliasEntry> {
    aliases::table().entries()
}

/// Set (or with no address, remove) a server's address on `kind`. A new I2P address is checked
/// as soon as I2P is up.
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
    #[cfg(any(feature = "tor", feature = "i2p"))]
    if entry.is_some() {
        vector_core::transport::twins::check_pending();
    }
    emit_view();
    revive_relays();
    Ok(entry)
}

/// Check a server's address on the network in use now: a TLS handshake for its own name through it.
pub async fn transport_check_alias(host: String) -> Result<AliasEntry, String> {
    vector_core::db::scoped_result(check_alias(host)).await
}

async fn check_alias(host: String) -> Result<AliasEntry, String> {
    ensure_live()?;
    #[cfg(any(feature = "tor", feature = "i2p"))]
    {
        let session = vector_core::db::current_session();
        // spawn-detached: pinned to the caller's account by with_session and awaited; runs where the network's sockets live.
        let checked = transport::spawn_on(vector_core::db::with_session(session, async move {
            vector_core::transport::twins::check_alias(&host).await
        }))
        .await
        .map_err(|e| e.to_string())?;
        emit_view();
        checked
    }
    #[cfg(not(any(feature = "tor", feature = "i2p")))]
    {
        let _ = host;
        Err("Use Tor or I2P to check this address.".into())
    }
}

/// Ask one of the user's own relays for its address on the network in use (NIP-11), then check
/// it. Nothing is saved: the user decides.
pub async fn transport_find_alias(url: String) -> Result<Value, String> {
    vector_core::db::scoped_result(find_alias(url)).await
}

async fn find_alias(url: String) -> Result<Value, String> {
    ensure_live()?;
    if !super::network::is_own_relay(&url) {
        return Err("Vector looks these up only for your own relays.".into());
    }
    #[cfg(any(feature = "tor", feature = "i2p"))]
    {
        let session = vector_core::db::current_session();
        // spawn-detached: pinned to the caller's account by with_session and awaited; runs where the network's sockets live.
        let found = transport::spawn_on(vector_core::db::with_session(session, async move {
            vector_core::transport::twins::find_alias(&url).await
        }))
        .await
        .map_err(|e| e.to_string())??;
        serde_json::to_value(found).map_err(|e| e.to_string())
    }
    #[cfg(not(any(feature = "tor", feature = "i2p")))]
    {
        let _ = url;
        Err("Use Tor or I2P to check this address.".into())
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

/// Use Vector's own Tor or one already running here (`port` None finds it); a running Tor moves
/// onto the chosen engine.
#[cfg(feature = "tor")]
pub(crate) async fn set_tor_engine(external: bool, port: Option<u16>) -> Result<TransportStateView, String> {
    let _serial = LIFECYCLE.lock().await;
    ensure_live()?;
    transport::tor_config::set(external, port)?;
    let restarted = restart_if_incompatible(Kind::Tor).await;
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

// Tor's own settings.

use serde::Serialize;

/// State surfaced to the frontend for the Tor toggle UI.
#[derive(Debug, Serialize)]
pub struct TorState {
    /// Whether the live account's network is Tor. May not match `running` until bootstrap
    /// completes or fails.
    pub enabled: bool,
    /// Is Tor the installed, connected network?
    pub running: bool,
    /// Whether the Vector build was compiled with Tor support at all.
    pub supported: bool,
    /// Human-readable status. "disabled" / "bootstrapping NN%" / "connected" /
    /// "failed: <error>". Empty string when nothing meaningful to show.
    pub status: String,
    /// Bootstrap progress 0..=100. Live values from Arti's bootstrap_events()
    /// stream while bootstrap is running. 100 once running. The frontend
    /// drives the comet-trail radial progress bar from this.
    pub bootstrap_progress: u8,
    /// The welcome screen's Tor choice (install-wide), which the next account inherits.
    pub prelogin: bool,
    /// Each host on its own circuit (default), or one shared circuit.
    pub multi_circuit: bool,
    /// For display only: the bridge address, which needs credentials.
    pub socks_proxy: Option<String>,
}

/// Read the user's saved bridge lines (newline-separated). Returns an empty
/// Vec when bridges aren't enabled, or when the setting is missing/empty.
///
/// Resilience: if any saved line is an obfs4 line but `obfs4proxy` is no
/// longer installed (user uninstalled it between sessions), auto-flip the
/// `tor_bridges_enabled` SQLite flag to false and return an empty Vec.
/// That way Tor still starts (direct, no bridges) and the user isn't stuck
/// in a "Starting…" failsafe state on the next launch. The bridge
/// LINES are preserved so they can re-enable them after reinstalling.
#[cfg(feature = "tor")]
pub(crate) fn read_saved_bridges() -> Vec<String> {
    let enabled = matches!(
        vector_core::db::settings::get_sql_setting("tor_bridges_enabled".to_string()),
        Ok(Some(ref v)) if v == "1" || v == "true"
    );
    if !enabled {
        return Vec::new();
    }
    let lines: Vec<String> = match vector_core::db::settings::get_sql_setting("tor_bridges".to_string()) {
        Ok(Some(blob)) => blob
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    };

    let needs_obfs4 = lines.iter().any(|l| l.to_ascii_lowercase().starts_with("obfs4 "));
    if needs_obfs4 && vector_core::tor::resolve_obfs4proxy().is_none() {
        vector_core::log_warn!("[Tor] obfs4 bridges configured but obfs4proxy not found — auto-disabling bridges; falling back to direct Tor.");
        // Persist the disable so the bridges UI agrees on next read.
        let _ = vector_core::db::settings::set_sql_setting(
            "tor_bridges_enabled".to_string(),
            "0".to_string(),
        );
        return Vec::new();
    }

    lines
}

/// The account's own Tor directories, seeding an empty cache from the install-level one.
#[cfg(feature = "tor")]
pub(crate) async fn tor_data_dirs() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let account = vector_core::db::get_current_account()?;
    let base = vector_core::db::account_dir(&account)?.join("tor");
    let state = base.join("state");
    let cache = base.join("cache");
    std::fs::create_dir_all(&state).map_err(|e| format!("create state dir: {e}"))?;
    std::fs::create_dir_all(&cache).map_err(|e| format!("create cache dir: {e}"))?;
    // Never while a service may still be writing the install cache; the copy is MBs of disk I/O.
    if !vector_core::tor::is_active() && !vector_core::tor::is_bootstrapping() && !running_prelogin() {
        let dst = cache.clone();
        let _ = tokio::task::spawn_blocking(move || vector_core::tor::seed_cache_from_prelogin(&dst)).await;
    }
    Ok((state, cache))
}

/// The welcome screen's Tor switch: remember the choice for this install and bring the
/// transport in line. Only while no account is signed in.
pub async fn tor_set_prelogin(enabled: bool, remember: Option<bool>) -> Result<TorState, String> {
    if account_booted() {
        return Err("Tor can only be switched here before signing in.".to_string());
    }
    #[cfg(feature = "tor")]
    {
        use vector_core::transport::prelogin;
        let marker_is_tor = matches!(prelogin::marker(), Ok(Some(ref c)) if c.kind == Kind::Tor);
        let armed_is_tor = prelogin::armed().is_some_and(|c| c.kind == Kind::Tor);
        // Off turns off only Tor: a welcome screen set to another network keeps it.
        if enabled {
            set_prelogin(Kind::Tor.as_str(), remember, None).await?;
        } else if marker_is_tor || armed_is_tor {
            let remember = if marker_is_tor { remember } else { Some(false) };
            set_prelogin(Kind::Clearnet.as_str(), remember, None).await?;
        }
    }
    #[cfg(not(feature = "tor"))]
    {
        let _ = remember;
        if enabled {
            return Err("This Vector build was compiled without the `tor` feature.".to_string());
        }
    }
    Ok(tor_get_state())
}

/// Leaving Add Profile after its commit: the account being returned to keeps its own choice.
pub async fn tor_prelogin_abandon() {
    prelogin_abandon().await;
}

#[cfg(feature = "tor")]
fn current_status_string() -> String {
    use vector_core::tor::TorStatus;
    // While the instance is bootstrapping nothing is installed yet: the flag covers that gap so
    // the UI renders "bootstrapping" instead of falling back to "disabled".
    if vector_core::tor::is_bootstrapping() {
        return format!("bootstrapping {}%", vector_core::tor::bootstrap_progress());
    }
    if let Some(t) = vector_core::tor::external::active() {
        use vector_core::transport::Transport;
        return if t.ready() { "connected".into() } else { "waiting for Tor".into() };
    }
    match vector_core::tor::current().map(|s| s.status()) {
        // Nothing installed: never started, or the last start failed. Surface the failure so
        // the toggle unlocks instead of looping on "Starting…".
        None => match vector_core::tor::last_bootstrap_error() {
            Some(e) => format!("failed: {e}"),
            None => "disabled".to_string(),
        },
        Some(TorStatus::Disabled) => "disabled".to_string(),
        Some(TorStatus::Bootstrapping(p)) => format!("bootstrapping {p}%"),
        Some(TorStatus::Connected) => "connected".to_string(),
        Some(TorStatus::Failed(e)) => format!("failed: {e}"),
    }
}

#[cfg(not(feature = "tor"))]
fn current_status_string() -> String {
    "disabled".to_string()
}

/// One circuit hop, surfaced to the frontend's Advanced panel. Mirrors
/// `vector_core::tor::CircuitHop` so the bare (no-`tor`-feature) build can
/// still expose the type without dragging the whole arti dep tree along.
#[derive(Debug, Serialize)]
pub struct CircuitHopOut {
    pub position: String,
    pub address: String,
    pub fingerprint: String,
    pub is_bridge: bool,
}

/// Result of checking whether the obfs4 pluggable transport binary is
/// available on the system. Frontend uses this to show inline install
/// guidance when the user is configuring obfs4 bridges.
#[derive(Debug, Serialize)]
pub struct Obfs4ProxyStatus {
    /// `true` if `obfs4proxy` (or `lyrebird`) was found on this system.
    pub installed: bool,
    /// Resolved path when installed.
    pub path: Option<String>,
}

/// Detect whether `obfs4proxy` is installed and reachable. Cheap (no spawn,
/// just file-existence checks against PATH + common install dirs).
pub fn tor_check_obfs4_proxy() -> Obfs4ProxyStatus {
    #[cfg(feature = "tor")]
    {
        match vector_core::tor::resolve_obfs4proxy() {
            Some(p) => Obfs4ProxyStatus {
                installed: true,
                path: Some(p.to_string_lossy().to_string()),
            },
            None => Obfs4ProxyStatus {
                installed: false,
                path: None,
            },
        }
    }
    #[cfg(not(feature = "tor"))]
    {
        Obfs4ProxyStatus { installed: false, path: None }
    }
}

/// Bridge configuration surfaced to the frontend.
#[derive(Debug, Serialize)]
pub struct BridgesState {
    /// Has the user enabled bridges?
    pub enabled: bool,
    /// Saved bridge lines, newline-separated (textarea content).
    pub lines: String,
}

/// Read the user's bridge configuration. Cheap; safe to call freely.
pub fn tor_get_bridges() -> BridgesState {
    let enabled = matches!(
        vector_core::db::settings::get_sql_setting("tor_bridges_enabled".to_string()),
        Ok(Some(ref v)) if v == "1" || v == "true"
    );
    let lines = vector_core::db::settings::get_sql_setting("tor_bridges".to_string())
        .ok()
        .flatten()
        .unwrap_or_default();
    BridgesState { enabled, lines }
}

/// Persist a new bridge configuration and, if Tor is currently running,
/// reconfigure the service to pick up the new bridges. The call awaits bootstrap
/// before returning, just like `tor_set_enabled`. Lines are stored verbatim
/// (whitespace preserved per textarea); `read_saved_bridges` trims and skips
/// empty entries at start time.
pub async fn tor_set_bridges(enabled: bool, lines: String) -> Result<BridgesState, String> {
    // Pre-validate before mutating SQLite or the running TorClient — if we
    // wrote the persisted state first and then errored on reconfigure, the
    // saved-vs-runtime state would drift silently.
    #[cfg(feature = "tor")]
    if enabled {
        let any_obfs4 = lines
            .lines()
            .any(|l| l.trim().to_ascii_lowercase().starts_with("obfs4 "));
        if any_obfs4 && vector_core::tor::resolve_obfs4proxy().is_none() {
            return Err(vector_core::tor::obfs4proxy_missing_error());
        }
    }

    vector_core::db::settings::set_sql_setting(
        "tor_bridges_enabled".to_string(),
        if enabled { "1" } else { "0" }.to_string(),
    )?;
    vector_core::db::settings::set_sql_setting(
        "tor_bridges".to_string(),
        lines.clone(),
    )?;

    #[cfg(feature = "tor")]
    {
        // The welcome screen's service runs on install-level dirs: move to the account's own.
        if running_prelogin() && vector_core::tor::is_active() {
            stop_prelogin_service().await;
            return tor_set_enabled(true).await.map(|_| tor_get_bridges());
        }
        // Reconfigure the live TorClient in place: it keeps its already-acquired state-dir lock,
        // which a stop+start would contend for. Relay sockets then cycle onto the new guard.
        if let Some(svc) = vector_core::tor::current() {
            let (state_dir, cache_dir) = tor_data_dirs().await?;
            let bridges = read_saved_bridges();
            svc.reconfigure_bridges(state_dir, cache_dir, &bridges).await?;
            switch_relay_transport().await;
        }
    }

    Ok(tor_get_bridges())
}

/// Return the current circuit's hop list. Used by the Settings "Advanced"
/// panel to show a Tor Browser–style circuit display.
///
/// `force_new = false`: returns whatever circuit Vector's traffic is
/// currently on (near-instant; uses the active isolation token).
/// `force_new = true`: rotates the global isolation token, builds the
/// matching new circuit, then cycles every relay socket so existing
/// connections drop and re-establish through the new path. End result:
/// the displayed hops are exactly the path your relays + HTTP traffic
/// are now riding.
pub async fn tor_get_circuits(force_new: Option<bool>) -> Result<CircuitOut, String> {
    let force_new = force_new.unwrap_or(false);
    #[cfg(feature = "tor")]
    {
        if force_new {
            // Every token rotates, then every relay socket reconnects onto a fresh circuit.
            vector_core::tor::rotate_circuits();
            switch_relay_transport().await;
        }
        // A running Tor builds its own circuits: rotating is all Vector does with them.
        if vector_core::tor::external::active().is_some() {
            return Ok(CircuitOut { host: None, hops: Vec::new(), circuits: 0, hosts: 0 });
        }
        let (circuits, hosts) = vector_core::tor::active_circuits();
        // Multi-circuit has no one circuit to show: the count is the view, and it builds nothing.
        if vector_core::tor::multi_circuit() {
            return Ok(CircuitOut { host: None, hops: Vec::new(), circuits, hosts });
        }
        // Hard cap so the UI can't get stuck on "Building circuit…" if Arti
        // can't find an exit (consensus stale, all candidate exits down, etc.).
        let (host, hops) = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            vector_core::tor::current_circuit_hops(),
        )
        .await
        .map_err(|_| "Timed out building/reading circuit (20s)".to_string())??;
        Ok(CircuitOut { host, hops: hops.into_iter().map(hop_out).collect(), circuits, hosts })
    }
    #[cfg(not(feature = "tor"))]
    {
        let _ = force_new;
        Err("This Vector build was compiled without the `tor` feature.".to_string())
    }
}

/// The circuit view: in single-circuit mode its hops and the host that last rode it (None before
/// any traffic); in both modes how many circuits are open and how many hosts they serve.
#[derive(Debug, Serialize)]
pub struct CircuitOut {
    pub host: Option<String>,
    pub hops: Vec<CircuitHopOut>,
    pub circuits: usize,
    pub hosts: usize,
}

#[cfg(feature = "tor")]
fn hop_out(h: vector_core::tor::CircuitHop) -> CircuitHopOut {
    CircuitHopOut { position: h.position, address: h.address, fingerprint: h.fingerprint, is_bridge: h.is_bridge }
}

/// The circuit one relay or server is on, for its info dialog: present only while Tor carries
/// traffic to it and that circuit is still open.
pub fn tor_get_host_circuit(url: String) -> Result<Option<Vec<CircuitHopOut>>, String> {
    #[cfg(feature = "tor")]
    {
        if !vector_core::tor::is_active() {
            return Ok(None);
        }
        let Some(host) = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)) else {
            return Ok(None);
        };
        match vector_core::tor::host_circuit_hops(&host) {
            Some(hops) => Ok(Some(hops?.into_iter().map(hop_out).collect())),
            None => Ok(None),
        }
    }
    #[cfg(not(feature = "tor"))]
    {
        let _ = url;
        Ok(None)
    }
}

/// Multi-circuit (each host on its own circuit) or one shared circuit. Live streams move at once:
/// every token rotates and every relay socket reconnects.
pub async fn tor_set_multi_circuit(enabled: bool) -> Result<TorState, String> {
    vector_core::db::settings::set_sql_setting(
        "tor_multi_circuit".to_string(),
        if enabled { "1" } else { "0" }.to_string(),
    )?;
    #[cfg(feature = "tor")]
    {
        vector_core::tor::set_multi_circuit(enabled);
        vector_core::tor::rotate_circuits();
        if vector_core::tor::is_active() {
            switch_relay_transport().await;
        }
    }
    Ok(tor_get_state())
}

/// Which Tor engine the account uses, and where a running Tor was found.
#[derive(Debug, Serialize)]
pub struct TorEngineView {
    /// A Tor already running here rather than Vector's own.
    pub external: bool,
    /// The port the user fixed; `None` finds it.
    pub port: Option<u16>,
    /// Where a running Tor answers now, if anywhere.
    pub found: Option<u16>,
}

/// The engine setting, and a look for a running Tor on the ports it would use.
pub async fn tor_get_engine() -> TorEngineView {
    let cfg = vector_core::transport::tor_config::current();
    #[cfg(feature = "tor")]
    let found = match vector_core::tor::external::active().and_then(|t| t.port()) {
        Some(p) => Some(p),
        None => {
            let ports = cfg.ports();
            // spawn-detached: two loopback probes on the runtime that owns transport sockets; awaited.
            vector_core::transport::spawn_on(async move { vector_core::tor::external::detect(&ports).await }).await.ok().flatten()
        }
    };
    #[cfg(not(feature = "tor"))]
    let found = None;
    TorEngineView { external: cfg.external, port: cfg.port, found }
}

/// Use a Tor already running here (`port` 0 or absent finds it) or Vector's own.
pub async fn tor_set_engine(external: bool, port: Option<i64>) -> Result<TorEngineView, String> {
    let port = match port {
        None | Some(0) => None,
        Some(p) => Some(vector_core::transport::i2p_config::validate_port(p)?),
    };
    #[cfg(feature = "tor")]
    {
        vector_core::db::scoped_result(set_tor_engine(external, port)).await?;
        Ok(tor_get_engine().await)
    }
    #[cfg(not(feature = "tor"))]
    {
        let _ = (external, port);
        Err(vector_core::transport::status::not_in_build(Kind::Tor).text)
    }
}

/// Read the current Tor state. Cheap, safe to poll from the UI.
pub fn tor_get_state() -> TorState {
    let enabled = vector_core::transport::preference() == Some(Kind::Tor);
    let running = {
        #[cfg(feature = "tor")]
        { vector_core::tor::is_active() }
        #[cfg(not(feature = "tor"))]
        { false }
    };
    let supported = cfg!(feature = "tor");
    let bootstrap_progress = {
        #[cfg(feature = "tor")]
        { if running { 100 } else { vector_core::tor::bootstrap_progress() } }
        #[cfg(not(feature = "tor"))]
        { 0u8 }
    };
    let socks_proxy = {
        #[cfg(feature = "tor")]
        { vector_core::tor::proxy_url() }
        #[cfg(not(feature = "tor"))]
        { None }
    };
    let prelogin = {
        #[cfg(feature = "tor")]
        { vector_core::tor::prelogin_preference() }
        #[cfg(not(feature = "tor"))]
        { false }
    };
    let multi_circuit = {
        #[cfg(feature = "tor")]
        { vector_core::tor::multi_circuit() }
        #[cfg(not(feature = "tor"))]
        { true }
    };
    TorState {
        enabled,
        prelogin,
        multi_circuit,
        running,
        supported,
        status: current_status_string(),
        bootstrap_progress,
        socks_proxy,
    }
}

/// The legacy Tor toggle. On switches the account to Tor; off returns to Clearnet only when
/// Tor is the account's network, so it never turns another network off. Mini App sessions are
/// kept, as this toggle always did.
pub async fn tor_set_enabled(enabled: bool) -> Result<TorState, String> {
    #[cfg(feature = "tor")]
    {
        if enabled {
            transport_set("tor".into(), Some(true)).await?;
        } else if vector_core::transport::preference() == Some(Kind::Tor) {
            transport_set("clearnet".into(), Some(true)).await?;
        }
    }
    #[cfg(not(feature = "tor"))]
    {
        if enabled {
            return Err("This Vector build was compiled without the `tor` feature. Re-build with `--features tor` to enable Tor support.".to_string());
        }
    }
    Ok(tor_get_state())
}

// I2P's own settings.

use vector_core::transport::i2p_config::{I2pConfigView, OutproxyInput};
use vector_core::transport::ExitPolicy;

/// What the router test reports: a HELLO, no session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RouterTestView {
    pub ok: bool,
    pub version: Option<String>,
    pub text: String,
}

/// The I2P settings in effect: the account's, or on the welcome screen the router it picked. The
/// SAM password never leaves core.
pub fn i2p_get_config() -> I2pConfigView {
    let cfg = vector_core::transport::prelogin::config_for(vector_core::transport::Kind::I2p);
    i2p_config::view_of(&cfg.downcast_ref::<i2p_config::I2pConfig>().cloned().unwrap_or_default())
}

/// Save the router's port and SAM credentials (`None` keeps the saved ones, `Some(None)` clears
/// them); a running I2P moves onto that router.
pub async fn i2p_set_router(port: i64, auth: Option<Option<(String, String)>>) -> Result<TransportStateView, String> {
    vector_core::db::scoped_result(set_i2p_router(port, auth)).await
}

/// Ask whatever answers on `port` for a SAM HELLO. Without credentials given, the saved ones,
/// but only for the saved router's port: they never go to whatever else listens.
pub async fn i2p_test_router(port: i64, user: Option<String>, password: Option<String>) -> Result<RouterTestView, String> {
    let port = i2p_config::validate_port(port)?;
    let given = i2p_config::validate_sam_auth(user.as_deref(), password.as_deref())?;
    #[cfg(feature = "i2p")]
    {
        let auth = match given {
            Some((u, p)) => Some(vector_core::i2p::sam::SamAuth::new(&u, &zeroize::Zeroizing::new(p))),
            None => saved_auth_for(port),
        };
        // spawn-detached: one loopback HELLO on the runtime that owns router sockets; awaited.
        let r = vector_core::transport::spawn_on(async move { vector_core::i2p::sam::test_router(port, auth.as_ref()).await })
            .await
            .map_err(|e| e.to_string())?;
        Ok(RouterTestView { ok: r.ok, version: r.version, text: r.text })
    }
    #[cfg(not(feature = "i2p"))]
    {
        let _ = (port, given);
        Err(vector_core::transport::status::not_in_build(vector_core::transport::Kind::I2p).text)
    }
}

/// The saved SAM credentials, when `port` is the saved router's.
#[cfg(feature = "i2p")]
fn saved_auth_for(port: u16) -> Option<vector_core::i2p::sam::SamAuth> {
    let cfg = vector_core::transport::prelogin::config_for(vector_core::transport::Kind::I2p);
    let cfg = cfg.downcast_ref::<i2p_config::I2pConfig>()?;
    if cfg.sam_port != port {
        return None;
    }
    match (cfg.sam_user.as_deref(), cfg.sam_password.as_deref()) {
        (Some(u), Some(p)) => Some(vector_core::i2p::sam::SamAuth::new(u, p)),
        _ => None,
    }
}

/// I2P-Only: `"off"` reaches only I2P servers and the I2P addresses the user added; `"allow"`
/// reaches everything else through the outproxies.
pub async fn i2p_set_exit(mode: String) -> Result<TransportStateView, String> {
    let mode = match mode.as_str() {
        "allow" => ExitPolicy::Allow,
        "off" => ExitPolicy::Off,
        _ => return Err(format!("Unknown I2P-Only mode: {mode}")),
    };
    vector_core::db::scoped_result(set_i2p_exit(mode)).await
}

/// Save the outproxy list in the order given; `null` restores the built-in list.
pub async fn i2p_set_outproxies(list: Option<Vec<OutproxyInput>>) -> Result<I2pConfigView, String> {
    vector_core::db::scoped_result(set_i2p_outproxies(list)).await
}

