//! Tor (Arti) integration for Vector: one transport kind on the shared meta-glue.
//!
//! `TorService::bootstrap` brings Arti up; the shell installs the instance in the transport
//! host, and every proxied connection reaches it through the process bridge (`connect.rs`).
//! Hostnames always reach Arti unresolved, so DNS goes through Tor too.
//!
//! Iroh / QUIC stays direct: Tor is a TCP-only transport.
//!
//! The free functions below the service (`transport_state`, the preference and pre-login
//! helpers) are the published API, kept as thin shims of `crate::transport`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::net::SocketAddr;
use futures_util::StreamExt;

use arti_client::{TorClient, TorClientConfig};
use arti_client::config::CfgPath;
use tor_rtcompat::PreferredRuntime;

use crate::transport::{Kind, TransportState};

mod connect;
pub use connect::{TorFactory, TorStartConfig};

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

/// A bootstrapped Arti client. The transport host owns the installed one.
pub struct TorService {
    /// Explicitly `Arc` since arti 2.4.0; clones are refcount handles, so the state-dir lock
    /// releases when the last one drops.
    client: Arc<TorClient<PreferredRuntime>>,
    status: Mutex<TorStatus>,
}

impl TorService {
    /// The arti client itself, for a test that weighs the bridge against arti on one client.
    #[doc(hidden)]
    pub fn arti(&self) -> Arc<TorClient<PreferredRuntime>> {
        self.client.clone()
    }

