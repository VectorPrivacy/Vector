//! The desktop client's Mini App core, compiled from its sources: packages, the `webxdc://`
//! scheme and bridge script, permissions and the realtime channel. Windows are the host's.

#[path = "../../../../src-tauri/src/miniapps/error.rs"]
#[allow(dead_code)]
pub mod error;
#[path = "../../../../src-tauri/src/miniapps/marketplace.rs"]
#[allow(dead_code)]
pub mod marketplace;
#[path = "../../../../src-tauri/src/miniapps/isolated.rs"]
#[allow(dead_code)]
mod isolated_core;
#[cfg(target_os = "linux")]
#[path = "../../../../src-tauri/src/miniapps/network_isolation.rs"]
pub mod network_isolation;
#[path = "../../../../src-tauri/src/miniapps/realtime.rs"]
#[allow(dead_code)]
pub mod realtime;
#[path = "../../../../src-tauri/src/miniapps/rt_ws.rs"]
#[allow(dead_code)]
pub mod rt_ws;
#[path = "../../../../src-tauri/src/miniapps/scheme.rs"]
#[allow(dead_code)]
pub mod scheme;
#[path = "../../../../src-tauri/src/miniapps/state.rs"]
#[allow(dead_code)]
pub mod state;

pub use vector_core::webxdc_permissions as permissions;

/// Runs first in every frame of a Mini App: engine shims, API removals, permission guards.
pub const INIT_SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/miniapp_init.js"));

/// The bridge script `webxdc.js` the pages load, for a host that serves it itself.
pub async fn bridge_js() -> String {
    let (npub, name) = scheme::get_user_info().await;
    scheme::generate_webxdc_bridge_js(&npub, &name)
}

/// Answers one `webxdc://` request from the Mini App window `label`.
pub fn serve(
    label: &str,
    request: http::Request<Vec<u8>>,
    respond: impl FnOnce(http::Response<std::borrow::Cow<'static, [u8]>>) + Send + 'static,
) {
    let app = *crate::TAURI_APP.get().expect("TAURI_APP set at startup");
    scheme::miniapp_protocol(
        tauri::UriSchemeContext::new(label, app),
        request,
        tauri::UriSchemeResponder::new(respond),
    );
}

/// The process's Mini App state; the host manages it once at startup.
pub fn apps() -> &'static state::MiniAppsState {
    tauri::manage(state::MiniAppsState::new())
}

/// The proxy every Linux Mini App webview is pointed at, so it has no network.
#[cfg(target_os = "linux")]
pub fn network_sink() -> Option<&'static url::Url> {
    network_isolation::proxy_url()
}

/// The loopback host cross-origin isolated apps are served from.
pub mod isolated {
    use super::isolated_core as core;

    pub struct Started {
        pub id: u64,
        pub port: u16,
        pub boot_url: url::Url,
    }

    pub enum StartError {
        /// Another window already serves this app's store.
        AlreadyOpen(String),
        Failed(String),
    }

    pub async fn start(app: &tauri::AppHandle, label: &str, partition: &str, href: Option<&str>) -> Result<Started, StartError> {
        match core::start(app, label, partition, href).await {
            Ok(s) => Ok(Started { id: s.id, port: s.port, boot_url: s.boot_url }),
            Err(core::StartError::AlreadyOpen(l)) => Err(StartError::AlreadyOpen(l)),
            Err(core::StartError::Failed(e)) => Err(StartError::Failed(e)),
        }
    }

    pub fn origin(port: u16) -> String {
        core::origin(port)
    }

    pub fn window_for_partition(partition: &str, except_label: &str) -> Option<String> {
        core::window_for_partition(partition, except_label)
    }

    pub fn stop_if(label: &str, id: u64) {
        core::stop_if(label, id)
    }

    #[cfg(target_os = "macos")]
    pub fn store_identifier(partition: &str) -> Option<[u8; 16]> {
        core::store_identifier(partition)
    }

    #[cfg(target_os = "linux")]
    pub fn enable_web_storage(view: &webkit2gtk::WebView) -> usize {
        core::enable_web_storage(view)
    }

    /// The realtime WebSocket's port, which an isolated app reaches past its proxy.
    pub fn rt_ws_port() -> Option<u16> {
        super::scheme::rt_ws_port()
    }
}
