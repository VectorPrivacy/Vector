//! The desktop client's Mini App core, compiled from its sources: packages, the `webxdc://`
//! scheme and bridge script, permissions and the realtime channel. Windows are the host's.

#[path = "../../../../src-tauri/src/miniapps/error.rs"]
#[allow(dead_code)]
pub mod error;
#[path = "../../../../src-tauri/src/miniapps/marketplace.rs"]
#[allow(dead_code)]
pub mod marketplace;
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
