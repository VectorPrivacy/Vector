//! The I2P kind's settings. Compiled in every build so Settings reads and writes them anywhere.

use super::ExitPolicy;

pub const DEFAULT_SAM_PORT: u16 = 7656;
pub const MAX_OUTPROXIES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Outproxy {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub enabled: bool,
    pub builtin: bool,
}

impl Outproxy {
    pub fn builtin(id: &str, name: &str, address: &str, port: u16) -> Self {
        Outproxy { id: id.into(), name: name.into(), address: address.into(), port, enabled: true, builtin: true }
    }

    /// A user entry: its id is derived from what it points at, so re-adding it is a duplicate.
    pub fn custom(name: &str, address: &str, port: u16, enabled: bool) -> Self {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(format!("{address}:{port}").as_bytes());
        let id = format!("custom-{}", crate::simd::hex::bytes_to_hex_string(&digest[..4]));
        Outproxy { id, name: name.into(), address: address.into(), port, enabled, builtin: false }
    }
}

/// Verified live through a client-only i2pd: StormyCloud allows CONNECT to 443 only; Acetone
/// answers 443 and 80.
pub fn defaults() -> Vec<Outproxy> {
    vec![
        Outproxy::builtin("stormycloud", "StormyCloud", "5d4s7pcvfdpftfk7npc7hllyujhufsdprtrf4o53i44rgsa2xbwa.b32.i2p", 80),
        Outproxy::builtin("acetone", "Acetone", "proxy4uwdijqxac2bvdx4fuhem6njmiwukuk2gelejv2nzxka2xq.b32.i2p", 3128),
    ]
}

/// A SAM password: zeroized wherever a copy of it is dropped, and never printed.
#[derive(Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SamSecret(String);

impl Drop for SamSecret {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl std::fmt::Debug for SamSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SamSecret(..)")
    }
}

