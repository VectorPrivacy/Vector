//! Twins: a server's address inside another network, used as a transport alias. The relay URL,
//! SNI, Host header and NIP-42 relay tag all stay clearnet, so only TLS on 443 may ride a twin:
//! a wrong twin can then only fail.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::Kind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasSource {
    #[default]
    User,
    Nip11,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    #[default]
    Unchecked,
    Ok,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct AliasCheck {
    pub state: CheckState,
    #[serde(default)]
    pub at: Option<u64>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AliasEntry {
    pub host: String,
    pub twins: BTreeMap<Kind, String>,
    #[serde(default)]
    pub source: AliasSource,
    #[serde(default)]
    pub check: AliasCheck,
}

#[derive(Clone, Debug, Default)]
pub struct AliasTable {
    by_host: HashMap<String, AliasEntry>,
}

impl AliasTable {
    pub fn from_entries(entries: Vec<AliasEntry>) -> Self {
        AliasTable { by_host: entries.into_iter().map(|e| (e.host.clone(), e)).collect() }
    }

    pub fn parse(json: Option<&str>) -> Self {
        Self::from_entries(json.and_then(|j| serde_json::from_str::<Vec<AliasEntry>>(j).ok()).unwrap_or_default())
    }

    /// The twin to route through: never one whose check failed.
    pub fn twin(&self, host: &str, kind: Kind) -> Option<&str> {
        let e = self.by_host.get(host)?;
        if e.check.state == CheckState::Failed {
            return None;
        }
        e.twins.get(&kind).map(String::as_str)
    }

    pub fn get(&self, host: &str) -> Option<&AliasEntry> {
        self.by_host.get(host)
    }

    pub fn entries(&self) -> Vec<AliasEntry> {
        let mut v: Vec<AliasEntry> = self.by_host.values().cloned().collect();
        v.sort_by(|a, b| a.host.cmp(&b.host));
        v
    }

    fn to_json(&self) -> String {
        serde_json::to_string(&self.entries()).unwrap_or_else(|_| "[]".into())
    }
}

/// The live session's snapshot.
pub fn table() -> Arc<AliasTable> {
    super::prefs::live().aliases()
}

/// A URL or a bare host, as the lowercase host the router sees.
pub fn normalize_host(input: &str) -> Result<String, String> {
    let (host, _) = super::host_port(input.trim()).ok_or_else(|| "That isn't a valid server address.".to_string())?;
    match super::Dest::parse(&host) {
        Some(super::Dest::Domain(d)) if super::route::valid_hostname(&d) => Ok(d),
        _ => Err("That isn't a valid server address.".into()),
    }
}

pub fn validate_twin(kind: Kind, addr: &str) -> Result<String, String> {
    match kind {
        Kind::I2p if super::i2p_config::is_i2p_address(addr) => Ok(addr.trim().to_ascii_lowercase()),
        Kind::I2p => Err("Enter a .b32.i2p address.".into()),
        Kind::Tor if super::route::is_onion(&addr.trim().to_ascii_lowercase()) => Ok(addr.trim().to_ascii_lowercase()),
        Kind::Tor => Err("Enter a .onion address.".into()),
        _ => Err(format!("{} addresses aren't supported yet.", kind.label())),
    }
}

fn now() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

struct AliasWrites;

/// Held across read, persist and apply: a user edit and a background check result each copy
/// the table, so without it the later write drops the earlier one.
fn write_lock(session: &Arc<crate::db::Session>) -> Arc<std::sync::Mutex<()>> {
    session.scoped::<AliasWrites, std::sync::Mutex<()>>()
}

/// Change one twin. Any change aborts that host's twin connections and bumps the policy
/// generation, never the epoch. `None` removes the twin (and the entry once it has none).
pub fn set(host: &str, kind: Kind, addr: Option<&str>, source: AliasSource) -> Result<Option<AliasEntry>, String> {
    let host = normalize_host(host)?;
    let addr = addr.map(|a| validate_twin(kind, a)).transpose()?;
    let session = crate::db::current_session();
    let lock = write_lock(&session);
    let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
    let prefs = super::prefs::of(&session);
    let current = prefs.aliases();
    let mut map = current.by_host.clone();
    let result = match addr {
        Some(a) => {
            let e = map.entry(host.clone()).or_insert_with(|| AliasEntry {
                host: host.clone(),
                twins: BTreeMap::new(),
                source,
                check: AliasCheck::default(),
            });
            if e.twins.get(&kind) != Some(&a) {
                e.twins.insert(kind, a);
                e.check = AliasCheck::default();
            }
            e.source = source;
            Some(e.clone())
        }
        None => {
            if let Some(e) = map.get_mut(&host) {
                e.twins.remove(&kind);
                if e.twins.is_empty() {
                    map.remove(&host);
                }
            }
            None
        }
    };
    let next = AliasTable { by_host: map };
    super::prefs::persist_setting(super::prefs::KEY_ALIASES, &next.to_json())?;
    apply(&session, next, &host);
    Ok(result)
}

/// Record a probe result. A check turning `failed` stops routing through the twin at once.
pub fn record_check(host: &str, check: AliasCheck) -> Result<(), String> {
    write_check(host, None, check)
}

/// [`record_check`] for the twin that was probed: a result for an address the user has since
/// changed is dropped.
pub fn record_check_for(host: &str, kind: Kind, via: &str, check: AliasCheck) -> Result<(), String> {
    write_check(host, Some((kind, via)), check)
}

fn write_check(host: &str, probed: Option<(Kind, &str)>, check: AliasCheck) -> Result<(), String> {
    let host = normalize_host(host)?;
    let session = crate::db::current_session();
    let lock = write_lock(&session);
    let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
    let prefs = super::prefs::of(&session);
    let mut map = prefs.aliases().by_host.clone();
    let Some(e) = map.get_mut(&host) else { return Ok(()) };
    if let Some((kind, via)) = probed {
        if e.twins.get(&kind).map(String::as_str) != Some(via) {
            return Ok(());
        }
    }
    let mut check = check;
    if check.at.is_none() {
        check.at = Some(now());
    }
    e.check = check;
    let next = AliasTable { by_host: map };
    super::prefs::persist_setting(super::prefs::KEY_ALIASES, &next.to_json())?;
    apply(&session, next, &host);
    Ok(())
}

fn apply(session: &Arc<crate::db::Session>, next: AliasTable, host: &str) {
    #[cfg(target_arch = "wasm32")]
    let _ = host;
    super::prefs::of(session).set_aliases(Arc::new(next));
    if session.is_live() {
        super::bump_policy_gen();
        #[cfg(not(target_arch = "wasm32"))]
        super::bridge::abort_where(|c| matches!(&c.route, Some(super::Route::Twin { host: h, .. }) if h == host));
        super::host::notify(super::host::TransportEvent::Changed);
    }
}
