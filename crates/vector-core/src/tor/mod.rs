//! Tor (Arti) integration for Vector.
//!
//! Bootstraps an embedded Arti `TorClient` and runs a localhost SOCKS5 listener
//! on an OS-assigned ephemeral port. Consumers (reqwest HTTP clients, the
//! nostr-sdk relay pool) point at `tor::proxy_url()` to route their TCP
//! traffic through Tor — no protocol-specific glue needed.
//!
//! Iroh / QUIC stays direct: Tor is a TCP-only transport.
//!
//! # Lifecycle
//!
//! 1. `TorService::start(state_dir, cache_dir).await` — bootstraps Arti, opens
//!    the SOCKS listener, installs a global handle.
//! 2. `tor::proxy_url()` — `Some("socks5h://127.0.0.1:<port>")` while active,
//!    `None` when not.
//! 3. `TorService::stop()` — drops the listener, clears the global. Active
//!    connections die with the `TorClient` Arc reaching zero refs.
//!
//! # Why `socks5h`?
//!
//! `socks5h://` tells the client to send hostnames *to the SOCKS server* for
//! resolution, instead of doing local DNS first. We want all DNS through Tor
//! too, otherwise the user's nameserver sees every domain they visit.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::net::SocketAddr;
use futures_util::StreamExt;

use arti_client::{TorClient, TorClientConfig};
use arti_client::config::CfgPath;
use tor_rtcompat::PreferredRuntime;
use tokio::sync::oneshot;

mod socks;

/// Global slot for the active Tor service. `None` when Tor is disabled.
static TOR_SERVICE: OnceLock<Mutex<Option<Arc<TorService>>>> = OnceLock::new();

fn tor_slot() -> &'static Mutex<Option<Arc<TorService>>> {
    TOR_SERVICE.get_or_init(|| Mutex::new(None))
}

/// Set true for the duration of a `TorService::start()` call. Lets `is_active`
/// callers distinguish "bootstrap in progress" (the service hasn't been put
/// into the slot yet but isn't disabled either) from "off".
static TOR_BOOTSTRAPPING: AtomicBool = AtomicBool::new(false);

/// Latest bootstrap percentage (0..=100), updated live from Arti's
/// `bootstrap_events()` stream while `TOR_BOOTSTRAPPING` is true.
static TOR_BOOTSTRAP_PROGRESS: AtomicU8 = AtomicU8::new(0);

/// Last start/bootstrap failure. A failed `start()` puts nothing in the slot
/// and clears the bootstrapping flag, so without this the state is byte-identical
/// to "still spawning" — the UI would loop on "Starting…" with a locked toggle.
/// Cleared on a fresh start attempt and when Tor is turned off.
static TOR_LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// The last recorded start/bootstrap failure, if any (see [`TOR_LAST_ERROR`]).
pub fn last_bootstrap_error() -> Option<String> {
    TOR_LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Record a start/bootstrap failure so status surfaces can report "failed"
/// rather than an indistinguishable "starting".
pub fn set_last_bootstrap_error(msg: impl Into<String>) {
    *TOR_LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.into());
}

/// Clear the recorded failure (a new attempt is starting, or Tor was disabled).
pub fn clear_last_bootstrap_error() {
    *TOR_LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Returns true while `TorService::start()` is mid-execution. Useful for
/// status surfaces that would otherwise see `is_active() == false` and
/// mistakenly report "off" during the 5–15s bootstrap window.
pub fn is_bootstrapping() -> bool {
    TOR_BOOTSTRAPPING.load(Ordering::Acquire)
}

/// Latest bootstrap progress percentage (0..=100). Only meaningful while
/// `is_bootstrapping()` is true; held at 100 (or whatever the last reading
/// was) after `start()` returns.
pub fn bootstrap_progress() -> u8 {
    TOR_BOOTSTRAP_PROGRESS.load(Ordering::Acquire)
}

/// Bootstrap status surfaced to the UI for progress display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TorStatus {
    /// Service hasn't been started.
    Disabled,
    /// Bootstrap in progress (0..=100).
    Bootstrapping(u8),
    /// Connected to the Tor network and SOCKS listener accepting.
    Connected,
    /// Bootstrap or runtime error. The string is a user-facing summary.
    Failed(String),
}

