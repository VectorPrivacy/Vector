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
}

const SETTING_KEY: &str = "tor_enabled";

fn read_setting_enabled() -> bool {
    matches!(
        vector_core::db::settings::get_sql_setting(SETTING_KEY.to_string()),
        Ok(Some(ref v)) if v == "1" || v == "true"
    )
}

fn write_setting_enabled(enabled: bool) -> Result<(), String> {
    vector_core::db::settings::set_sql_setting(
        SETTING_KEY.to_string(),
        if enabled { "1" } else { "0" }.to_string(),
    )
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
        return "bootstrapping".to_string();
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
    TorState {
        enabled,
        running,
        supported,
        status: current_status_string(),
    }
}

/// Persist the user's Tor preference and start/stop the embedded service to
/// match. Bootstrap (~5–15s first boot, ~2s subsequent) is awaited before this
/// returns. Existing Nostr relay connections are forcefully torn down and
/// re-established through the new transport so the toggle is a "total switch"
/// rather than partial.
#[tauri::command]
pub async fn tor_set_enabled(enabled: bool) -> Result<TorState, String> {
    write_setting_enabled(enabled)?;

    #[cfg(feature = "tor")]
    {
        if enabled {
            // Start (idempotent: skip if already running).
            if !vector_core::tor::is_active() {
                let (state_dir, cache_dir) = tor_data_dirs()?;
                vector_core::tor::TorService::start(state_dir, cache_dir).await?;
            }
        } else {
            // Stop if running.
            if let Some(svc) = vector_core::tor::current() {
                svc.stop();
            }
        }
        // Whether or not Tor itself flipped, our pooled HTTP clients should
        // re-pick up the proxy state.
        vector_core::net::rebuild_shared_http_client()?;
        // Hard-cycle every Nostr relay connection so they pick up the new
        // transport. Without this, existing WSS sockets would stay on the
        // pre-toggle path until reconnect/restart.
        switch_relay_transport(enabled).await?;
    }

    #[cfg(not(feature = "tor"))]
    {
        if enabled {
            return Err("This Vector build was compiled without the `tor` feature. Re-build with `--features tor` to enable Tor support.".to_string());
        }
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
            // Tor service should be up by the time this runs; if not, fall
            // back to direct so we don't leave the pool in a broken state.
            None => ConnectionMode::Direct,
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
