//! Tor (Arti) Tauri commands.
//!
//! Wires the embedded Tor service to the frontend's Privacy settings toggle.
//!
//!   - `tor_get_state`: read current state (enabled flag + bootstrap status)
//!   - `tor_set_enabled`: persist the flag and start/stop the service
//!
//! When the `tor` feature is OFF, these commands all return a "disabled"
//! state and refuse to enable — Vector wasn't compiled with Tor support.

use serde::Serialize;

/// State surfaced to the frontend for the Tor toggle UI.
#[derive(Debug, Serialize)]
pub struct TorState {
    /// Was the user's preference to have Tor enabled? Persisted in settings.
    /// May not match `running` until bootstrap completes / fails.
    pub enabled: bool,
    /// Is the embedded service currently running and accepting SOCKS connections?
    pub running: bool,
    /// Whether the Vector build was compiled with Tor support at all
    /// (`--features tor`). Frontend uses this to grey out the toggle when off.
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
    /// `socks5h://127.0.0.1:<port>` while the service is up, else None.
    /// Lets JS-initiated plugin requests (updater) ride the same proxy —
    /// they use their own reqwest client, invisible to the backend failsafe.
    pub socks_proxy: Option<String>,
}

const SETTING_KEY: &str = "tor_enabled";

fn read_setting_enabled() -> bool {
    matches!(
        vector_core::db::settings::get_sql_setting(SETTING_KEY.to_string()),
        Ok(Some(ref v)) if v == "1" || v == "true"
    )
}

/// Read the user's saved bridge lines (newline-separated). Returns an empty
/// Vec when bridges aren't enabled, or when the setting is missing/empty.
///
/// Resilience: if any saved line is an obfs4 line but `obfs4proxy` is no
/// longer installed (user uninstalled it between sessions), auto-flip the
/// `tor_bridges_enabled` SQLite flag to false and return an empty Vec.
/// That way Tor still starts (direct, no bridges) and the user isn't stuck
/// in a "Starting…" failsafe-blackhole state on the next launch. The bridge
/// LINES are preserved so they can re-enable them after reinstalling.
#[cfg(feature = "tor")]
fn read_saved_bridges() -> Vec<String> {
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

    #[cfg(feature = "tor")]
    {
        let needs_obfs4 = lines.iter().any(|l| l.to_ascii_lowercase().starts_with("obfs4 "));
        if needs_obfs4 && vector_core::tor::resolve_obfs4proxy().is_none() {
            log_warn!("[Tor] obfs4 bridges configured but obfs4proxy not found — auto-disabling bridges; falling back to direct Tor.");
            // Persist the disable so the bridges UI agrees on next read.
            let _ = vector_core::db::settings::set_sql_setting(
                "tor_bridges_enabled".to_string(),
                "0".to_string(),
            );
            return Vec::new();
        }
    }

    lines
}

fn write_setting_enabled(enabled: bool) -> Result<(), String> {
    vector_core::db::settings::set_sql_setting(
        SETTING_KEY.to_string(),
        if enabled { "1" } else { "0" }.to_string(),
    )?;
    // Push the new value into vector-core's hot cache so the next outgoing
    // connection picks it up without a SQLite read.
    #[cfg(feature = "tor")]
    vector_core::tor::set_tor_enabled_pref(enabled);
    Ok(())
}

/// Stop any running Tor service. Safe across all builds; no-op if the `tor`
/// feature isn't compiled in. Returns immediately; the SOCKS task finishes
/// asynchronously. For flows that need file handles released before
/// continuing (logout's rm_dir_all), use `stop_and_join_if_running` instead.
#[allow(dead_code)] // bare build never reaches the gated callers
pub fn stop_if_running() {
    #[cfg(feature = "tor")]
    if let Some(svc) = vector_core::tor::current() {
        svc.stop();
    }
}

/// Stop any running Tor service and await the accept-loop task's exit + a
/// brief settle window for per-stream tasks. Use when the caller is about
/// to touch the on-disk state/cache directories (e.g. logout's rm_dir_all)
/// to avoid Windows sharing violations.
pub async fn stop_and_join_if_running() {
    #[cfg(feature = "tor")]
    if let Some(svc) = vector_core::tor::current() {
        svc.stop_and_join().await;
    }
}