/// An active Tor service: a bootstrapped Arti client + a localhost SOCKS5
/// listener that bridges incoming connections into the Tor network.
pub struct TorService {
    /// Arti's high-level client — owns circuits, the directory cache, etc.
    client: TorClient<PreferredRuntime>,
    /// Where the SOCKS5 listener is bound. `127.0.0.1:<ephemeral>`.
    socks_addr: SocketAddr,
    /// Drop signal for the SOCKS accept loop. `take()`-d on stop.
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// JoinHandle of the SOCKS accept loop. `take()`-d on stop_and_join() so
    /// callers awaiting it know the listener has fully exited and the file
    /// handles for `<account>/tor/state` and tor/cache can release before a
    /// caller (e.g. logout) wipes those directories.
    socks_join: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Latest bootstrap state.
    status: Mutex<TorStatus>,
}

impl TorService {
    /// Bootstrap Arti and start the SOCKS5 listener. Awaits full bootstrap
    /// before returning. `state_dir` and `cache_dir` are persisted across
    /// runs — caching the consensus directory dramatically speeds subsequent
    /// boots (~2s vs the 10–15s first-boot consensus fetch).
    ///
    /// `bridges`: optional vanilla-bridge lines like `"1.2.3.4:443 FINGER..."`.
    /// Each line is parsed individually; invalid lines are logged and skipped.
    /// If at least one valid bridge is supplied, Arti will use bridges
    /// instead of public guards. Pass an empty slice for normal direct Tor.
    ///
    /// Records any failure into [`TOR_LAST_ERROR`] so the toggle UI can show
    /// "failed" instead of looping on "Starting…"; clears it on a fresh attempt.
    pub async fn start(
        state_dir: PathBuf,
        cache_dir: PathBuf,
        bridges: &[String],
    ) -> Result<Arc<Self>, String> {
        clear_last_bootstrap_error();
        match Self::start_inner(state_dir, cache_dir, bridges).await {
            Ok(svc) => Ok(svc),
            Err(e) => {
                set_last_bootstrap_error(e.clone());
                Err(e)
            }
        }
    }