    /// Bootstrap Arti without installing it. Awaits full bootstrap. `state_dir` and `cache_dir`
    /// persist across runs, so later boots skip the consensus fetch (~2s against 10-15s).
    ///
    /// `bridges`: optional bridge lines like `"1.2.3.4:443 FINGER..."`. Invalid lines are logged
    /// and skipped; with at least one valid line Arti uses bridges instead of public guards.
    ///
    /// Records any failure into [`TOR_LAST_ERROR`] so the UI can show "failed" instead of
    /// looping on "Starting…"; clears it on a fresh attempt.
    pub async fn bootstrap(
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

    /// Bootstrap and install for the calling session (the SDK path). The app's own lifecycle
    /// bootstraps and installs separately.
    pub async fn start(
        state_dir: PathBuf,
        cache_dir: PathBuf,
        bridges: &[String],
    ) -> Result<Arc<Self>, String> {
        let svc = Self::bootstrap(state_dir, cache_dir, bridges).await?;
        crate::transport::host::activate(svc.clone(), crate::db::current_session_id(), false)?;
        Ok(svc)
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
        // spawn-detached: logging the Tor daemon's bootstrap progress; the daemon is process-wide.
        crate::rt::spawn(async move {
            let mut events = bootstrap_events;
            while let Some(status) = events.next().await {
                let pct = (status.as_frac() * 100.0).clamp(0.0, 100.0) as u8;
                if TOR_BOOTSTRAP_PROGRESS.swap(pct, Ordering::AcqRel) != pct {
                    crate::transport::host::notify(crate::transport::host::TransportEvent::Changed);
                }
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

        *status.lock().unwrap_or_else(|e| e.into_inner()) = TorStatus::Connected;
        Ok(Arc::new(TorService { client, status }))
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
        // spawn-detached: bootstrap progress again — same daemon, same reason.
        crate::rt::spawn(async move {
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
        rotate_circuits();

        Ok(())
    }

    fn installed(&self) -> bool {
        current().is_some_and(|c| std::ptr::eq(Arc::as_ptr(&c), self))
    }

    /// Uninstall this service if it is the installed one; its connections are cut at once.
    pub fn stop(&self) {
        if self.installed() {
            if let Some(a) = crate::transport::host::uninstall() {
                // spawn-detached: finishes the uninstalled instance's own shutdown.
                drop(crate::transport::spawn_on(async move { a.transport().shutdown().await }));
            }
            log_info!("[Tor] stopped");
        }
    }

    /// [`Self::stop`], returning only once the bridge holds none of its streams, so the state-dir
    /// lock is free for a subsequent start.
    pub async fn stop_and_join(&self) {
        if self.installed() {
            crate::transport::host::deactivate(true).await;
            log_info!("[Tor] stopped (joined)");
        }
    }

    /// For display only: the bridge address. A connect there needs credentials.
    pub fn proxy_url(&self) -> String {
        proxy_url().unwrap_or_default()
    }

    /// The bridge address. A connect there needs credentials.
    pub fn socks_addr(&self) -> SocketAddr {
        crate::transport::bridge::addr().unwrap_or(crate::transport::bridge::NOWHERE)
    }

    /// Latest bootstrap / running state.
    pub fn status(&self) -> TorStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The installed Tor service, if Tor is the active kind.
pub fn current() -> Option<Arc<TorService>> {
    crate::transport::host::active_as::<TorService>()
}

/// For display only (`socks5h://127.0.0.1:<port>`, no credentials) while Tor is installed.
pub fn proxy_url() -> Option<String> {
    current().and_then(|_| crate::transport::bridge::display_url())
}

/// The bridge address while Tor is installed. A connect there without credentials is refused.
pub fn socks_addr() -> Option<SocketAddr> {
    current().and_then(|_| crate::transport::bridge::addr().ok())
}

/// Is Tor the installed kind?
pub fn is_active() -> bool {
    current().is_some()
}

/// What the transport status reports for Tor while no instance is installed.
pub(crate) fn uninstalled_status() -> crate::transport::status::KindStatus {
    use crate::transport::status::{KindStatus, Phase, Reason};
    let booting = is_bootstrapping();
    let error = if booting { None } else { last_bootstrap_error() };
    let (phase, status) = match (&error, booting) {
        (Some(_), _) => (Phase::Failed, "failed"),
        (None, true) => (Phase::Starting, "bootstrapping"),
        (None, false) => (Phase::Starting, "starting"),
    };
    KindStatus {
        phase,
        progress: booting.then(bootstrap_progress),
        reason: error.map(|e| Reason::new("tor_failed", e)),
        retry_in: None,
        steps: Vec::new(),
        detail: serde_json::json!({
            "status": status,
            "bootstrap_progress": bootstrap_progress(),
            "multi_circuit": multi_circuit(),
        }),
    }
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

/// Arm (Tor chosen on the welcome screen) or disarm the carry. Disarming never touches
/// another kind's carry.
pub fn arm_prelogin_carry(armed: bool) {
    use crate::transport::prelogin;
    if armed {
        prelogin::arm(prelogin::PreloginChoice::new(Kind::Tor));
    } else if prelogin_carry_armed() {
        prelogin::disarm();
    }
}

/// Is the welcome screen's Tor choice waiting for an account to commit?
pub fn prelogin_carry_armed() -> bool {
    crate::transport::prelogin::armed().is_some_and(|c| c.kind == Kind::Tor)
}

/// An account's stored Tor preference, raised while the welcome screen's choice is armed. The
/// same rule hydration uses.
pub fn effective_tor_pref(stored: bool) -> bool {
    use crate::transport::prefs::StoredKind;
    let stored = if stored { StoredKind::Kind(Kind::Tor) } else { StoredKind::Absent };
    crate::transport::prelogin::effective(&stored) == Some(Kind::Tor)
}

/// The account on screen committed: it keeps the welcome screen's choice.
pub fn commit_prelogin_carry() {
    crate::transport::prelogin::commit();
}

pub fn prelogin_generation() -> u64 {
    crate::transport::prelogin::generation()
}

pub fn cancel_prelogin_start() {
    crate::transport::prelogin::cancel_start();
}

/// Whether the install remembers Tor as the welcome screen's choice.
pub fn prelogin_preference() -> bool {
    matches!(crate::transport::prelogin::marker(), Ok(Some(c)) if c.kind == Kind::Tor)
}

/// Remember Tor for this install, or forget it (only when Tor is what is remembered).
pub fn set_prelogin_preference(enabled: bool) -> Result<(), String> {
    use crate::transport::prelogin;
    if enabled {
        prelogin::set_marker(Some(&prelogin::PreloginChoice::new(Kind::Tor)))
    } else if prelogin_preference() {
        prelogin::set_marker(None)
    } else {
        Ok(())
    }
}

/// Install-level `(state, cache)` dirs for the service the welcome screen runs.
pub fn prelogin_dirs() -> Result<(PathBuf, PathBuf), String> {
    let base = crate::db::get_app_data_dir()?.join("tor");
    let (state, cache) = (base.join("state"), base.join("cache"));
    std::fs::create_dir_all(&state).map_err(|e| format!("create state dir: {e}"))?;
    std::fs::create_dir_all(&cache).map_err(|e| format!("create cache dir: {e}"))?;
    Ok((state, cache))
}

/// Fill an empty account cache from the install-level one, so the account's first bootstrap
/// skips the consensus download. The cache holds only public directory documents; guards live
/// in `state`, which is never shared. Callers must have stopped the service first.
pub fn seed_cache_from_prelogin(account_cache: &std::path::Path) {
    let Ok(app) = crate::db::get_app_data_dir() else { return };
    match seed_cache(&app.join("tor").join("cache"), account_cache) {
        Ok(true) => log_info!("[Tor] seeded account cache from the pre-login cache"),
        Ok(false) => {}
        Err(e) => log_warn!("[Tor] cache seed skipped: {e}"),
    }
}

/// Copy `src` into `dst` only when `dst` is empty. Copies beside it and swaps in whole, so a
/// torn copy never becomes the cache (an empty one just bootstraps from scratch).
fn seed_cache(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<bool> {
    let empty = std::fs::read_dir(dst).map_or(true, |mut d| d.next().is_none());
    if !empty || !src.is_dir() {
        return Ok(false);
    }
    fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let dest = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&entry.path(), &dest)?;
            } else {
                std::fs::copy(entry.path(), dest)?;
            }
        }
        Ok(())
    }
    let tmp = dst.with_extension("seed");
    let _ = std::fs::remove_dir_all(&tmp);
    let swapped = copy_dir(src, &tmp).and_then(|()| {
        if dst.exists() {
            std::fs::remove_dir(dst)?;
        }
        std::fs::rename(&tmp, dst)
    });
    if let Err(e) = swapped {
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::create_dir_all(dst);
        return Err(e);
    }
    Ok(true)
}

/// `true` chooses Tor for the live session; `false` returns to Clearnet only when Tor was the
/// choice, so it never turns another kind off.
pub fn set_tor_enabled_pref(enabled: bool) {
    if enabled {
        crate::transport::set_preference(Some(Kind::Tor));
    } else if crate::transport::preference() == Some(Kind::Tor) {
        crate::transport::set_preference(Some(Kind::Clearnet));
    }
}

/// Reload the live session's network settings from its database.
pub fn init_tor_enabled_pref_from_db() {
    let session = crate::db::live_session();
    if let Some(path) = session.db_path() {
        crate::transport::prefs::hydrate(&session, &path);
    }
}

/// The transport every TCP client honours, seen as Tor. Fail safe for outside callers:
/// anything but an active Tor or a chosen Clearnet is `RequiredButInactive`.
pub fn transport_state() -> TorTransportState {
    match crate::transport::state() {
        TransportState::Active { kind: Kind::Tor } => match crate::transport::bridge::addr() {
            Ok(addr) => TorTransportState::Active(addr),
            Err(_) => TorTransportState::RequiredButInactive,
        },
        TransportState::Clearnet => TorTransportState::Disabled,
        _ => TorTransportState::RequiredButInactive,
    }
}

/// Where a refused connection may point: nothing can listen on port 0.
#[deprecated(note = "egress is refused in process now; see vector_core::transport::egress")]
pub fn blackhole_proxy_addr() -> SocketAddr {
    crate::transport::bridge::NOWHERE
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
    drop(guard);
    host_isolation().lock().unwrap_or_else(|e| e.into_inner()).clear();
    host_tunnels().lock().unwrap_or_else(|e| e.into_inner()).clear();
    *LATEST_HOST.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Multi-circuit (the default): every host rides its own circuit, so one stalled circuit costs
/// one connection and no exit sees the whole set of relays and servers. Off: one shared circuit.
static MULTI_CIRCUIT: AtomicBool = AtomicBool::new(true);

/// Is each host on its own circuit?
pub fn multi_circuit() -> bool {
    MULTI_CIRCUIT.load(Ordering::Acquire)
}

/// Switch circuit mode. Callers rotate the circuits and cycle sockets so live streams move too.
pub fn set_multi_circuit(on: bool) {
    MULTI_CIRCUIT.store(on, Ordering::Release);
}

#[cfg(feature = "tor")]
type HostTokens = std::collections::HashMap<String, tor_circmgr::isolation::IsolationToken>;

#[cfg(feature = "tor")]
fn host_isolation() -> &'static Mutex<HostTokens> {
    static SLOT: OnceLock<Mutex<HostTokens>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(HostTokens::new()))
}

/// The circuit each host's latest stream rode, held weakly: a closed circuit simply drops out.
#[cfg(feature = "tor")]
type HostTunnels = std::collections::HashMap<String, std::sync::Weak<tor_proto::ClientTunnel>>;

#[cfg(feature = "tor")]
fn host_tunnels() -> &'static Mutex<HostTunnels> {
    static SLOT: OnceLock<Mutex<HostTunnels>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(HostTunnels::new()))
}

/// The isolation a new stream to `host` carries: its own per-host token, or the shared one.
#[cfg(feature = "tor")]
pub fn isolation_for(host: &str) -> tor_circmgr::isolation::IsolationToken {
    if !multi_circuit() {
        return current_isolation_token();
    }
    *host_isolation()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(host.to_ascii_lowercase())
        .or_insert_with(tor_circmgr::isolation::IsolationToken::new)
}

/// Remember the circuit a stream to `host` was attached to.
#[cfg(feature = "tor")]
pub(crate) fn record_tunnel(host: &str, tunnel: &Arc<tor_proto::ClientTunnel>) {
    let mut map = host_tunnels().lock().unwrap_or_else(|e| e.into_inner());
    // Bounded by hosts contacted this generation; dead entries go once it grows.
    if map.len() > 256 {
        map.retain(|_, t| t.strong_count() > 0);
    }
    let host = host.to_ascii_lowercase();
    map.insert(host.clone(), Arc::downgrade(tunnel));
    *LATEST_HOST.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

/// Open circuits carrying traffic now, and how many hosts they serve.
#[cfg(feature = "tor")]
pub fn active_circuits() -> (usize, usize) {
    let map = host_tunnels().lock().unwrap_or_else(|e| e.into_inner());
    let mut circuits = std::collections::HashSet::new();
    let mut hosts = 0;
    for tunnel in map.values().filter_map(std::sync::Weak::upgrade) {
        hosts += 1;
        circuits.insert(Arc::as_ptr(&tunnel) as usize);
    }
    (circuits.len(), hosts)
}

/// The circuit `host` is currently using, if a stream to it went over Tor recently and that
/// circuit is still open.
#[cfg(feature = "tor")]
pub fn host_circuit_hops(host: &str) -> Option<Result<Vec<CircuitHop>, String>> {
    let tunnel = host_tunnels()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&host.to_ascii_lowercase())
        .and_then(std::sync::Weak::upgrade)?;
    Some(tunnel_hops(&tunnel))
}

#[cfg(not(feature = "tor"))]
pub fn host_circuit_hops(_host: &str) -> Option<Result<Vec<CircuitHop>, String>> {
    None
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
    /// "Guard" / "Middle" / "Exit", or for an onion service "Rendezvous" / "Onion" at the end.
    /// Derived from the position in the path.
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

/// The circuit Vector's traffic is on, with the host whose stream rode it last: the most
/// recently used circuit that is still open. Before any traffic, one is built for the shared
/// isolation so the view isn't empty. "New circuit" is `rotate_circuits` plus a socket cycle.
#[cfg(feature = "tor")]
pub async fn current_circuit_hops() -> Result<(Option<String>, Vec<CircuitHop>), String> {
    use tor_circmgr::isolation::StreamIsolation;
    use tor_dirmgr::Timeliness;

    let latest = latest_tunnel();
    if let Some((host, tunnel)) = latest {
        return Ok((Some(host), tunnel_hops(&tunnel)?));
    }

    let svc = current().ok_or_else(|| "Tor not running".to_string())?;
    let netdir = svc
        .client
        .dirmgr()
        .map_err(|e| format!("dirmgr unavailable: {e}"))?
        .netdir(Timeliness::Timely)
        .map_err(|e| format!("netdir unavailable: {e}"))?;
    let isolation = StreamIsolation::builder()
        .owner_token(current_isolation_token())
        .build()
        .map_err(|e| format!("isolation build: {e}"))?;
    let tunnel = svc
        .client
        .circmgr()
        .map_err(|e| format!("circmgr unavailable: {e}"))?
        .get_or_launch_exit(netdir.as_ref().into(), &[], isolation)
        .await
        .map_err(|e| format!("launch exit: {e}"))?;
    Ok((None, tunnel_hops(tunnel.as_ref())?))
}

/// The host whose stream most recently rode a circuit, with that circuit, while it is open.
#[cfg(feature = "tor")]
fn latest_tunnel() -> Option<(String, Arc<tor_proto::ClientTunnel>)> {
    let slot = LATEST_HOST.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    let tunnel = host_tunnels().lock().unwrap_or_else(|e| e.into_inner()).get(&slot)?.upgrade()?;
    Some((slot, tunnel))
}

#[cfg(feature = "tor")]
static LATEST_HOST: Mutex<Option<String>> = Mutex::new(None);

#[cfg(feature = "tor")]
fn tunnel_hops(tunnel: &tor_proto::ClientTunnel) -> Result<Vec<CircuitHop>, String> {
    use tor_linkspec::{HasAddrs, HasRelayIds, RelayIdType};
    let path = tunnel
        .all_paths()
        .into_iter()
        .next()
        .ok_or_else(|| "tunnel has no path".to_string())?;

    let total = path.n_hops();
    // A circuit to an onion service ends in the service's end-to-end layer, which has no relay:
    // the hop before it is the rendezvous point, and there is no exit.
    let onion = path.hops().last().is_some_and(|h| h.as_chan_target().is_none());
    let bridge_addrs = active_bridge_addrs();
    let hops = path
        .hops()
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let position = match i {
                0 => "Guard",
                n if n + 1 == total && onion => "Onion",
                n if n + 1 == total => "Exit",
                n if n + 2 == total && onion => "Rendezvous",
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
                    address: "End to end".to_string(),
                    fingerprint: String::new(),
                    is_bridge: false,
                },
            }
        })
        .collect();

