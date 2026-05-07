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
/// account's data dir if its preference is on. Called from `switch_account`
/// after the new account's DB pool is up so `tor_data_dirs()` resolves
/// correctly. Failures fall through to a "Tor off" state — better than
/// crashing the switch.
pub async fn sync_to_active_account() -> Result<(), String> {
    #[cfg(feature = "tor")]
    {
        // Drop the previous account's TorClient + circuit cache.
        stop_if_running();

        // The cache was hydrated by `init_database` for the new account, so
        // `transport_state()` already reflects this account's preference.
        let want_on = matches!(
            vector_core::tor::transport_state(),
            vector_core::tor::TorTransportState::Active(_)
                | vector_core::tor::TorTransportState::RequiredButInactive
        );
        if want_on {
            let (state_dir, cache_dir) = tor_data_dirs()?;
            let bridges = read_saved_bridges();
            vector_core::tor::TorService::start(state_dir, cache_dir, &bridges).await?;
        }
        vector_core::net::rebuild_shared_http_client()?;
    }
    Ok(())
}

#[cfg(feature = "tor")]
fn tor_data_dirs() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    // Stash Arti's state + cache under the active account's data dir so
    // multiple accounts have independent Tor consensus caches when the user
    // switches between them.
    let app_dir = vector_core::db::get_app_data_dir()?;
    let account = vector_core::db::get_current_account()?;
    let base = app_dir.join("data").join(&account).join("tor");
    let state = base.join("state");
    let cache = base.join("cache");
    std::fs::create_dir_all(&state).map_err(|e| format!("create state dir: {e}"))?;
    std::fs::create_dir_all(&cache).map_err(|e| format!("create cache dir: {e}"))?;
    Ok((state, cache))
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
        None => "disabled".to_string(),
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
        // If Tor is currently running, reconfigure the live TorClient with
        // the new bridge list. Reconfigure reuses the same TorClient (and
        // its already-acquired state-dir lock) so it sidesteps the lock
        // contention from a stop+start cycle. Existing relay sockets get
        // cycled afterward so they pick up the new guard via the new bridge.
        if let Some(svc) = vector_core::tor::current() {
            let (state_dir, cache_dir) = tor_data_dirs()?;
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
    TorState {
        enabled,
        running,
        supported,
        status: current_status_string(),
        bootstrap_progress,
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
            if !vector_core::tor::is_active() {
                let (state_dir, cache_dir) = tor_data_dirs()?;
                let bridges = read_saved_bridges();
                vector_core::tor::TorService::start(state_dir, cache_dir, &bridges).await?;
            }
        } else {
            // OFF: stop the service first so transport_state stays in
            // RequiredButInactive (blackhole) during the transition, THEN
            // flip the preference to release into Disabled (direct).
            if let Some(svc) = vector_core::tor::current() {
                svc.stop();
            }
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
async fn switch_relay_transport(tor_enabled: bool) -> Result<(), String> {
    // ConnectionMode is re-exported from nostr-relay-pool via nostr-sdk::pool.
    use nostr_sdk::pool::ConnectionMode;
    use nostr_sdk::RelayUrl;

    let client = match crate::NOSTR_CLIENT.get() {
        Some(c) => c,
        None => return Ok(()), // Client hasn't been built yet (e.g. pre-login). No-op.
    };

    let new_mode = if tor_enabled {
        match vector_core::tor::socks_addr() {
            Some(addr) => ConnectionMode::proxy(addr),
            // Failsafe: if the user wanted Tor but the service is somehow
            // not up at this exact moment, route to the blackhole rather
            // than direct. A relay that fails to connect is recoverable;
            // a relay that leaks the user's IP is not.
            None => ConnectionMode::proxy(vector_core::tor::blackhole_proxy_addr()),
        }
    } else {
        ConnectionMode::Direct
    };

    // Snapshot the current pool — URL + its existing RelayOptions clone — then
    // mutate. We don't iterate-and-mutate concurrently because remove/add take
    // the pool's internal lock.
    let relays = client.relays().await;
    let snapshots: Vec<(RelayUrl, nostr_sdk::RelayOptions)> = relays
        .iter()
        .map(|(url, relay)| (url.clone(), relay.opts().clone()))
        .collect();

    log_info!("[Tor] cycling {} relay connection(s) onto new transport...", snapshots.len());

    for (url, opts) in snapshots {
        // Take down the existing socket + drop the relay registration.
        if let Err(e) = client.remove_relay(url.clone()).await {
            log_warn!("[Tor] remove_relay({}) failed: {} — continuing", url, e);
        }
        // Re-add with the same options modulo connection_mode.
        let new_opts = opts.connection_mode(new_mode.clone());
        if let Err(e) = client.pool().add_relay(url.clone(), new_opts).await {
            log_warn!("[Tor] re-add_relay({}) failed: {}", url, e);
        }
    }

    // Reconnect any relays the pool didn't auto-connect.
    client.connect().await;
    log_info!("[Tor] relay transport switch complete");
    Ok(())
}