    async fn start_inner(
        state_dir: PathBuf,
        cache_dir: PathBuf,
        bridges: &[String],
    ) -> Result<Arc<Self>, String> {
        log_info!("[Tor] starting; state={} cache={} bridges={}", state_dir.display(), cache_dir.display(), bridges.len());
        TOR_BOOTSTRAPPING.store(true, Ordering::Release);
        TOR_BOOTSTRAP_PROGRESS.store(0, Ordering::Release);
        // RAII guard so the flag clears even if any of the `?` paths below errors.
        struct BootstrapGuard;
        impl Drop for BootstrapGuard {
            fn drop(&mut self) { TOR_BOOTSTRAPPING.store(false, Ordering::Release); }
        }
        let _guard = BootstrapGuard;

        let mut config_builder = TorClientConfig::builder();
        // Arti's config paths support shell-expansion templates (${HOME} etc).
        // We always pass concrete paths from Vector's data dir, so use the
        // _literal constructor and skip the expansion machinery.
        config_builder.storage().state_dir(CfgPath::new_literal(state_dir));
        config_builder.storage().cache_dir(CfgPath::new_literal(cache_dir));

        // Parse + apply bridges. Invalid lines are logged and skipped so a
        // single typo doesn't take down the whole connection. If zero lines
        // parse cleanly, we fall back to direct Tor (no bridges).
        let mut bridge_addrs: Vec<SocketAddr> = Vec::new();
        if !bridges.is_empty() {
            use tor_guardmgr::bridge::BridgeConfigBuilder;
            use tor_linkspec::HasAddrs;
            let mut valid_count = 0usize;
            for line in bridges {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                match trimmed.parse::<BridgeConfigBuilder>() {
                    Ok(builder) => {
                        if let Ok(built) = builder.build() {
                            for addr in built.addrs() {
                                bridge_addrs.push(addr);
                            }
                        }
                        config_builder.bridges().bridges().push(builder);
                        valid_count += 1;
                    }
                    Err(e) => log_warn!("[Tor] invalid bridge line, skipping: {} ({})", trimmed, e),
                }
            }
            if valid_count > 0 {
                log_info!("[Tor] using {} valid bridge(s)", valid_count);
            } else {
                log_warn!("[Tor] no valid bridges parsed, falling back to direct connection");
            }

            // If any bridge uses obfs4 AND we have at least one valid bridge,
            // register the obfs4proxy pluggable transport so arti can talk to
            // obfs4 bridges. The valid_count gate matters: if every line
            // failed to parse, we'd be falling back to direct anyway, so
            // surfacing an obfs4-missing error there is misleading.
            if valid_count > 0 && any_obfs4(bridges) {
                use arti_client::config::pt::TransportConfigBuilder;
                match resolve_obfs4proxy() {
                    Some(path) => {
                        log_info!("[Tor] obfs4 transport via {}", path.display());
                        let mut transport = TransportConfigBuilder::default();
                        transport
                            .protocols(vec![
                                "obfs4".parse().expect("obfs4 is a valid PT name"),
                            ])
                            .path(CfgPath::new_literal(path))
                            .run_on_startup(true);
                        config_builder.bridges().transports().push(transport);
                    }
                    None => {
                        return Err(obfs4proxy_missing_error());
                    }
                }
            }
        }
        // Stash the bridge addresses (or clear them) so the circuit display
        // can mark the guard accordingly.
        *bridge_addrs_slot().lock().unwrap_or_else(|e| e.into_inner()) = bridge_addrs;

        let config = config_builder
            .build()
            .map_err(|e| format!("Tor config build: {e}"))?;

        let runtime = PreferredRuntime::current()
            .map_err(|e| format!("Tor runtime acquire (need an active tokio runtime): {e}"))?;

        let client = TorClient::with_runtime(runtime)
            .config(config)
            // Generous local-resource timeout so a fresh start that immediately
            // follows a stop (e.g. tor_set_bridges restarting Tor with a new
            // config) gets time for the previous TorClient's Arc count to hit
            // zero and release the state-dir lock. Sync `create_unbootstrapped`
            // would default to 0ms and fail-fast; the async version with a
            // bumped timeout retries cleanly.
            .local_resource_timeout(std::time::Duration::from_secs(3))
            .create_unbootstrapped_async()
            .await
            .map_err(|e| format!("Tor client create: {e}"))?;

        let status = Mutex::new(TorStatus::Bootstrapping(0));

        // Subscribe to Arti's bootstrap event stream BEFORE calling bootstrap()
        // so we don't miss early progress. Each `BootstrapStatus` carries an
        // `as_frac()` 0.0..=1.0 — we map to a percent and stash in the global
        // atomic that `bootstrap_progress()` reads. UI polls and reflects
        // the value as a radial progress bar via the comet dasharray.
        let bootstrap_events = client.bootstrap_events();
        let log_progress = !bridges.is_empty();
        tokio::spawn(async move {
            let mut events = bootstrap_events;
            while let Some(status) = events.next().await {
                let pct = (status.as_frac() * 100.0).clamp(0.0, 100.0) as u8;
                TOR_BOOTSTRAP_PROGRESS.store(pct, Ordering::Release);
                // With bridges, the suspiciously-fast "complete" usually means
                // arti accepted cached non-bridge consensus and never actually
                // verified the bridge. Surface progress detail so the user
                // can see whether arti is doing real work.
                if log_progress {
                    log_info!("[Tor] bootstrap progress: {}% (blocked: {})",
                        pct,
                        status.blocked().map(|b| b.to_string()).unwrap_or_else(|| "no".into())
                    );
                }
                if status.ready_for_traffic() {
                    break;
                }
            }
        });

        log_info!("[Tor] bootstrapping...");
        client
            .bootstrap()
            .await
            .map_err(|e| format!("Tor bootstrap: {e}"))?;
        TOR_BOOTSTRAP_PROGRESS.store(100, Ordering::Release);
        log_info!("[Tor] bootstrap complete");
        if !bridges.is_empty() {
            log_info!("[Tor] bridges configured: if connections still fail, the bridge may be unreachable or its fingerprint may be missing the ed25519 ID. Get fresh bridges at bridges.torproject.org/bridges/en?transport=vanilla");
        }

        // Bind the SOCKS listener on localhost only — port 0 lets the kernel pick.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("SOCKS bind: {e}"))?;
        let socks_addr = listener
            .local_addr()
            .map_err(|e| format!("SOCKS local_addr: {e}"))?;
        log_info!("[Tor] SOCKS5 listener on {}", socks_addr);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let client_for_socks = client.clone();
        let socks_join = tokio::spawn(async move {
            socks::run(listener, client_for_socks, shutdown_rx).await;
            log_info!("[Tor] SOCKS5 listener stopped");
        });

        *status.lock().unwrap_or_else(|e| e.into_inner()) = TorStatus::Connected;

        let service = Arc::new(TorService {
            client,
            socks_addr,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            socks_join: Mutex::new(Some(socks_join)),
            status,
        });

        *tor_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&service));
        Ok(service)
    }

    /// Reconfigure the running TorClient with a new bridge list. Reuses the
    /// same TorClient instance (and its already-locked state dir), so this
    /// avoids the daemon-task / file-lock contention that a stop+start would
    /// hit. Pass an empty slice to remove bridges (return to direct Tor).
    ///
    /// After reconfigure, awaits a fresh bootstrap so the bridge's descriptor
    /// is fetched and the guard manager picks the bridge as the new guard;
    /// without this, circmgr can't build any new exit circuits and every
    /// subsequent SOCKS connect fails with "Failed to obtain exit circuit".
    pub async fn reconfigure_bridges(
        &self,
        state_dir: PathBuf,
        cache_dir: PathBuf,
        bridges: &[String],
    ) -> Result<(), String> {
        use arti_client::config::Reconfigure;
        use tor_guardmgr::bridge::BridgeConfigBuilder;
        use tor_linkspec::HasAddrs;

        let mut config_builder = TorClientConfig::builder();
        config_builder.storage().state_dir(CfgPath::new_literal(state_dir));
        config_builder.storage().cache_dir(CfgPath::new_literal(cache_dir));

        let mut bridge_addrs: Vec<SocketAddr> = Vec::new();
        let mut valid_count = 0usize;
        for line in bridges {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            match trimmed.parse::<BridgeConfigBuilder>() {
                Ok(builder) => {
                    if let Ok(built) = builder.build() {
                        for addr in built.addrs() {
                            bridge_addrs.push(addr);
                        }
                    }
                    config_builder.bridges().bridges().push(builder);
                    valid_count += 1;
                }
                Err(e) => log_warn!("[Tor] invalid bridge line, skipping: {} ({})", trimmed, e),
            }
        }
        if valid_count > 0 {
            log_info!("[Tor] reconfiguring with {} bridge(s)", valid_count);
        } else {
            log_info!("[Tor] reconfiguring without bridges (direct)");
        }

        // Register obfs4 transport if any bridge uses it (same logic as start()).
        // Gated on valid_count > 0 so a config of all-malformed-bridges falls
        // back to direct without surfacing a misleading obfs4 error.
        if valid_count > 0 && any_obfs4(bridges) {
            use arti_client::config::pt::TransportConfigBuilder;
            match resolve_obfs4proxy() {
                Some(path) => {
                    log_info!("[Tor] obfs4 transport via {}", path.display());
                    let mut transport = TransportConfigBuilder::default();
                    transport
                        .protocols(vec!["obfs4".parse().expect("obfs4 is a valid PT name")])
                        .path(CfgPath::new_literal(path))
                        .run_on_startup(true);
                    config_builder.bridges().transports().push(transport);
                }
                None => {
                    return Err(obfs4proxy_missing_error());
                }
            }
        }

        let new_config = config_builder
            .build()
            .map_err(|e| format!("Tor config build: {e}"))?;

        self.client
            .reconfigure(&new_config, Reconfigure::AllOrNothing)
            .map_err(|e| format!("Tor reconfigure: {e}"))?;

        // Update the cached bridge addresses so circuit-display "via bridge"
        // matching reflects the new state.
        *bridge_addrs_slot().lock().unwrap_or_else(|e| e.into_inner()) = bridge_addrs;

        // Force a fresh bootstrap so the bridge's router descriptor is
        // fetched and the guardmgr selects the bridge as the active guard.
        // arti's bootstrap is idempotent — if no work is needed it returns
        // immediately, so this is cheap on the no-bridge case.
        TOR_BOOTSTRAPPING.store(true, Ordering::Release);
        TOR_BOOTSTRAP_PROGRESS.store(0, Ordering::Release);
        struct BootstrapGuard;
        impl Drop for BootstrapGuard {
            fn drop(&mut self) { TOR_BOOTSTRAPPING.store(false, Ordering::Release); }
        }
        let _guard = BootstrapGuard;

        let bootstrap_events = self.client.bootstrap_events();
        tokio::spawn(async move {
            let mut events = bootstrap_events;
            while let Some(status) = events.next().await {
                let pct = (status.as_frac() * 100.0).clamp(0.0, 100.0) as u8;
                TOR_BOOTSTRAP_PROGRESS.store(pct, Ordering::Release);
                if status.ready_for_traffic() { break; }
            }
        });

        log_info!("[Tor] re-bootstrapping after bridge reconfigure...");
        self.client
            .bootstrap()
            .await
            .map_err(|e| format!("Tor re-bootstrap: {e}"))?;
        TOR_BOOTSTRAP_PROGRESS.store(100, Ordering::Release);
        log_info!("[Tor] re-bootstrap complete");

        // Rotate the global isolation token AFTER a successful bridge change.
        // Without this, the post-cycle relay sockets would re-connect through
        // SOCKS using the SAME token that matched pre-bridge circuits — and
        // arti's circmgr is allowed to hand back any cached circuit whose
        // isolation tag matches, potentially routing new traffic over a
        // pre-bridge guard. Rotating forces every new stream to a freshly
        // built circuit through whichever guard the new config selects.
        let new_token = tor_circmgr::isolation::IsolationToken::new();
        *isolation_slot().lock().unwrap_or_else(|e| e.into_inner()) = new_token;

        Ok(())
    }

    /// Stop the SOCKS listener and unregister from the global slot.
    /// In-flight Tor connections drop when the last `TorClient` ref goes away.
    /// Returns immediately; the accept-loop task and per-stream tasks finish
    /// asynchronously. For callers that need the underlying file handles
    /// released before continuing (e.g. logout's rm_dir_all on Windows), use
    /// `stop_and_join()` instead.
    pub fn stop(&self) {
        if let Some(tx) = self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
        *tor_slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
        log_info!("[Tor] stopped");
    }

    /// Stop the SOCKS listener and await the accept-loop task's exit before
    /// returning. The accept loop in `socks::run` aborts all in-flight
    /// per-stream tasks and joins them before exiting, so by the time the
    /// awaited JoinHandle resolves, every TorClient Arc held by SOCKS has
    /// been dropped and the state-dir lock is free for a subsequent start.
    pub async fn stop_and_join(&self) {
        if let Some(tx) = self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
        let join = self.socks_join.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(j) = join {
            let _ = j.await;
        }
        *tor_slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
        log_info!("[Tor] stopped (joined)");
    }

    /// SOCKS5 proxy URL suitable for `reqwest::Proxy::all`. `socks5h` so DNS
    /// resolution happens at the proxy (i.e. through Tor), not locally.
    pub fn proxy_url(&self) -> String {
        format!("socks5h://{}", self.socks_addr)
    }

    /// Raw SOCKS5 listener address — used by clients that take a `SocketAddr`
    /// directly instead of a URL (e.g. `nostr_sdk::ClientOptions::proxy`).
    pub fn socks_addr(&self) -> SocketAddr {
        self.socks_addr
    }

    /// Latest bootstrap / running state.
    pub fn status(&self) -> TorStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Returns the active Tor service if Tor is currently enabled.