impl std::ops::Deref for SamSecret {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<String> for SamSecret {
    fn from(s: String) -> Self {
        SamSecret(s)
    }
}

impl From<&str> for SamSecret {
    fn from(s: &str) -> Self {
        SamSecret(s.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct I2pConfig {
    pub sam_port: u16,
    pub sam_user: Option<String>,
    pub sam_password: Option<SamSecret>,
    #[serde(default)]
    pub exit: ExitPolicy,
    /// `None` = the built-in list.
    pub outproxies: Option<Vec<Outproxy>>,
}

impl Default for I2pConfig {
    fn default() -> Self {
        I2pConfig { sam_port: DEFAULT_SAM_PORT, sam_user: None, sam_password: None, exit: ExitPolicy::Allow, outproxies: None }
    }
}

impl I2pConfig {
    /// Absent JSON is the default config. A row that is there but can't be read fails closed:
    /// what it says is kept field by field, and anything short of an explicit `"allow"` (a policy
    /// a newer build wrote, a corrupt row) is I2P-Only, with no outproxy it can't name.
    pub fn parse(json: Option<&str>) -> Self {
        let Some(j) = json else { return Self::default() };
        if let Ok(cfg) = serde_json::from_str::<I2pConfig>(j) {
            return cfg;
        }
        crate::log_warn!("[Transport] the I2P settings row can't be read; I2P-Only until it is saved again");
        let mut v = serde_json::from_str::<serde_json::Value>(j).unwrap_or_default();
        let cfg = Self::salvage(&v);
        super::prefs::scrub_json(&mut v);
        cfg
    }

    /// The strict reading of a settings row that exists but can't be read.
    pub fn unreadable() -> Self {
        Self::salvage(&serde_json::Value::Null)
    }

    fn salvage(v: &serde_json::Value) -> Self {
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str);
        let sam_port = v.get("sam_port").and_then(serde_json::Value::as_i64).and_then(|p| validate_port(p).ok()).unwrap_or(DEFAULT_SAM_PORT);
        let login = validate_sam_auth(text("sam_user"), text("sam_password")).ok().flatten();
        let outproxies = v.get("outproxies").and_then(|o| serde_json::from_value::<Option<Vec<Outproxy>>>(o.clone()).ok());
        let exit = match (text("exit"), &outproxies) {
            (Some("allow"), Some(_)) => ExitPolicy::Allow,
            _ => ExitPolicy::Off,
        };
        I2pConfig {
            sam_port,
            sam_user: login.as_ref().map(|(u, _)| u.clone()),
            sam_password: login.map(|(_, p)| p.into()),
            exit,
            outproxies: outproxies.unwrap_or_else(|| Some(Vec::new())),
        }
    }

    /// The stricter of two readings of the same account's settings: I2P-Only if either has it.
    pub fn stricter(&self, other: &I2pConfig) -> I2pConfig {
        let mut out = self.clone();
        if other.exit == ExitPolicy::Off {
            out.exit = ExitPolicy::Off;
        }
        out
    }

    /// The stored row. It can hold the SAM password, so it is zeroized when dropped.
    pub fn to_json(&self) -> zeroize::Zeroizing<String> {
        super::prefs::secret_json(self)
    }

    pub fn outproxy_list(&self) -> Vec<Outproxy> {
        self.outproxies.clone().unwrap_or_else(defaults)
    }

    /// Whether this config and `other` can share one router connection.
    pub fn same_router(&self, other: &I2pConfig) -> bool {
        self.sam_port == other.sam_port && self.sam_user == other.sam_user && self.sam_password == other.sam_password
    }
}

pub fn validate_port(p: i64) -> Result<u16, String> {
    if (1..=65535).contains(&p) {
        Ok(p as u16)
    } else {
        Err("Enter a port from 1 to 65535.".into())
    }
}

/// SAM values are space-delimited, so a credential carries no space, quote or `=`.
pub fn validate_sam_auth(user: Option<&str>, password: Option<&str>) -> Result<Option<(String, String)>, String> {
    match (user.filter(|s| !s.is_empty()), password.filter(|s| !s.is_empty())) {
        (None, None) => Ok(None),
        (Some(u), Some(p)) => {
            let ok = |s: &str| (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'=');
            if ok(u) && ok(p) {
                Ok(Some((u.to_string(), p.to_string())))
            } else {
                Err("Use letters, numbers and symbols, without spaces or quotes.".into())
            }
        }
        _ => Err("Enter both a username and a password.".into()),
    }
}

/// A `.b32.i2p` or a `name.i2p` address.
pub fn is_i2p_address(addr: &str) -> bool {
    let a = addr.trim().to_ascii_lowercase();
    super::route::is_b32(&a) || (a.ends_with(".i2p") && a.len() > 4 && super::route::valid_hostname(&a))
}

pub fn validate_outproxy_list(list: &[Outproxy], exit: ExitPolicy) -> Result<(), String> {
    if list.len() > MAX_OUTPROXIES {
        return Err("You can add up to 8 outproxies.".into());
    }
    let mut seen = std::collections::HashSet::new();
    for o in list {
        let name = o.name.trim();
        if name.is_empty() || name.chars().count() > 32 {
            return Err("Enter a name up to 32 characters.".into());
        }
        if !is_i2p_address(&o.address) {
            return Err("Enter a .b32.i2p or .i2p address.".into());
        }
        if o.port == 0 {
            return Err("Enter a port from 1 to 65535.".into());
        }
        if !seen.insert((o.address.trim().to_ascii_lowercase(), o.port)) {
            return Err("This outproxy is already in the list.".into());
        }
    }
    if exit == ExitPolicy::Allow && !list.iter().any(|o| o.enabled) {
        return Err("Keep one outproxy on, or turn on I2P-Only.".into());
    }
    Ok(())
}

// ── Settings changes (the shell's I2P commands are thin wrappers of these) ────────────────

/// What Settings shows. The SAM password never leaves core.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct I2pConfigView {
    pub sam_port: u16,
    pub sam_user: Option<String>,
    pub sam_auth: bool,
    pub exit: ExitPolicy,
    pub outproxies: Vec<Outproxy>,
    pub defaults: Vec<Outproxy>,
    pub customized: bool,
}

/// One row of a saved outproxy list; the id is derived here.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct OutproxyInput {
    pub name: String,
    pub address: String,
    pub port: i64,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// A change that tightened the policy took effect but could not be saved.
pub const UNSAVED: &str = "Couldn't save this. It applies until Vector restarts.";

/// The I2P config of the account this work belongs to.
pub fn current() -> I2pConfig {
    super::prefs::current().config(super::Kind::I2p).downcast_ref::<I2pConfig>().cloned().unwrap_or_default()
}

pub fn view_of(cfg: &I2pConfig) -> I2pConfigView {
    let outproxies = cfg.outproxy_list();
    let defaults = defaults();
    I2pConfigView {
        sam_port: cfg.sam_port,
        sam_user: cfg.sam_user.clone().filter(|_| cfg.sam_password.is_some()),
        sam_auth: cfg.sam_user.is_some() && cfg.sam_password.is_some(),
        exit: cfg.exit,
        customized: outproxies != defaults,
        outproxies,
        defaults,
    }
}

pub fn view() -> I2pConfigView {
    view_of(&current())
}

/// Turn I2P-Only on (`Off`) or off (`Allow`).
pub fn set_exit(mode: ExitPolicy) -> Result<I2pConfigView, String> {
    let lock = super::prefs::config_write_lock();
    let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = current();
    if cfg.exit == mode {
        return Ok(view_of(&cfg));
    }
    if mode == ExitPolicy::Allow {
        validate_outproxy_list(&cfg.outproxy_list(), mode)?;
    }
    let next = I2pConfig { exit: mode, ..cfg };
    match mode {
        ExitPolicy::Off => tighten(next, |_| true),
        ExitPolicy::Allow => loosen(next),
    }
}

/// Save the outproxy list as the user ordered it; `None` restores the built-in list.
pub fn set_outproxies(list: Option<Vec<OutproxyInput>>) -> Result<I2pConfigView, String> {
    let lock = super::prefs::config_write_lock();
    let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = current();
    // The built-in list as it ships is stored as none, so a later release's defaults reach it.
    let next_list = match list {
        None => None,
        Some(rows) => Some(outproxies_from(&rows)?).filter(|l| *l != defaults()),
    };
    let effective = next_list.clone().unwrap_or_else(defaults);
    validate_outproxy_list(&effective, cfg.exit)?;
    let before: std::collections::HashSet<String> = cfg.outproxy_list().into_iter().filter(|o| o.enabled).map(|o| o.id).collect();
    let after: std::collections::HashSet<String> = effective.iter().filter(|o| o.enabled).map(|o| o.id.clone()).collect();
    let dropped: Vec<String> = before.difference(&after).cloned().collect();
    let next = I2pConfig { outproxies: next_list, ..cfg };
    if dropped.is_empty() {
        loosen(next)
    } else {
        tighten(next, move |id| dropped.iter().any(|d| d == id))
    }
}

/// Rows as entered: a row pointing at a built-in outproxy stays that built-in.
pub fn outproxies_from(rows: &[OutproxyInput]) -> Result<Vec<Outproxy>, String> {
    let builtins = defaults();
    rows.iter()
        .map(|r| {
            let port = validate_port(r.port)?;
            let address = r.address.trim().to_ascii_lowercase();
            if !is_i2p_address(&address) {
                return Err("Enter a .b32.i2p or .i2p address.".to_string());
            }
            Ok(match builtins.iter().find(|b| b.address == address && b.port == port) {
                Some(b) => Outproxy { enabled: r.enabled, ..b.clone() },
                None => Outproxy::custom(r.name.trim(), &address, port, r.enabled),
            })
        })
        .collect()
}

/// The router Vector talks to. `auth`: `None` keeps the saved credentials, `Some(None)` clears
/// them. A running I2P instance for another router is the shell's to restart.
pub fn set_router(port: i64, auth: Option<Option<(String, String)>>) -> Result<I2pConfigView, String> {
    let lock = super::prefs::config_write_lock();
    let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
    let port = validate_port(port)?;
    let cfg = current();
    let (sam_user, sam_password) = match auth {
        None => (cfg.sam_user.clone(), cfg.sam_password.clone()),
        Some(None) => (None, None),
        Some(Some((u, p))) => match validate_sam_auth(Some(&u), Some(&zeroize::Zeroizing::new(p)))? {
            Some((u, p)) => (Some(u), Some(p.into())),
            None => (None, None),
        },
    };
    let next = I2pConfig { sam_port: port, sam_user, sam_password, ..cfg };
    loosen(next)
}

/// Save first, then use: a failed save changes nothing.
fn loosen(next: I2pConfig) -> Result<I2pConfigView, String> {
    super::prefs::persist_config(super::Kind::I2p, &next.to_json())?;
    install(&next);
    Ok(view_of(&next))
}

/// Use first, cutting every I2P exit the change forbids (`hit` takes an outproxy id), then
/// save. A failed save keeps the stricter policy in force.
fn tighten(next: I2pConfig, hit: impl Fn(&str) -> bool) -> Result<I2pConfigView, String> {
    let all_exits = next.exit == ExitPolicy::Off;
    install(&next);
    #[cfg(not(target_arch = "wasm32"))]
    if crate::db::current_session().is_live() {
        if let Some(a) = super::host::active().filter(|a| a.kind == super::Kind::I2p) {
            let id = a.id();
            // An exit still dialing hasn't said which outproxy it is trying: it may be the one
            // just removed, so it goes too and the client dials again on the new list.
            super::bridge::abort_where(|c| {
                c.instance == id
                    && matches!(c.route, Some(super::Route::Exit { .. }))
                    && (all_exits || c.outproxy.as_deref().map_or(!c.spliced, &hit))
            });
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (&hit, all_exits);
    super::prefs::persist_config(super::Kind::I2p, &next.to_json()).map_err(|_| UNSAVED.to_string())?;
    Ok(view_of(&next))
}

fn install(next: &I2pConfig) {
    let session = crate::db::current_session();
    super::prefs::of(&session).set_config_value(super::Kind::I2p, std::sync::Arc::new(next.clone()));
    if session.is_live() {
        super::bump_policy_gen();
        super::host::notify(super::host::TransportEvent::Changed);
    }
}