/// Sync the Tor service to the active account's preference: stop any service
/// from a previous account, then start a fresh one bound to the current
/// account's data dir if its preference is on. A sign-in from the welcome screen
/// instead keeps that service for its first session. Called from `switch_account`
/// after the new account's DB pool is up so `tor_data_dirs()` resolves
/// correctly. Failures fall through to a "Tor off" state — better than
/// crashing the switch.
pub async fn sync_to_active_account() -> Result<(), String> {
    #[cfg(feature = "tor")]
    {
        let _serial = TOR_LIFECYCLE.lock().await;
        let want_on = !matches!(
            vector_core::tor::transport_state(),
            vector_core::tor::TorTransportState::Disabled
        );
        // Keep the welcome screen's service: it carried the sign-up, and a restart here drops
        // every relay mid-boot. The account's own dirs and guards take over from its next start.
        if running_prelogin() {
            if want_on {
                return vector_core::net::rebuild_shared_http_client();
            }
            stop_and_join_if_running().await;
        } else {
            // Drop the previous account's TorClient + circuit cache.
            stop_if_running();
        }

        // `init_database` hydrated the preference for the new account, so `want_on` is its.
        if want_on {
            // Fail closed before the bootstrap await: the previous account's
            // service is already stopped, so a rebuild here lands the shared
            // client on the blackhole instead of leaving the old (possibly
            // direct) client serving requests during bootstrap.
            vector_core::net::rebuild_shared_http_client()?;
            let (state_dir, cache_dir) = tor_data_dirs().await?;
            let bridges = read_saved_bridges();
            // Bounded like the toggle path — account switching must not hang
            // forever on a censored network. Timeout/error leaves the pref ON
            // with no service: blackholed, and the caller logs + continues.
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                vector_core::tor::TorService::start(state_dir, cache_dir, &bridges),
            )
            .await
            {
                Ok(Ok(_svc)) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    let msg = "Tor bootstrap timed out (120s) during account switch".to_string();
                    vector_core::tor::set_last_bootstrap_error(msg.clone());
                    return Err(msg);
                }
            }
        }
        vector_core::net::rebuild_shared_http_client()?;
    }
    Ok(())
}

#[cfg(feature = "tor")]
async fn tor_data_dirs() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let account = vector_core::db::get_current_account()?;
    let base = vector_core::db::account_dir(&account)?.join("tor");
    let state = base.join("state");
    let cache = base.join("cache");
    std::fs::create_dir_all(&state).map_err(|e| format!("create state dir: {e}"))?;
    std::fs::create_dir_all(&cache).map_err(|e| format!("create cache dir: {e}"))?;
    // Never while a service may still be writing the install cache; the copy is MBs of disk I/O.
    let ours = PRELOGIN_SVC.lock().unwrap_or_else(|e| e.into_inner()).upgrade().is_some();
    if !vector_core::tor::is_active() && !vector_core::tor::is_bootstrapping() && !ours {
        let dst = cache.clone();
        let _ = tokio::task::spawn_blocking(move || vector_core::tor::seed_cache_from_prelogin(&dst)).await;
    }
    Ok((state, cache))
}

/// One service start at a time across the welcome screen and the account paths: two
/// `TorService::start`s racing leave whichever finishes last in the slot.
#[cfg(feature = "tor")]
static TOR_LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// An account has booted in this session: the welcome screen's switch no longer applies.
static ACCOUNT_BOOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_account_booted(booted: bool) {
    ACCOUNT_BOOTED.store(booted, std::sync::atomic::Ordering::Release);
}

/// The service the welcome screen started, to tell it apart from an account's.
#[cfg(feature = "tor")]
static PRELOGIN_SVC: std::sync::Mutex<std::sync::Weak<vector_core::tor::TorService>> =
    std::sync::Mutex::new(std::sync::Weak::new());

/// Is the running service the welcome screen's (install-level dirs) rather than an account's?
#[allow(dead_code)] // bare build never reaches the gated callers
pub fn running_prelogin() -> bool {
    #[cfg(feature = "tor")]
    {
        let ours = PRELOGIN_SVC.lock().unwrap_or_else(|e| e.into_inner()).upgrade();
        matches!((vector_core::tor::current(), ours), (Some(a), Some(b)) if std::sync::Arc::ptr_eq(&a, &b))
    }
    #[cfg(not(feature = "tor"))]
    false
}

