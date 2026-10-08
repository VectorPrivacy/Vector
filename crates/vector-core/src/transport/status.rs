//! Serde views shared by every shell (Tauri and Vector Web), with the reason codes and texts.

use std::sync::Mutex;

use serde::Serialize;

use super::{Kind, TransportState};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub code: String,
    pub text: String,
}

impl Reason {
    pub fn new(code: &str, text: impl Into<String>) -> Self {
        Reason { code: code.into(), text: text.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Step {
    pub id: String,
    pub label: String,
    /// `pending` | `ok` | `fail` | `off` | `unknown`.
    pub state: String,
    pub text: String,
}

impl Step {
    pub fn new(id: &str, label: &str, state: &str, text: impl Into<String>) -> Self {
        Step { id: id.into(), label: label.into(), state: state.into(), text: text.into() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Direct,
    Off,
    Starting,
    Waiting,
    Ready,
    Failed,
    Unsupported,
}

/// What a kind reports about itself.
#[derive(Clone, Debug, Serialize)]
pub struct KindStatus {
    pub phase: Phase,
    pub progress: Option<u8>,
    pub reason: Option<Reason>,
    pub retry_in: Option<u64>,
    pub steps: Vec<Step>,
    pub detail: serde_json::Value,
}

impl KindStatus {
    pub fn ready(detail: serde_json::Value) -> Self {
        KindStatus { phase: Phase::Ready, progress: None, reason: None, retry_in: None, steps: Vec::new(), detail }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TransportStateView {
    pub kind: String,
    pub label: Option<&'static str>,
    pub supported: Vec<Kind>,
    pub phase: Phase,
    pub ready: bool,
    pub blocked: bool,
    pub progress: Option<u8>,
    pub reason: Option<Reason>,
    pub retry_in: Option<u64>,
    pub since: u64,
    pub prelogin: String,
    pub prelogin_armed: bool,
    pub realtime_allowed: bool,
    pub realtime_active: bool,
    pub call_active: bool,
    pub config_seq: u64,
    pub steps: Vec<Step>,
    pub detail: serde_json::Value,
}

pub fn not_in_build(k: Kind) -> Reason {
    Reason::new("not_in_build", format!("This build doesn't include {}.", k.label()))
}

pub fn unknown_network() -> Reason {
    Reason::new("unknown_network", "Choose how Vector connects.")
}

/// The status of a kind whose instance is not installed (Tor reports its bootstrap here).
fn uninstalled_status(k: Kind) -> KindStatus {
    if !k.compiled() {
        return KindStatus {
            phase: Phase::Unsupported,
            progress: None,
            reason: Some(not_in_build(k)),
            retry_in: None,
            steps: Vec::new(),
            detail: serde_json::Value::Null,
        };
    }
    match k {
        #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
        Kind::Tor => crate::tor::uninstalled_status(),
        _ => KindStatus {
            phase: Phase::Starting,
            progress: None,
            reason: None,
            retry_in: None,
            steps: Vec::new(),
            detail: serde_json::Value::Null,
        },
    }
}

/// The chosen kind's status: its live instance's, or what it reports while not installed.
pub fn kind_status(k: Kind) -> KindStatus {
    match super::host::active() {
        Some(a) if a.kind == k && a.owner() == crate::db::live_session_id() => a.transport().kind_status(),
        _ => uninstalled_status(k),
    }
}

/// Why `k` is blocked right now, when it says.
pub fn reason_for(k: Kind) -> Option<Reason> {
    kind_status(k).reason
}

/// Blocks no amount of waiting lifts: a network this build lacks, a start that failed and is
/// retried only by the user, or a router that turned Vector down. A transfer that waits on one
/// only spends its whole start budget to fail the same way.
const NEEDS_USER: &[&str] = &["not_in_build", "tor_failed", "sam_refused", "sam_too_old", "sam_auth_required", "sam_auth_failed"];

/// Whether the live account's network is held by something only the user can change.
pub fn waits_for_user() -> bool {
    let Some(k) = super::preference().filter(|k| *k != Kind::Clearnet) else { return false };
    let ks = kind_status(k);
    matches!(ks.phase, Phase::Failed | Phase::Unsupported) || ks.reason.is_some_and(|r| NEEDS_USER.contains(&r.code.as_str()))
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

static PHASE_SINCE: Mutex<Option<(String, Phase, u64)>> = Mutex::new(None);

fn since(kind: &str, phase: Phase) -> u64 {
    let mut slot = PHASE_SINCE.lock().unwrap_or_else(|e| e.into_inner());
    match slot.as_ref() {
        Some((k, p, t)) if k == kind && *p == phase => *t,
        _ => {
            let t = now_secs();
            *slot = Some((kind.to_string(), phase, t));
            t
        }
    }
}

fn realtime_active() -> bool {
    #[cfg(feature = "xdc")]
    {
        crate::xdc::session::any_live()
    }
    #[cfg(not(feature = "xdc"))]
    {
        false
    }
}

pub fn call_active() -> bool {
    #[cfg(feature = "calls")]
    {
        crate::calls::session::snapshot().is_some()
    }
    #[cfg(not(feature = "calls"))]
    {
        false
    }
}

pub fn view() -> TransportStateView {
    let pref = super::preference();
    let state = super::state();
    let (kind, label, ks) = match pref {
        None => (
            "unknown".to_string(),
            None,
            KindStatus {
                phase: Phase::Off,
                progress: None,
                reason: Some(unknown_network()),
                retry_in: None,
                steps: Vec::new(),
                detail: serde_json::Value::Null,
            },
        ),
        Some(Kind::Clearnet) => (
            "clearnet".to_string(),
            Some(Kind::Clearnet.label()),
            KindStatus {
                phase: Phase::Direct,
                progress: None,
                reason: None,
                retry_in: None,
                steps: Vec::new(),
                detail: serde_json::Value::Null,
            },
        ),
        Some(k) => (k.as_str().to_string(), Some(k.label()), kind_status(k)),
    };
    let ready = matches!(state, TransportState::Clearnet | TransportState::Active { .. });
    let prelogin = match super::prelogin::marker_kind() {
        Ok(Some(k)) => k.as_str().to_string(),
        Ok(None) => "clearnet".to_string(),
        Err(super::prelogin::UnreadableMarker) => "unknown".to_string(),
    };
    let phase = if ready && ks.phase != Phase::Direct { Phase::Ready } else { ks.phase };
    TransportStateView {
        since: since(&kind, phase),
        kind,
        label,
        supported: super::supported(),
        phase,
        ready,
        blocked: !ready,
        progress: ks.progress,
        reason: if ready { None } else { ks.reason },
        retry_in: ks.retry_in,
        prelogin,
        prelogin_armed: super::prelogin::armed().is_some(),
        realtime_allowed: super::realtime::allowed(),
        realtime_active: realtime_active(),
        call_active: call_active(),
        config_seq: super::prefs::live().config_seq(),
        steps: ks.steps,
        detail: ks.detail,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RouteView {
    pub url: String,
    pub host: String,
    pub kind: String,
    /// `direct` | `native` | `twin` | `exit` | `refused` | `blocked`.
    pub class: String,
    pub via: Option<String>,
    pub text: String,
    pub last_error: Option<String>,
}

/// How long a recorded failure still describes a host.
const RECENT_FAILURE: std::time::Duration = std::time::Duration::from_secs(30);

pub fn route_view(url: &str) -> RouteView {
    let (host, port) = super::host_port(url).unwrap_or_default();
    let pref = super::preference();
    let kind = pref.map_or("unknown", Kind::as_str).to_string();
    let mk = |class: &str, via: Option<String>, text: String, last_error: Option<String>| RouteView {
        url: url.to_string(),
        host: host.clone(),
        kind: kind.clone(),
        class: class.to_string(),
        via,
        text,
        last_error,
    };
    let Some(dest) = super::Dest::parse(&host) else {
        return mk("refused", None, super::Refusal::BadName.text(), None);
    };
    if let Some(r) = super::route::pre_route(pref.unwrap_or(Kind::Clearnet), &dest) {
        return mk("refused", None, r.text(), None);
    }
    match super::state() {
        TransportState::Clearnet => mk("direct", None, "Direct".into(), None),
        TransportState::Unknown => {
            let t = super::ConnectError::NotReady(None).text();
            mk("blocked", None, format!("Blocked: {t}"), Some(t))
        }
        TransportState::RequiredButInactive { kind } => {
            let t = reason_for(kind).map(|r| r.text).unwrap_or_else(|| super::ConnectError::NotReady(Some(kind)).text());
            mk("blocked", None, format!("Blocked: {t}"), Some(t))
        }
        TransportState::Active { kind } => {
            let Some(active) = super::host::active().filter(|a| a.kind == kind) else {
                return mk("blocked", None, super::ConnectError::NotReady(Some(kind)).text(), None);
            };
            let p = super::prefs::live();
            let (aliases, config) = (p.aliases(), p.config(kind));
            let route = active.transport().route(&dest, port, &super::RouteCtx { aliases: &aliases, config: &config });
            let last = active
                .last_failure(&dest.host())
                .filter(|(at, _)| at.elapsed() < RECENT_FAILURE)
                .map(|(_, e)| e.text());
            match &route {
                super::Route::Refuse(r) => mk("refused", None, r.text(), last),
                super::Route::Native { .. } => mk("native", None, format!("Inside {}", kind.label()), last),
                super::Route::Twin { via, .. } => mk("twin", Some(via.clone()), format!("Through its {}", kind.address_noun()), last),
                super::Route::Exit { .. } => {
                    let via = active.last_exit(&dest.host()).map(|id| outproxy_name(&config, &id));
                    let text = match (kind, &via) {
                        (Kind::Tor, _) => "Through Tor".to_string(),
                        (_, Some(name)) => format!("Through the {name} outproxy"),
                        (_, None) => "Through an outproxy".to_string(),
                    };
                    mk("exit", via, text, last)
                }
            }
        }
    }
}

fn outproxy_name(config: &super::KindConfig, id: &str) -> String {
    config
        .downcast_ref::<super::i2p_config::I2pConfig>()
        .and_then(|c| c.outproxy_list().into_iter().find(|o| o.id == id).map(|o| o.name))
        .unwrap_or_else(|| id.to_string())
}
