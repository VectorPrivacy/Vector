//! I2P's own settings: the router Vector talks to, I2P-Only, and the outproxy list. Compiled in
//! every build so Settings reads and saves them anywhere; reaching the router needs `i2p`.

use serde::Serialize;
use serde_json::Value;
use tauri::ipc::{InvokeBody, Request};
use vector_core::transport::i2p_config::{self, I2pConfigView, OutproxyInput};
use vector_core::transport::status::TransportStateView;
use vector_core::transport::ExitPolicy;

use super::transport as lifecycle;

/// What the router test reports: a HELLO, no session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RouterTestView {
    pub ok: bool,
    pub version: Option<String>,
    pub text: String,
}

/// The I2P settings in effect: the account's, or on the welcome screen the router it picked. The
/// SAM password never leaves core.
#[tauri::command]
pub fn i2p_get_config() -> I2pConfigView {
    let cfg = vector_core::transport::prelogin::config_for(vector_core::transport::Kind::I2p);
    i2p_config::view_of(&cfg.downcast_ref::<i2p_config::I2pConfig>().cloned().unwrap_or_default())
}

/// Credentials from the raw arguments: both keys left out keeps the saved ones; given (both
/// `null` clears them).
pub(crate) fn auth_arg(args: &Value) -> Result<Option<Option<(String, String)>>, String> {
    let (user, password) = (args.get("user"), args.get("password"));
    if user.is_none() && password.is_none() {
        return Ok(None);
    }
    let text = |v: Option<&Value>| v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    i2p_config::validate_sam_auth(text(user).as_deref(), text(password).as_deref()).map(Some)
}

/// Save the router's port and SAM credentials; a running I2P moves onto that router.
#[tauri::command]
pub async fn i2p_set_router(port: i64, request: Request<'_>) -> Result<TransportStateView, String> {
    let auth = match request.body() {
        InvokeBody::Json(args) => auth_arg(args)?,
        InvokeBody::Raw(_) => None,
    };
    vector_core::db::scoped_result(lifecycle::set_i2p_router(port, auth)).await
}

/// Ask whatever answers on `port` for a SAM HELLO. Without credentials given, the saved ones,
/// but only for the saved router's port: they never go to whatever else listens.
#[tauri::command]
pub async fn i2p_test_router(port: i64, user: Option<String>, password: Option<String>) -> Result<RouterTestView, String> {
    let port = i2p_config::validate_port(port)?;
    let given = i2p_config::validate_sam_auth(user.as_deref(), password.as_deref())?;
    #[cfg(feature = "i2p")]
    {
        let auth = match given {
            Some((u, p)) => Some(vector_core::i2p::sam::SamAuth::new(&u, &zeroize::Zeroizing::new(p))),
            None => saved_auth_for(port),
        };
        // spawn-detached: one loopback HELLO on the runtime that owns router sockets; awaited.
        let r = vector_core::transport::spawn_on(async move { vector_core::i2p::sam::test_router(port, auth.as_ref()).await })
            .await
            .map_err(|e| e.to_string())?;
        Ok(RouterTestView { ok: r.ok, version: r.version, text: r.text })
    }
    #[cfg(not(feature = "i2p"))]
    {
        let _ = (port, given);
        Err(vector_core::transport::status::not_in_build(vector_core::transport::Kind::I2p).text)
    }
}

/// The saved SAM credentials, when `port` is the saved router's.
#[cfg(feature = "i2p")]
fn saved_auth_for(port: u16) -> Option<vector_core::i2p::sam::SamAuth> {
    let cfg = vector_core::transport::prelogin::config_for(vector_core::transport::Kind::I2p);
    let cfg = cfg.downcast_ref::<i2p_config::I2pConfig>()?;
    if cfg.sam_port != port {
        return None;
    }
    match (cfg.sam_user.as_deref(), cfg.sam_password.as_deref()) {
        (Some(u), Some(p)) => Some(vector_core::i2p::sam::SamAuth::new(u, p)),
        _ => None,
    }
}

/// I2P-Only: `"off"` reaches only I2P servers and the I2P addresses the user added; `"allow"`
/// reaches everything else through the outproxies.
#[tauri::command]
pub async fn i2p_set_exit(mode: String) -> Result<TransportStateView, String> {
    let mode = match mode.as_str() {
        "allow" => ExitPolicy::Allow,
        "off" => ExitPolicy::Off,
        _ => return Err(format!("Unknown I2P-Only mode: {mode}")),
    };
    vector_core::db::scoped_result(lifecycle::set_i2p_exit(mode)).await
}

/// Save the outproxy list in the order given; `null` restores the built-in list.
#[tauri::command]
pub async fn i2p_set_outproxies(list: Option<Vec<OutproxyInput>>) -> Result<I2pConfigView, String> {
    vector_core::db::scoped_result(lifecycle::set_i2p_outproxies(list)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Leaving the credentials out keeps the saved ones, so the port row never needs the password.
    #[test]
    fn router_credentials_keep_clear_or_set() {
        assert_eq!(auth_arg(&json!({ "port": 7656 })), Ok(None));
        assert_eq!(auth_arg(&json!({ "port": 7656, "user": null, "password": null })), Ok(Some(None)));
        assert_eq!(auth_arg(&json!({ "user": "", "password": "" })), Ok(Some(None)));
        assert_eq!(auth_arg(&json!({ "user": "vec", "password": "p4ss!" })), Ok(Some(Some(("vec".into(), "p4ss!".into())))));
        assert_eq!(auth_arg(&json!({ "user": "vec" })).unwrap_err(), "Enter both a username and a password.");
        assert_eq!(auth_arg(&json!({ "user": "v ec", "password": "p" })).unwrap_err(), "Use letters, numbers and symbols, without spaces or quotes.");
        assert_eq!(auth_arg(&json!({ "user": "vec", "password": "a=b" })).unwrap_err(), "Use letters, numbers and symbols, without spaces or quotes.");
    }

    /// Saved SAM credentials go only to the saved router's port, never to whatever else answers.
    #[cfg(feature = "i2p")]
    #[tokio::test]
    async fn saved_credentials_stay_with_their_port() {
        use vector_core::transport::{prefs, Kind};
        let _serial = super::super::transport::tests::TEST_LOCK.lock().await;
        let cfg = i2p_config::I2pConfig { sam_port: 7656, sam_user: Some("vec".into()), sam_password: Some("p4ss".into()), ..Default::default() };
        prefs::set_config(Kind::I2p, std::sync::Arc::new(cfg));
        let auth = saved_auth_for(7656).expect("the saved router gets its credentials");
        assert_eq!((auth.user.as_str(), auth.password.as_str()), ("vec", "p4ss"));
        assert!(saved_auth_for(7657).is_none(), "another port is tested without them");
        prefs::set_config(Kind::I2p, std::sync::Arc::new(i2p_config::I2pConfig::default()));
    }
}
