//! The Tor kind's settings: Vector's own Tor, or a Tor already running on this device. Compiled in
//! every build so Settings reads and writes them anywhere.

/// Where a running Tor listens: Tor Browser, then a tor daemon (or Orbot).
pub const PROBE_PORTS: [u16; 2] = [9150, 9050];

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TorConfig {
    /// Use a Tor already running here instead of starting Vector's own.
    #[serde(default)]
    pub external: bool,
    /// Its SOCKS port; `None` finds it on [`PROBE_PORTS`].
    #[serde(default)]
    pub port: Option<u16>,
}

impl TorConfig {
    /// Absent or unreadable, Vector's own Tor: a local port is trusted only once chosen.
    pub fn parse(json: Option<&str>) -> Self {
        json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// The ports to look on, in order.
    pub fn ports(&self) -> Vec<u16> {
        match self.port {
            Some(p) => vec![p],
            None => PROBE_PORTS.to_vec(),
        }
    }
}

pub fn current() -> TorConfig {
    super::prefs::current().config(super::Kind::Tor).downcast_ref::<TorConfig>().cloned().unwrap_or_default()
}

/// Save, then use: a failed save changes nothing.
pub fn set(external: bool, port: Option<u16>) -> Result<TorConfig, String> {
    let lock = super::prefs::config_write_lock();
    let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
    let next = TorConfig { external, port };
    super::prefs::persist_config(super::Kind::Tor, &next.to_json())?;
    super::prefs::set_config(super::Kind::Tor, std::sync::Arc::new(next.clone()));
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tor_config_reads_back_and_defaults_to_vectors_own() {
        assert_eq!(TorConfig::parse(None), TorConfig::default());
        assert_eq!(TorConfig::parse(Some("not json")), TorConfig::default(), "unreadable is the built-in");
        let cfg = TorConfig { external: true, port: Some(9150) };
        assert_eq!(TorConfig::parse(Some(&cfg.to_json())), cfg);
        assert_eq!(cfg.ports(), vec![9150]);
        assert_eq!(TorConfig { external: true, port: None }.ports(), PROBE_PORTS.to_vec());
    }
}
