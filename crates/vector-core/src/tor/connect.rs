//! Tor as a transport kind: the bridge's dial becomes an arti stream on the host's isolation.

use std::sync::Arc;

use arti_client::{IntoTorAddr, StreamPrefs};
use tokio_util::compat::FuturesAsyncReadCompatExt;

use crate::transport::status::KindStatus;
use crate::transport::{
    BoxedStream, ConnectError, Dialed, Dest, Kind, KindConfig, Lane, Route, RouteCtx, StartCtx, Transport, TransportFactory,
};

use super::TorService;

/// What a Tor start needs beyond its directories.
#[derive(Clone, Debug, Default)]
pub struct TorStartConfig {
    pub bridges: Vec<String>,
}

#[async_trait::async_trait]
impl Transport for TorService {
    fn kind(&self) -> Kind {
        Kind::Tor
    }

    fn route(&self, dest: &Dest, port: u16, ctx: &RouteCtx) -> Route {
        crate::transport::route::route_tor(dest, port, ctx)
    }

    /// Every arti failure is `Unreachable`, which the bridge answers with 0x04 as before.
    async fn dial(&self, route: &Route, _lane: Lane) -> Result<(BoxedStream, Dialed), ConnectError> {
        let (host, port) = match route {
            Route::Native { host, port } | Route::Exit { host, port } => (host.as_str(), *port),
            Route::Twin { via, port, .. } => (via.as_str(), *port),
            Route::Refuse(r) => return Err(ConnectError::Refused(*r)),
        };
        let addr = (host, port).into_tor_addr().map_err(|e| ConnectError::Unreachable(format!("addr parse: {e}")))?;
        let mut prefs = StreamPrefs::new();
        prefs.set_isolation(super::isolation_for(host));
        let stream = match self.client.connect_with_prefs(addr, &prefs).await {
            Ok(s) => s,
            Err(e) => {
                crate::log_debug!("[Tor] connect({}:{}) failed: {}", host, port, e);
                return Err(ConnectError::Unreachable(format!("tor connect: {e}")));
            }
        };
        {
            use tor_proto::client::stream::ClientStreamCtrl;
            if let Some(tunnel) = stream.client_stream_ctrl().and_then(|c| c.tunnel()) {
                super::record_tunnel(host, &tunnel);
            }
        }
        Ok((Box::new(stream.compat()), Dialed::default()))
    }

    /// Installed only once bootstrapped.
    fn ready(&self) -> bool {
        true
    }

    fn kind_status(&self) -> KindStatus {
        let (status, progress) = if super::is_bootstrapping() {
            ("bootstrapping", super::bootstrap_progress())
        } else {
            ("connected", 100)
        };
        KindStatus::ready(serde_json::json!({
            "status": status,
            "bootstrap_progress": progress,
            "multi_circuit": super::multi_circuit(),
        }))
    }

    async fn new_identity(&self) {
        super::rotate_circuits();
    }

    /// Arti stops when its last handle drops; the host has already cut every stream.
    async fn shutdown(&self) {}

    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }
}

pub struct TorFactory;

#[async_trait::async_trait]
impl TransportFactory for TorFactory {
    fn kind(&self) -> Kind {
        Kind::Tor
    }

    async fn start(&self, ctx: StartCtx) -> Result<Arc<dyn Transport>, String> {
        let (state, cache) = ctx.dirs.ok_or_else(|| "Tor needs its data directories.".to_string())?;
        let bridges = ctx.config.downcast_ref::<TorStartConfig>().map(|c| c.bridges.clone()).unwrap_or_default();
        let budget = crate::transport::budget(crate::transport::Op::Startup, std::time::Duration::ZERO);
        match crate::rt::time::timeout(budget, TorService::bootstrap(state, cache, &bridges)).await {
            Ok(Ok(svc)) => Ok(svc),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // The timeout cancels the bootstrap before it can record its own failure.
                let msg = format!("Tor timed out after {}s.", budget.as_secs());
                super::set_last_bootstrap_error(msg.clone());
                Err(msg)
            }
        }
    }

    fn adopt_on_unlock(&self) -> bool {
        false
    }

    fn compatible(&self, _inst: &dyn Transport, _cfg: &KindConfig) -> bool {
        true
    }
}