#[cfg(feature = "tor")]
async fn start_prelogin() -> Result<(), String> {
    // Read before queueing on the lock: a cancel issued while this waits must still count.
    let generation = vector_core::tor::prelogin_generation();
    let _serial = TOR_LIFECYCLE.lock().await;
    if vector_core::tor::is_active() || vector_core::tor::prelogin_generation() != generation {
        return Ok(());
    }
    let (state_dir, cache_dir) = vector_core::tor::prelogin_dirs()?;
    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        vector_core::tor::TorService::start(state_dir, cache_dir, &[]),
    )
    .await
    {
        Ok(Ok(svc)) => {
            // Turned off, abandoned or reset while it bootstrapped: nobody wants it now.
            if vector_core::tor::prelogin_generation() != generation {
                svc.stop_and_join().await;
                return Ok(());
            }
            *PRELOGIN_SVC.lock().unwrap_or_else(|e| e.into_inner()) = std::sync::Arc::downgrade(&svc);
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            let msg = "Tor bootstrap timed out (120s)".to_string();
            vector_core::tor::set_last_bootstrap_error(msg.clone());
            Err(msg)
        }
    }
}

/// Match the transport to the welcome screen's choice while no account is live. On: arm the
/// carry and fail closed before the bootstrap await. Off: stop before releasing to direct.
#[cfg(feature = "tor")]
pub async fn apply_prelogin(enabled: bool) -> Result<(), String> {
    vector_core::tor::arm_prelogin_carry(enabled);
    if enabled {
        vector_core::tor::set_tor_enabled_pref(true);
        vector_core::net::rebuild_shared_http_client()?;
        start_prelogin().await?;
    } else {
        vector_core::tor::cancel_prelogin_start();
        stop_and_join_if_running().await;
        vector_core::tor::clear_last_bootstrap_error();
        // An account already picked (its unlock screen's way back) keeps its own preference.
        if vector_core::db::get_current_account().is_ok() {
            vector_core::tor::init_tor_enabled_pref_from_db();
        } else {
            vector_core::tor::set_tor_enabled_pref(false);
        }
    }
    vector_core::net::rebuild_shared_http_client()
}