    Ok(hops)
}

#[cfg(not(feature = "tor"))]
pub async fn current_circuit_hops() -> Result<(Option<String>, Vec<CircuitHop>), String> {
    Err("Vector was built without the `tor` feature.".to_string())
}

#[cfg(test)]
mod circuit_mode_tests {
    use super::*;

    // ONE test: the mode and tokens are process-global, so split tests would race.
    #[test]
    fn isolation_follows_the_circuit_mode() {
        set_multi_circuit(true);
        let a = isolation_for("relay.one.example");
        assert_eq!(a, isolation_for("RELAY.one.example"), "a host keeps its circuit");
        assert_ne!(a, isolation_for("relay.two.example"), "each host gets its own circuit");

        rotate_circuits();
        assert_ne!(a, isolation_for("relay.one.example"), "a new circuit replaces every host's");

        set_multi_circuit(false);
        let shared = current_isolation_token();
        assert_eq!(isolation_for("relay.one.example"), shared, "single mode shares one circuit");
        assert_eq!(isolation_for("relay.two.example"), shared);
        set_multi_circuit(true);
    }
}

#[cfg(test)]
mod prelogin_tests {
    use super::seed_cache;

    #[test]
    fn seed_fills_only_an_empty_cache_and_leaves_no_temp_dir() {
        let root = std::env::temp_dir().join(format!("vector-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, dst) = (root.join("src"), root.join("dst"));
        std::fs::create_dir_all(src.join("dir_blobs")).unwrap();
        std::fs::write(src.join("dir.sqlite3"), b"consensus").unwrap();
        std::fs::write(src.join("dir_blobs").join("a"), b"blob").unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        assert!(seed_cache(&src, &dst).unwrap(), "an empty cache is seeded");
        assert_eq!(std::fs::read(dst.join("dir.sqlite3")).unwrap(), b"consensus");
        assert_eq!(std::fs::read(dst.join("dir_blobs").join("a")).unwrap(), b"blob");
        assert!(!dst.with_extension("seed").exists(), "the staging copy is swapped in, not left behind");

        // An account's own cache is never overwritten.
        std::fs::write(src.join("dir.sqlite3"), b"newer").unwrap();
        assert!(!seed_cache(&src, &dst).unwrap());
        assert_eq!(std::fs::read(dst.join("dir.sqlite3")).unwrap(), b"consensus");

        // No install cache yet: nothing to copy, and the account starts from scratch.
        let fresh = root.join("fresh");
        assert!(!seed_cache(&root.join("missing"), &fresh).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }
}
