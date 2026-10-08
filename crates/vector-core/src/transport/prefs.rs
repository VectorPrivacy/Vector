//! The account's network choice and routing settings. One source of truth: the session's
//! [`SessionPrefs`]; instances never hold a copy.
//!
//! Nothing outside this file reads or writes the legacy Tor row: it is a projection kept so an
//! older build of the same schema stays fail closed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use super::aliases::{AliasEntry, AliasTable};
use super::{Kind, KindConfig};

pub const KEY_TRANSPORT: &str = "transport";
pub const KEY_LEGACY_TOR: &str = "tor_enabled";
pub const KEY_ALIASES: &str = "transport_aliases";
pub const KEY_MULTI_CIRCUIT: &str = "tor_multi_circuit";
pub const KEY_I2P_CONFIG: &str = "transport_cfg_i2p";

/// `transport_cfg_{kind}`: one JSON row per kind.
pub fn config_key(kind: Kind) -> String {
    format!("transport_cfg_{}", kind.as_str())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredKind {
    Absent,
    Kind(Kind),
    Unparseable(String),
    ReadError(String),
}

pub struct StoredPrefs {
    pub kind: StoredKind,
    pub multi_circuit: bool,
    pub configs: HashMap<Kind, KindConfig>,
    pub aliases: Vec<AliasEntry>,
}

fn row(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>, String> {
    match conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0)) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// The strictest reading wins, so a round trip through an older build can only raise.
pub fn resolve(transport: Option<&str>, legacy_tor: Option<&str>) -> StoredKind {
    let tor_on = matches!(legacy_tor, Some("1") | Some("true"));
    match transport {
        Some(v) => match Kind::parse(v) {
            Some(Kind::Clearnet) if tor_on => StoredKind::Kind(Kind::Tor),
            Some(k) => StoredKind::Kind(k),
            None => StoredKind::Unparseable(v.to_string()),
        },
        None if tor_on => StoredKind::Kind(Kind::Tor),
        None if legacy_tor.is_some() => StoredKind::Kind(Kind::Clearnet),
        None => StoredKind::Absent,
    }
}

pub fn read_stored(conn: &rusqlite::Connection) -> StoredPrefs {
    let kind = match (row(conn, KEY_TRANSPORT), row(conn, KEY_LEGACY_TOR)) {
        (Ok(t), Ok(l)) => resolve(t.as_deref(), l.as_deref()),
        (Err(e), _) | (_, Err(e)) => StoredKind::ReadError(e),
    };
    let multi_circuit = !matches!(row(conn, KEY_MULTI_CIRCUIT), Ok(Some(ref v)) if v == "0" || v == "false");
    let configs = Kind::ALL
        .into_iter()
        .map(|k| match row(conn, &config_key(k)) {
            Ok(json) => {
                let json = json.map(zeroize::Zeroizing::new);
                (k, super::kinds::decode(k, json.as_deref().map(String::as_str)))
            }
            Err(e) => {
                crate::log_warn!("[Transport] could not read the {} settings: {e}", k.label());
                (k, super::kinds::decode_unreadable(k))
            }
        })
        .collect();
    let aliases = AliasTable::parse(row(conn, KEY_ALIASES).ok().flatten().as_deref()).entries();
    StoredPrefs { kind, multi_circuit, configs, aliases }
}

struct PrefsKey;

pub struct SessionPrefs {
    /// 0 Unknown, else `Kind::code`.
    kind: AtomicU8,
    configs: RwLock<HashMap<Kind, KindConfig>>,
    aliases: RwLock<Arc<AliasTable>>,
    consent: Mutex<Option<Kind>>,
    config_seq: AtomicU64,
    /// The database these settings were loaded from. Loading it again only raises: a choice
    /// whose save failed stays in force until Vector restarts, as the user was told.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    hydrated_from: Mutex<Option<std::path::PathBuf>>,
}

impl Default for SessionPrefs {
    fn default() -> Self {
        let kind = if super::is_strict() && !cfg!(target_arch = "wasm32") { 0 } else { Kind::Clearnet.code() };
        SessionPrefs {
            kind: AtomicU8::new(kind),
            configs: RwLock::new(HashMap::new()),
            aliases: RwLock::new(Arc::new(AliasTable::default())),
            consent: Mutex::new(None),
            config_seq: AtomicU64::new(0),
            hydrated_from: Mutex::new(None),
        }
    }
}

impl SessionPrefs {
    pub fn kind(&self) -> Option<Kind> {
        Kind::from_code(self.kind.load(Ordering::Acquire))
    }

    pub fn config(&self, kind: Kind) -> KindConfig {
        if let Some(c) = self.configs.read().unwrap_or_else(|e| e.into_inner()).get(&kind) {
            return c.clone();
        }
        super::kinds::decode(kind, None)
    }

    pub fn aliases(&self) -> Arc<AliasTable> {
        self.aliases.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn config_seq(&self) -> u64 {
        self.config_seq.load(Ordering::Acquire)
    }

    pub(crate) fn set_aliases(&self, table: Arc<AliasTable>) {
        *self.aliases.write().unwrap_or_else(|e| e.into_inner()) = table;
        self.config_seq.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn set_config_value(&self, kind: Kind, cfg: KindConfig) {
        self.configs.write().unwrap_or_else(|e| e.into_inner()).insert(kind, cfg);
        self.config_seq.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn consent(&self) -> Option<Kind> {
        *self.consent.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn set_consent(&self, k: Option<Kind>) {
        *self.consent.lock().unwrap_or_else(|e| e.into_inner()) = k;
    }
}

/// Insert the entry before the session is shared, so a rebound clone of the session's map holds
/// the same `Arc` instead of growing its own.
pub fn attach(session: &Arc<crate::db::Session>) {
    let _ = of(session);
}

pub fn of(session: &Arc<crate::db::Session>) -> Arc<SessionPrefs> {
    session.scoped::<PrefsKey, SessionPrefs>()
}

struct ConfigWrites;

/// Held across a kind config's read, persist and install, so two writers of the same session
/// (a settings change, the welcome screen's commit retry, a reload) never drop each other's change.
pub(crate) fn config_write_lock() -> Arc<Mutex<()>> {
    config_write_lock_of(&crate::db::current_session())
}

fn config_write_lock_of(session: &Arc<crate::db::Session>) -> Arc<Mutex<()>> {
    session.scoped::<ConfigWrites, Mutex<()>>()
}

/// The live session's settings: unbound callers (relay tasks, the bridge) act for it.
pub fn live() -> Arc<SessionPrefs> {
    of(&crate::db::live_session())
}

/// The settings of the session this work belongs to.
pub fn current() -> Arc<SessionPrefs> {
    of(&crate::db::current_session())
}

/// Change `session`'s kind. A live session's change moves the epoch; a kind change also drops
/// its realtime consent.
pub(crate) fn set_kind(session: &Arc<crate::db::Session>, kind: Option<Kind>) {
    let p = of(session);
    let code = kind.map_or(0, Kind::code);
    let prev = p.kind.swap(code, Ordering::AcqRel);
    if prev == code {
        return;
    }
    if Kind::from_code(prev) != kind {
        p.set_consent(None);
    }
    if session.is_live() {
        super::bump_epoch();
        super::host::notify(super::host::TransportEvent::Changed);
    }
}

/// The live session's config for `kind`, parsed once.
pub fn config(kind: Kind) -> KindConfig {
    live().config(kind)
}

/// Install `cfg` into the session this work belongs to, the same one `persist_config` writes.
pub fn set_config(kind: Kind, cfg: KindConfig) {
    let session = crate::db::current_session();
    of(&session).set_config_value(kind, cfg);
    if session.is_live() {
        super::bump_policy_gen();
        super::host::notify(super::host::TransportEvent::Changed);
    }
}

/// Load the account's settings into THIS session (never resolved through the task binding: the
/// caller is installing it). A read error leaves the session Unknown.
pub fn hydrate(session: &Arc<crate::db::Session>, db_path: &std::path::Path) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = db_path;
        set_kind(session, Some(Kind::Clearnet));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let stored = match crate::db::connect_at(db_path) {
            Ok(conn) => read_stored(&conn),
            Err(e) => StoredPrefs {
                kind: StoredKind::ReadError(e),
                multi_circuit: true,
                configs: Kind::ALL.into_iter().map(|k| (k, super::kinds::decode_unreadable(k))).collect(),
                aliases: Vec::new(),
            },
        };
        let p = of(session);
        let mut from = p.hydrated_from.lock().unwrap_or_else(|e| e.into_inner());
        if from.as_deref() == Some(db_path) {
            reapply_stored(session, stored);
        } else {
            apply_stored(session, stored);
            *from = Some(db_path.to_path_buf());
        }
    }
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn apply_stored(session: &Arc<crate::db::Session>, stored: StoredPrefs) {
    #[cfg(feature = "tor")]
    crate::tor::set_multi_circuit(stored.multi_circuit);
    let p = of(session);
    {
        let lock = config_write_lock_of(session);
        let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
        *p.configs.write().unwrap_or_else(|e| e.into_inner()) = stored.configs;
    }
    p.set_aliases(Arc::new(AliasTable::from_entries(stored.aliases)));
    if let StoredKind::ReadError(e) = &stored.kind {
        crate::log_warn!("[Transport] could not read the network preference: {e}");
    }
    set_kind(session, super::prelogin::effective(&stored.kind));
}

/// The same account's settings loaded again into a session that already holds them: what the
/// session holds wins, unless the row is stricter. Every change saves before it loosens, so only
/// a tightening whose save failed can differ, and it stays.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn reapply_stored(session: &Arc<crate::db::Session>, stored: StoredPrefs) {
    let p = of(session);
    {
        let lock = config_write_lock_of(session);
        let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut configs = p.configs.write().unwrap_or_else(|e| e.into_inner());
        for (k, row) in stored.configs {
            let next = match configs.get(&k) {
                Some(held) => super::kinds::stricter(k, held, &row),
                None => row,
            };
            configs.insert(k, next);
        }
    }
    p.config_seq.fetch_add(1, Ordering::AcqRel);
    set_kind(session, stricter_kind(p.kind(), super::prelogin::effective(&stored.kind)));
}

/// Unknown over any kind, any anonymity kind over Clearnet; between two anonymity kinds the
/// session's own, the user's latest choice.
pub(crate) fn stricter_kind(held: Option<Kind>, stored: Option<Kind>) -> Option<Kind> {
    match (held, stored) {
        (_, None) => None,
        (None, s) => s,
        (Some(Kind::Clearnet), s) => s,
        (Some(h), Some(_)) => Some(h),
    }
}

/// The stored kind of the session this work belongs to.
pub fn stored_kind() -> StoredKind {
    match crate::db::get_db_connection_guard_static() {
        Ok(conn) => read_stored(&conn).kind,
        Err(e) => StoredKind::ReadError(e),
    }
}

/// `transport` and its legacy projection, in one transaction.
pub fn persist_kind(kind: Kind) -> Result<(), String> {
    let mut conn = crate::db::get_write_connection_guard_static()?;
    persist_kind_on(&mut conn, kind)
}

pub fn persist_kind_on(conn: &mut rusqlite::Connection, kind: Kind) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Failed to save the network choice: {e}"))?;
    let legacy = if kind == Kind::Clearnet { "0" } else { "1" };
    for (k, v) in [(KEY_TRANSPORT, kind.as_str()), (KEY_LEGACY_TOR, legacy)] {
        tx.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", rusqlite::params![k, v])
            .map_err(|e| format!("Failed to save the network choice: {e}"))?;
    }
    tx.commit().map_err(|e| format!("Failed to save the network choice: {e}"))
}

pub fn persist_config(kind: Kind, json: &str) -> Result<(), String> {
    persist_setting(&config_key(kind), json)
}

/// Borrowed straight into the statement: a row that can hold a secret is never copied here.
pub(crate) fn persist_setting(key: &str, value: &str) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", rusqlite::params![key, value])
        .map_err(|e| format!("Failed to set setting: {e}"))?;
    Ok(())
}

/// JSON written once into a buffer of its exact size, so no reallocation leaves a copy of a
/// secret behind, and zeroized when dropped.
pub fn secret_json<T: serde::Serialize + ?Sized>(v: &T) -> zeroize::Zeroizing<String> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 += b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    if serde_json::to_writer(&mut count, v).is_err() {
        return zeroize::Zeroizing::new("{}".into());
    }
    let mut buf = Vec::with_capacity(count.0);
    if serde_json::to_writer(&mut buf, v).is_err() {
        zeroize::Zeroize::zeroize(&mut buf);
        return zeroize::Zeroizing::new("{}".into());
    }
    // serde_json writes UTF-8 only.
    zeroize::Zeroizing::new(String::from_utf8(buf).unwrap_or_default())
}

/// Wipe every string in a JSON value that may have held a secret before it is dropped.
pub fn scrub_json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => zeroize::Zeroize::zeroize(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(scrub_json),
        serde_json::Value::Object(o) => o.values_mut().for_each(scrub_json),
        _ => {}
    }
}

/// Every settings key this module owns: the generic settings commands never touch them.
pub const PROTECTED_KEYS: &[&str] = &[KEY_TRANSPORT, KEY_LEGACY_TOR, KEY_I2P_CONFIG, KEY_ALIASES];