/// The welcome screen's Tor switch: remember the choice for this install and bring the
/// transport in line. Only while no account is signed in.
#[tauri::command]
pub async fn tor_set_prelogin(enabled: bool, remember: Option<bool>) -> Result<TorState, String> {
    if ACCOUNT_BOOTED.load(std::sync::atomic::Ordering::Acquire) {
        return Err("Tor can only be switched here before signing in.".to_string());
    }
    #[cfg(feature = "tor")]
    {
        // Add Profile applies its own choice without rewriting the install's remembered one.
        if remember != Some(false) {
            vector_core::tor::set_prelogin_preference(enabled)?;
        }
        apply_prelogin(enabled).await?;
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
#[tauri::command]
pub async fn tor_prelogin_abandon() {
    #[cfg(feature = "tor")]
    {
        vector_core::tor::arm_prelogin_carry(false);
        stop_prelogin_service().await;
    }
}

/// Stop the welcome screen's service, waiting out a start still in flight so it can't land
/// after an account's own service has taken over.
#[allow(dead_code)] // bare build never reaches the gated callers
pub async fn stop_prelogin_service() {
    #[cfg(feature = "tor")]
    {
        vector_core::tor::cancel_prelogin_start();
        let _serial = TOR_LIFECYCLE.lock().await;
        if running_prelogin() {
            stop_and_join_if_running().await;
        }
    }
}

#[cfg(feature = "tor")]
fn current_status_string() -> String {
    use vector_core::tor::TorStatus;
    // The slot is only populated after bootstrap finishes. While start() is
    // mid-flight, current() returns None — the bootstrap flag covers that gap
    // so the UI can render "bootstrapping" instead of falling back to "disabled".
    if vector_core::tor::is_bootstrapping() {
        return format!("bootstrapping {}%", vector_core::tor::bootstrap_progress());
    }
    match vector_core::tor::current().map(|s| s.status()) {
        // Empty slot means either "never started" (disabled) or "the last
        // start attempt failed". Surface the failure so the toggle unlocks
        // instead of looping on "Starting…" (enabled + !running is otherwise
        // indistinguishable from mid-spawn).
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
#[tauri::command]
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
#[tauri::command]
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
/// restart the service to pick up the new bridges. The call awaits bootstrap
/// before returning, just like `tor_set_enabled`. Lines are stored verbatim
/// (whitespace preserved per textarea); `read_saved_bridges` trims and skips
/// empty entries at start time.
#[tauri::command]
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
        if running_prelogin() {
            stop_prelogin_service().await;
            return tor_set_enabled(true).await.map(|_| tor_get_bridges());
        }
        // If Tor is currently running, reconfigure the live TorClient with
        // the new bridge list. Reconfigure reuses the same TorClient (and
        // its already-acquired state-dir lock) so it sidesteps the lock
        // contention from a stop+start cycle. Existing relay sockets get
        // cycled afterward so they pick up the new guard via the new bridge.
        if let Some(svc) = vector_core::tor::current() {
            let (state_dir, cache_dir) = tor_data_dirs().await?;
            let bridges = read_saved_bridges();
            svc.reconfigure_bridges(state_dir, cache_dir, &bridges).await?;
            vector_core::net::rebuild_shared_http_client()?;
            switch_relay_transport(true).await?;
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
#[tauri::command]
pub async fn tor_get_circuits(force_new: Option<bool>) -> Result<Vec<CircuitHopOut>, String> {
    let force_new = force_new.unwrap_or(false);
    #[cfg(feature = "tor")]
    {
        // Hard cap so the UI can't get stuck on "Building circuit…" if Arti
        // can't find an exit (consensus stale, all candidate exits down, etc.).
        let timeout = std::time::Duration::from_secs(20);
        let hops = tokio::time::timeout(
            timeout,
            vector_core::tor::current_circuit_hops(force_new),
        )
        .await
        .map_err(|_| "Timed out building/reading circuit (20s)".to_string())??;
        if force_new {
            // Force every existing relay socket to reconnect. New sockets
            // pick up the rotated isolation token via the SOCKS handler and
            // land on the freshly-built circuit.
            switch_relay_transport(true).await?;
        }
        Ok(hops
            .into_iter()
            .map(|h| CircuitHopOut {
                position: h.position,
                address: h.address,
                fingerprint: h.fingerprint,
                is_bridge: h.is_bridge,
            })
            .collect())
    }
    #[cfg(not(feature = "tor"))]
    {
        let _ = force_new;
        Err("This Vector build was compiled without the `tor` feature.".to_string())
    }
}

/// Read the current Tor state. Cheap, safe to poll from the UI.
#[tauri::command]
pub fn tor_get_state() -> TorState {
    let enabled = read_setting_enabled();
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
    TorState {
        enabled,
        prelogin,
        running,
        supported,
        status: current_status_string(),
        bootstrap_progress,
        socks_proxy,
    }
}

/// Persist the user's Tor preference and start/stop the embedded service to
/// match. Bootstrap (~5–15s first boot, ~2s subsequent) is awaited before this
/// returns. Existing Nostr relay connections are forcefully torn down and
/// re-established through the new transport so the toggle is a "total switch"
/// rather than partial.
///
/// Failsafe ordering: when going OFF, we stop the service BEFORE flipping the
/// preference cache to false. While the cache is still true and the service
/// is down, `transport_state()` returns `RequiredButInactive` → connections
/// blackhole. If we flipped the cache first instead, there'd be a microsecond
/// window where `transport_state()` reports `Disabled` (= direct) while the
/// proxy was still up, and any concurrent in-flight `build_http_client()`
/// would build a no-proxy client.
#[tauri::command]
pub async fn tor_set_enabled(enabled: bool) -> Result<TorState, String> {
    #[cfg(feature = "tor")]
    {
        if enabled {
            // ON: flip the preference first (cache flips → transport_state
            // returns RequiredButInactive while we boot Tor → blackhole), then
            // start the service. Once it's up, transport_state returns Active.
            write_setting_enabled(true)?;
            // Fail closed for the whole bootstrap window: rebuild the shared
            // HTTP client (lands on the blackhole) and cycle existing direct
            // relay sockets onto the blackhole proxy BEFORE the multi-second
            // bootstrap await. Otherwise pre-toggle clients/sockets keep
            // talking clearnet while the user believes Tor is on. The rebuild
            // result is deferred so a (near-impossible) builder failure can't
            // early-return with relays still flowing direct.
            let rebuild = vector_core::net::rebuild_shared_http_client();
            switch_relay_transport(true).await?;
            rebuild?;
            // Iroh (mini-app realtime) is deliberately left running: it's
            // QUIC/UDP (can't ride Tor) but relay-only, and the user consents
            // per session before launching a realtime app under Tor — killing
            // an active lobby on toggle would punish that informed choice.
            let _serial = TOR_LIFECYCLE.lock().await;
            if !vector_core::tor::is_active() {
                let (state_dir, cache_dir) = tor_data_dirs().await?;
                let bridges = read_saved_bridges();
                // Bounded: a censored/blackholed network can stall Arti's
                // consensus fetch indefinitely, and login awaits this command.
                // On timeout/error everything is already blackholed (pref ON,
                // no service) — never half-switched.
                match tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    vector_core::tor::TorService::start(state_dir, cache_dir, &bridges),
                )
                .await
                {
                    Ok(Ok(_svc)) => {}
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        // The timeout cancels start()'s future, so its own
                        // error-recording never runs — record it here so the
                        // toggle surfaces "failed" and unlocks.
                        let msg = "Tor bootstrap timed out (120s) — connections stay blocked until Tor connects or is disabled".to_string();
                        vector_core::tor::set_last_bootstrap_error(msg.clone());
                        return Err(msg);
                    }
                }
            }
        } else {
            // OFF: stop the service first so transport_state stays in
            // RequiredButInactive (blackhole) during the transition, THEN
            // flip the preference to release into Disabled (direct).
            // The adopted pre-login service is joined: its cache may seed the account's next.
            if running_prelogin() {
                stop_and_join_if_running().await;
            } else if let Some(svc) = vector_core::tor::current() {
                svc.stop();
            }
            // A prior failed attempt's error must not survive an off-toggle —
            // else the card would read "failed" while the pref is now off.
            vector_core::tor::clear_last_bootstrap_error();
            write_setting_enabled(false)?;
        }
        vector_core::net::rebuild_shared_http_client()?;
        switch_relay_transport(enabled).await?;
    }

    #[cfg(not(feature = "tor"))]
    {
        if enabled {
            return Err("This Vector build was compiled without the `tor` feature. Re-build with `--features tor` to enable Tor support.".to_string());
        }
        write_setting_enabled(enabled)?;
    }

    Ok(tor_get_state())
}

/// Tear down every Nostr relay in the active client's pool and re-add it with
/// its `connection_mode` set to match the desired Tor state. Preserves all
/// other RelayOptions (read/write/both, reconnect flags, retry config) so the
/// pool's behavior is otherwise unchanged.
///
/// Why we don't just rebuild the Client: NOSTR_CLIENT is a OnceLock that's
/// read from hundreds of places — replacing it would force a wide refactor.
/// Per-relay re-add achieves the same end (every socket is a fresh circuit
/// through the new transport) without touching the Client's identity.
#[cfg(feature = "tor")]
async fn switch_relay_transport(_tor_enabled: bool) -> Result<(), String> {
    let client = match crate::nostr_client() {
        Some(c) => c,
        None => return Ok(()), // Client hasn't been built yet (e.g. pre-login). No-op.
    };

    // The client's `Proxy` resolves Tor state at every connection attempt (see
    // `vector_core::apply_tor_proxy`), so a transport switch only needs the live
    // sockets torn down — no re-registration, and the blackhole failsafe lives in
    // that one closure instead of being duplicated here.
    //
    // `all()` deliberately: `relays()` is READ|WRITE-only, which would leave
    // GOSSIP-only community/discovery relays on their old transport — i.e. still
    // direct after the user switched Tor on.
    let relays = client.relays().all().await;
    log_info!("[Tor] cycling {} relay connection(s) onto new transport...", relays.len());

    // Reconnect via per-relay `try_connect`, never `client.connect()`: that marks every
    // relay Pending before checking whether its old connection task has finished
    // unwinding, and skips spawning a new one if it hasn't — wedging the relay in a
    // status nothing can recover from.
    let budget = vector_core::relay_connect_timeout(std::time::Duration::from_secs(15));
    for relay in relays.values() {
        relay.disconnect();
    }

    // Let the old connection tasks observe the terminate signal and exit. Reconnecting
    // while one is still running gets the fresh socket dropped on arrival.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    futures_util::future::join_all(relays.into_values().map(|relay| async move {
        let _ = relay.try_connect().timeout(budget).await;
    }))
    .await;

    log_info!("[Tor] relay transport switch complete");
    Ok(())
}
