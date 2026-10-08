//! The registry: a config codec for every kind (compiled always) and a factory for every compiled
//! kind. One line per kind.

use std::sync::Arc;

use super::{Kind, KindConfig, TransportFactory};

/// A kind's stored config, parsed into its own type. Absent JSON is its default; a row that
/// can't be read takes the kind's strict reading.
pub fn decode(kind: Kind, json: Option<&str>) -> KindConfig {
    match kind {
        Kind::I2p => Arc::new(super::i2p_config::I2pConfig::parse(json)),
        Kind::Clearnet | Kind::Tor => Arc::new(()),
    }
}

/// A kind's config when its row exists but the read itself failed.
pub fn decode_unreadable(kind: Kind) -> KindConfig {
    match kind {
        Kind::I2p => Arc::new(super::i2p_config::I2pConfig::unreadable()),
        Kind::Clearnet | Kind::Tor => Arc::new(()),
    }
}

/// The stricter of a session's live config and a fresh reading of the same account's row.
pub fn stricter(kind: Kind, live: &KindConfig, stored: &KindConfig) -> KindConfig {
    use super::i2p_config::I2pConfig;
    match (kind, live.downcast_ref::<I2pConfig>(), stored.downcast_ref::<I2pConfig>()) {
        (Kind::I2p, Some(l), Some(s)) => Arc::new(l.stricter(s)),
        _ => live.clone(),
    }
}

pub fn factory(kind: Kind) -> Option<&'static dyn TransportFactory> {
    match kind {
        #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
        Kind::Tor => Some(&crate::tor::TorFactory),
        #[cfg(all(feature = "i2p", not(target_arch = "wasm32")))]
        Kind::I2p => Some(&crate::i2p::I2pFactory),
        _ => None,
    }
}
