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
use std::sync::atomic::{AtomicBool, Ordering};
use std::net::SocketAddr;

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

/// Returns true while `TorService::start()` is mid-execution. Useful for
/// status surfaces that would otherwise see `is_active() == false` and
/// mistakenly report "off" during the 5–15s bootstrap window.
pub fn is_bootstrapping() -> bool {
    TOR_BOOTSTRAPPING.load(Ordering::Acquire)
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
    /// Latest bootstrap state.
    status: Mutex<TorStatus>,
}

impl TorService {
    /// Bootstrap Arti and start the SOCKS5 listener. Awaits full bootstrap
    /// before returning. `state_dir` and `cache_dir` are persisted across
    /// runs — caching the consensus directory dramatically speeds subsequent
    /// boots (~2s vs the 10–15s first-boot consensus fetch).
    pub async fn start(state_dir: PathBuf, cache_dir: PathBuf) -> Result<Arc<Self>, String> {
        log_info!("[Tor] starting; state={} cache={}", state_dir.display(), cache_dir.display());
        TOR_BOOTSTRAPPING.store(true, Ordering::Release);
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
        let config = config_builder
            .build()
            .map_err(|e| format!("Tor config build: {e}"))?;

        let runtime = PreferredRuntime::current()
            .map_err(|e| format!("Tor runtime acquire (need an active tokio runtime): {e}"))?;

        let client = TorClient::with_runtime(runtime)
            .config(config)
            .create_unbootstrapped()
            .map_err(|e| format!("Tor client create: {e}"))?;

        let status = Mutex::new(TorStatus::Bootstrapping(0));

        // TODO: subscribe to bootstrap_events() in a background task so the UI
        // can show real progress. For now we just block on the bootstrap call
        // and report 0 → 100 in two states.
        log_info!("[Tor] bootstrapping...");
        client
            .bootstrap()
            .await
            .map_err(|e| format!("Tor bootstrap: {e}"))?;
        log_info!("[Tor] bootstrap complete");

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
        tokio::spawn(async move {
            socks::run(listener, client_for_socks, shutdown_rx).await;
            log_info!("[Tor] SOCKS5 listener stopped");
        });

        *status.lock().unwrap() = TorStatus::Connected;

        let service = Arc::new(TorService {
            client,
            socks_addr,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            status,
        });

        *tor_slot().lock().unwrap() = Some(Arc::clone(&service));
        Ok(service)
    }

    /// Stop the SOCKS listener and unregister from the global slot.
    /// In-flight Tor connections drop when the last `TorClient` ref goes away.
    pub fn stop(&self) {
        if let Some(tx) = self.shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        *tor_slot().lock().unwrap() = None;
        log_info!("[Tor] stopped");
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
        self.status.lock().unwrap().clone()
    }
}

/// Returns the active Tor service if Tor is currently enabled.
pub fn current() -> Option<Arc<TorService>> {
    tor_slot().lock().unwrap().clone()
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
