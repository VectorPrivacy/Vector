//! Tor (Arti) Tauri commands: Tor's own settings (bridges, circuits) and the legacy toggle,
//! which now rides the generic network switch in `commands::transport`.
//!
//! When the `tor` feature is OFF these commands report a "disabled" state and refuse to
//! enable: Vector wasn't compiled with Tor support.

use serde::Serialize;

use vector_core::transport::Kind;

use super::transport as lifecycle;

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
        log_warn!("[Tor] obfs4 bridges configured but obfs4proxy not found — auto-disabling bridges; falling back to direct Tor.");
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
    if !vector_core::tor::is_active() && !vector_core::tor::is_bootstrapping() && !lifecycle::running_prelogin() {
        let dst = cache.clone();
        let _ = tokio::task::spawn_blocking(move || vector_core::tor::seed_cache_from_prelogin(&dst)).await;
    }
    Ok((state, cache))
}

/// The welcome screen's Tor switch: remember the choice for this install and bring the
/// transport in line. Only while no account is signed in.
#[tauri::command]
pub async fn tor_set_prelogin(enabled: bool, remember: Option<bool>) -> Result<TorState, String> {
    if lifecycle::account_booted() {
        return Err("Tor can only be switched here before signing in.".to_string());
    }
    #[cfg(feature = "tor")]
    {
        use vector_core::transport::prelogin;
        let marker_is_tor = matches!(prelogin::marker(), Ok(Some(ref c)) if c.kind == Kind::Tor);
        let armed_is_tor = prelogin::armed().is_some_and(|c| c.kind == Kind::Tor);
        // Off turns off only Tor: a welcome screen set to another network keeps it.
        if enabled {
            lifecycle::set_prelogin(Kind::Tor.as_str(), remember, None).await?;
        } else if marker_is_tor || armed_is_tor {
            let remember = if marker_is_tor { remember } else { Some(false) };
            lifecycle::set_prelogin(Kind::Clearnet.as_str(), remember, None).await?;
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
#[tauri::command]
pub async fn tor_prelogin_abandon() {
    lifecycle::prelogin_abandon().await;
}

#[cfg(feature = "tor")]
fn current_status_string() -> String {
    use vector_core::tor::TorStatus;
    // While the instance is bootstrapping nothing is installed yet: the flag covers that gap so
    // the UI renders "bootstrapping" instead of falling back to "disabled".
    if vector_core::tor::is_bootstrapping() {
        return format!("bootstrapping {}%", vector_core::tor::bootstrap_progress());
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
/// reconfigure the service to pick up the new bridges. The call awaits bootstrap
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
        if lifecycle::running_prelogin() && vector_core::tor::is_active() {
            lifecycle::stop_prelogin_service().await;
            return tor_set_enabled(true).await.map(|_| tor_get_bridges());
        }
        // Reconfigure the live TorClient in place: it keeps its already-acquired state-dir lock,
        // which a stop+start would contend for. Relay sockets then cycle onto the new guard.
        if let Some(svc) = vector_core::tor::current() {
            let (state_dir, cache_dir) = tor_data_dirs().await?;
            let bridges = read_saved_bridges();
            svc.reconfigure_bridges(state_dir, cache_dir, &bridges).await?;
            lifecycle::switch_relay_transport().await;
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
pub async fn tor_get_circuits(force_new: Option<bool>) -> Result<CircuitOut, String> {
    let force_new = force_new.unwrap_or(false);
    #[cfg(feature = "tor")]
    {
        if force_new {
            // Every token rotates, then every relay socket reconnects onto a fresh circuit.
            vector_core::tor::rotate_circuits();
            lifecycle::switch_relay_transport().await;
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
#[tauri::command]
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
#[tauri::command]
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
            lifecycle::switch_relay_transport().await;
        }
    }
    Ok(tor_get_state())
}

/// Read the current Tor state. Cheap, safe to poll from the UI.
#[tauri::command]
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
#[tauri::command]
pub async fn tor_set_enabled(enabled: bool) -> Result<TorState, String> {
    #[cfg(feature = "tor")]
    {
        if enabled {
            lifecycle::transport_set("tor".into(), Some(true)).await?;
        } else if vector_core::transport::preference() == Some(Kind::Tor) {
            lifecycle::transport_set("clearnet".into(), Some(true)).await?;
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
