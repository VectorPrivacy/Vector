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
/// returns so the frontend knows the new state is real before reflecting it
/// in the UI. Existing relay connections are NOT migrated — see the lifecycle
/// note in commands/account.rs / vector_core::nostr_client_options docs.
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
        // re-pick up the proxy state — same pattern used by the chat list
        // re-render hook in M2.
        vector_core::net::rebuild_shared_http_client()?;
    }

    #[cfg(not(feature = "tor"))]
    {
        if enabled {
            return Err("This Vector build was compiled without the `tor` feature. Re-build with `--features tor` to enable Tor support.".to_string());
        }
    }

    Ok(tor_get_state())
}