pub fn current() -> Option<Arc<TorService>> {
    tor_slot().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Convenience: SOCKS5 proxy URL when Tor is on, `None` when off. This is the
/// hook every HTTP client builder consults.
pub fn proxy_url() -> Option<String> {
    current().map(|s| s.proxy_url())
}

/// Convenience: SOCKS5 listener address when Tor is on, `None` when off.
/// Used by code paths that take a `SocketAddr` directly (nostr-sdk's
/// `ClientOptions::proxy`).
pub fn socks_addr() -> Option<SocketAddr> {
    current().map(|s| s.socks_addr())
}

/// Convenience: is the embedded Tor service currently running?
pub fn is_active() -> bool {
    current().is_some()
}

/// What transport new TCP connections should use.
#[derive(Debug, Clone, Copy)]
pub enum TorTransportState {
    /// Tor is up. Route through the SOCKS proxy at this address.
    Active(SocketAddr),
    /// Tor is enabled in settings but not currently running (bootstrap in
    /// flight, mid-restart, or service crashed). Callers MUST treat this as
    /// "block all clearnet" — the failsafe guarantee is that traffic can
    /// never leak directly while Tor is the user's chosen transport.
    RequiredButInactive,
    /// Tor is disabled. Direct connections allowed.
    Disabled,
}

/// Hot-path cache of the `tor_enabled` user preference. Loaded from SQLite on
/// account init and updated on toggle.
static TOR_ENABLED_PREF: AtomicBool = AtomicBool::new(false);

/// Socket addresses of currently-active configured bridges. Populated by
/// `TorService::start` from the parsed bridge config; consumed by
/// `current_circuit_hops` to mark the Guard hop "via bridge" when its
/// address matches one of these.
static ACTIVE_BRIDGE_ADDRS: OnceLock<Mutex<Vec<SocketAddr>>> = OnceLock::new();

fn bridge_addrs_slot() -> &'static Mutex<Vec<SocketAddr>> {
    ACTIVE_BRIDGE_ADDRS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Snapshot of currently-active bridge socket addresses.
pub fn active_bridge_addrs() -> Vec<SocketAddr> {
    bridge_addrs_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Drop the active bridge socket addresses. Used by `reset_session()` so
/// stale bridge metadata from the prior account doesn't get reported as
/// "via bridge" by `current_circuit_hops` until the new account's Tor
/// service repopulates the slot on next start.
pub fn clear_active_bridge_addrs() {
    bridge_addrs_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Resolve the system path to `obfs4proxy`. Looks at `$PATH` first, then a
/// few common install locations on each desktop OS. Returns `None` if the
/// binary isn't installed; callers surface a clear error so the user can
/// `brew install obfs4` (mac), `apt install obfs4proxy` (linux), etc.
#[cfg(feature = "tor")]
pub fn resolve_obfs4proxy() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    // 1) PATH lookup
    if let Ok(paths) = std::env::var("PATH") {
        for p in std::env::split_paths(&paths) {
            for name in &["obfs4proxy", "lyrebird"] {
                let candidate = p.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    // 2) Common install locations
    let common: &[&str] = &[
        "/opt/homebrew/bin/obfs4proxy",
        "/usr/local/bin/obfs4proxy",
        "/usr/bin/obfs4proxy",
        "/opt/local/bin/obfs4proxy",
        // Newer Tor distributions use lyrebird (a maintained obfs4 fork)
        "/opt/homebrew/bin/lyrebird",
        "/usr/local/bin/lyrebird",
        "/usr/bin/lyrebird",
    ];
    for c in common {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Quick check: does any line in `bridges` use the obfs4 pluggable transport?
#[cfg(feature = "tor")]
fn any_obfs4(bridges: &[String]) -> bool {
    bridges.iter().any(|line| {
        line.trim().to_ascii_lowercase().starts_with("obfs4 ")
    })
}

/// User-facing error string for "obfs4 needed but obfs4proxy not found".
/// Returns only the install hint relevant to the current OS, instead of
/// dumping every platform's command in one wall of text.
#[cfg(feature = "tor")]
pub fn obfs4proxy_missing_error() -> String {
    let hint = if cfg!(target_os = "macos") {
        "Install via `brew install obfs4proxy`."
    } else if cfg!(target_os = "linux") {
        "Install via `apt install obfs4proxy` (or your distro's package manager)."
    } else if cfg!(target_os = "windows") {
        "Download `obfs4proxy.exe` from torproject.org and add it to PATH."
    } else if cfg!(target_os = "android") {
        "obfs4 isn't supported on Android yet."
    } else {
        "Install `obfs4proxy` for your platform and add it to PATH."
    };
    format!(
        "obfs4 bridges configured but `obfs4proxy` (or `lyrebird`) was not found. {}",
        hint
    )
}

fn tor_enabled_pref() -> bool {
    TOR_ENABLED_PREF.load(Ordering::Acquire)
}

/// Update the cache. Call after writing the `tor_enabled` SQLite setting.
pub fn set_tor_enabled_pref(enabled: bool) {
    TOR_ENABLED_PREF.store(enabled, Ordering::Release);
}

/// Hydrate the cache from the per-account DB. Idempotent.
pub fn init_tor_enabled_pref_from_db() {
    let enabled = matches!(
        crate::db::settings::get_sql_setting("tor_enabled".to_string()),
        Ok(Some(ref v)) if v == "1" || v == "true"
    );
    set_tor_enabled_pref(enabled);
}

/// Compute the transport state every TCP-bearing client (HTTP, nostr) should
/// honor. The failsafe — `RequiredButInactive` — exists because the user's
/// preference is an absolute guarantee: if Tor is enabled, NO traffic ever
/// goes direct, even during the bootstrap window or a Tor service crash.
pub fn transport_state() -> TorTransportState {
    match (tor_enabled_pref(), socks_addr()) {
        (true, Some(addr)) => TorTransportState::Active(addr),
        (true, None) => TorTransportState::RequiredButInactive,
        (false, _) => TorTransportState::Disabled,
    }
}

/// Sentinel SocketAddr to use as a "blackhole" proxy when Tor is required
/// but not active — connection attempts to it fail at the TCP layer
/// instantly, with no possibility of clearnet leak.
pub fn blackhole_proxy_addr() -> SocketAddr {
    // 127.0.0.1 port 1: reserved/no-listener on every sane OS.
    SocketAddr::from(([127, 0, 0, 1], 1))
}

/// The current isolation token applied to every SOCKS connection through
/// Vector's TorClient. Stays stable until `rotate_circuits()` is called, so
/// all of Vector's TCP traffic shares circuits matching this token. When the
/// user clicks "New circuit" we bump it; new sockets opened after that pick
/// up the new value and end up on a freshly-built circuit instead of the
/// previously-shared one.
#[cfg(feature = "tor")]
static CIRCUIT_ISOLATION: OnceLock<Mutex<tor_circmgr::isolation::IsolationToken>> = OnceLock::new();

#[cfg(feature = "tor")]
fn isolation_slot() -> &'static Mutex<tor_circmgr::isolation::IsolationToken> {
    CIRCUIT_ISOLATION.get_or_init(|| Mutex::new(tor_circmgr::isolation::IsolationToken::new()))
}

/// Generate a fresh isolation token. Subsequent SOCKS connections will pick
/// it up via `current_isolation_token()` and end up on a brand-new circuit,
/// partitioning their traffic from anything still running on the old one.
#[cfg(feature = "tor")]
pub fn rotate_circuits() {
    // IsolationToken is Copy, so a poisoned mutex carries no half-modified
    // state; recover the inner value rather than panicking the whole process.
    let mut guard = isolation_slot().lock().unwrap_or_else(|e| e.into_inner());
    *guard = tor_circmgr::isolation::IsolationToken::new();
}

/// Read the current isolation token. Used by the SOCKS handler to label
/// every outgoing connection with the active generation.
#[cfg(feature = "tor")]
pub fn current_isolation_token() -> tor_circmgr::isolation::IsolationToken {
    *isolation_slot().lock().unwrap_or_else(|e| e.into_inner())
}

/// One hop in a Tor circuit. Suitable for surfacing to the UI in a circuit
/// inspector. We deliberately keep this minimal — no nicknames or country
/// codes — to avoid pulling `experimental-api` / `geoip` features and the
/// binary weight that comes with them.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CircuitHop {
    /// "Guard" / "Middle" / "Exit". Derived from the position in the path.
    pub position: String,
    /// `<ip>:<port>` of the OR connection to the relay, if known.
    pub address: String,
    /// Base64 Ed25519 identity of the relay (the most stable per-relay
    /// identifier in the consensus). Empty for virtual hops.
    pub fingerprint: String,
    /// True when this hop's address matches one of the user's configured
    /// bridges. Only ever set on the Guard hop in practice.
    pub is_bridge: bool,
}

/// Return the current circuit's hop list. Awaits bootstrap if it isn't done
/// yet.
///
/// `force_new`: when true, rotates the global circuit isolation token before
/// querying — so the returned hops describe a brand-new circuit AND every
/// SOCKS connection opened after this point ends up on circuits matching
/// the new token. The caller is responsible for cycling existing sockets
/// (e.g. via the relay-transport switch) so old streams move onto the new
/// path.
///
/// Returns Err if Tor isn't running or no consensus is available yet.
#[cfg(feature = "tor")]
pub async fn current_circuit_hops(force_new: bool) -> Result<Vec<CircuitHop>, String> {
    use tor_circmgr::isolation::{IsolationToken, StreamIsolation};
    use tor_dirmgr::Timeliness;
    use tor_linkspec::{HasAddrs, HasRelayIds, RelayIdType};

    let svc = current().ok_or_else(|| "Tor not running".to_string())?;

    let netdir = svc
        .client
        .dirmgr()
        .netdir(Timeliness::Timely)
        .map_err(|e| format!("netdir unavailable: {e}"))?;

    // For force_new, build with a fresh local token. Only commit it to the
    // global slot AFTER the build succeeds — a failed/timed-out build with
    // the global already rotated would leave every subsequent SOCKS connect
    // unable to find a matching circuit.
    let token = if force_new {
        IsolationToken::new()
    } else {
        current_isolation_token()
    };
    let isolation = StreamIsolation::builder()
        .owner_token(token)
        .build()
        .map_err(|e| format!("isolation build: {e}"))?;

    let tunnel = svc
        .client
        .circmgr()
        .get_or_launch_exit(netdir.as_ref().into(), &[], isolation)
        .await
        .map_err(|e| format!("launch exit: {e}"))?;

    if force_new {
        *isolation_slot().lock().unwrap_or_else(|e| e.into_inner()) = token;
    }

    let path = tunnel
        .as_ref()
        .all_paths()
        .into_iter()
        .next()
        .ok_or_else(|| "tunnel has no path".to_string())?;

    let total = path.n_hops();
    let bridge_addrs = active_bridge_addrs();
    let hops = path
        .hops()
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let position = match i {
                0 => "Guard",
                n if n + 1 == total => "Exit",
                _ => "Middle",
            }
            .to_string();
            match entry.as_chan_target() {
                Some(ct) => {
                    // Match on the chan target's actual SocketAddr (not the
                    // String form) so port + IP comparison is exact.
                    let raw_addr = ct.addrs().next();
                    let address = raw_addr.map(|a| a.to_string()).unwrap_or_default();
                    let is_bridge = i == 0
                        && raw_addr
                            .map(|a| bridge_addrs.contains(&a))
                            .unwrap_or(false);
                    // RelayIdRef formats as "ed25519:<base64>"; strip the prefix.
                    let fingerprint = ct
                        .identity(RelayIdType::Ed25519)
                        .map(|id| {
                            let s = id.to_string();
                            s.strip_prefix("ed25519:")
                                .map(|p| p.to_string())
                                .unwrap_or(s)
                        })
                        .unwrap_or_default();
                    CircuitHop { position, address, fingerprint, is_bridge }
                }
                None => CircuitHop {
                    position,
                    address: "<virtual>".to_string(),
                    fingerprint: String::new(),
                    is_bridge: false,
                },
            }
        })
        .collect();

    Ok(hops)
}

#[cfg(not(feature = "tor"))]
pub async fn current_circuit_hops(_force_new: bool) -> Result<Vec<CircuitHop>, String> {
    Err("Vector was built without the `tor` feature.".to_string())
}
