//! Host classification and the routing rules: one pre-router shared by every kind, then each
//! kind's own pure rules.

use super::{ExitPolicy, Kind, RouteCtx};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Dest {
    /// Lowercase, trailing dot stripped. A name that parses as an IP is an `Ip`.
    Domain(String),
    Ip(std::net::IpAddr),
}

impl Dest {
    /// `None` only for an empty host. Everything else classifies, untrimmed: validity is
    /// `pre_route`'s call, so stray whitespace or CR/LF is a bad name, never a different host.
    pub fn parse(host: &str) -> Option<Dest> {
        let h = host.trim_start_matches('[').trim_end_matches(']');
        if h.is_empty() {
            return None;
        }
        if let Ok(ip) = h.parse::<std::net::IpAddr>() {
            return Some(Dest::Ip(ip));
        }
        let lower = h.to_ascii_lowercase();
        let name = lower.strip_suffix('.').unwrap_or(&lower);
        if name.is_empty() {
            return None;
        }
        if let Ok(ip) = name.parse::<std::net::IpAddr>() {
            return Some(Dest::Ip(ip));
        }
        Some(Dest::Domain(name.to_string()))
    }

    pub fn host(&self) -> String {
        match self {
            Dest::Domain(d) => d.clone(),
            Dest::Ip(ip) => ip.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// `.i2p` via SAM; `.onion` via arti.
    Native { host: String, port: u16 },
    /// TLS passthrough to an alias; port is always 443.
    Twin { host: String, via: String, port: u16 },
    /// Tor exit; I2P outproxy.
    Exit { host: String, port: u16 },
    Refuse(Refusal),
}

impl Route {
    pub fn class(&self) -> &'static str {
        match self {
            Route::Native { .. } => "native",
            Route::Twin { .. } => "twin",
            Route::Exit { .. } => "exit",
            Route::Refuse(_) => "refused",
        }
    }

    pub fn host(&self) -> Option<&str> {
        match self {
            Route::Native { host, .. } | Route::Twin { host, .. } | Route::Exit { host, .. } => Some(host),
            Route::Refuse(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    WrongNetwork { needs: Kind },
    /// A name native to a network whose own addresses this build can't reach (`.onion`).
    NotReachable { network: Kind },
    ExitOff,
    IpLiteral,
    LocalName,
    BadName,
}

impl Refusal {
    pub fn text(&self) -> String {
        match self {
            Refusal::WrongNetwork { needs } => format!("This server is only reachable over {}.", needs.label()),
            Refusal::NotReachable { network } => {
                format!("Vector can't reach {} addresses yet.", network.native_suffixes().first().copied().unwrap_or("these"))
            }
            Refusal::ExitOff => "I2P-Only is on, so this server is off.".into(),
            Refusal::IpLiteral => "I2P can't reach a bare IP address.".into(),
            Refusal::LocalName => "Local addresses aren't reachable over I2P.".into(),
            Refusal::BadName => "That isn't a valid server address.".into(),
        }
    }
}

/// 1..=253 bytes, labels 1..=63 of `[a-z0-9-_]`: no CR/LF, spaces, ':' or '/' can reach a
/// CONNECT line or a SOCKS request.
pub fn valid_hostname(h: &str) -> bool {
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    h.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    })
}

/// A `.b32.i2p` address: 52 base32 chars (b32), or 56 and more (b33, encrypted LeaseSets).
pub fn is_b32(h: &str) -> bool {
    let Some(label) = h.strip_suffix(".b32.i2p") else { return false };
    (label.len() == 52 || label.len() >= 56) && label.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// A v3 onion address: 56 base32 characters.
pub fn is_onion(h: &str) -> bool {
    let Some(label) = h.strip_suffix(".onion") else { return false };
    label.len() == 56 && label.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

pub fn is_local_name(h: &str) -> bool {
    h == "localhost"
        || !h.contains('.')
        || [".localhost", ".local", ".lan", ".internal", ".home.arpa"].iter().any(|s| h.ends_with(s))
}

fn has_suffix(name: &str, suffix: &str) -> bool {
    name.ends_with(suffix) || name == &suffix[1..]
}

/// Shared by every kind, Clearnet included: a name native to another kind never reaches an
/// exit, an outproxy or a DNS resolver.
pub fn pre_route(chosen: Kind, dest: &Dest) -> Option<Refusal> {
    let Dest::Domain(name) = dest else { return None };
    if !valid_hostname(name) {
        return Some(Refusal::BadName);
    }
    for k in Kind::ALL {
        if k.native_suffixes().iter().any(|s| has_suffix(name, s)) {
            if !k.reaches_native() {
                return Some(Refusal::NotReachable { network: k });
            }
            if k != chosen {
                return Some(Refusal::WrongNetwork { needs: k });
            }
        }
    }
    None
}

fn twin_for(dest: &Dest, port: u16, kind: Kind, ctx: &RouteCtx) -> Option<Route> {
    let Dest::Domain(name) = dest else { return None };
    if port != 443 {
        return None;
    }
    ctx.aliases
        .twin(name, kind)
        .map(|via| Route::Twin { host: name.clone(), via: via.to_string(), port: 443 })
}

pub fn route_tor(dest: &Dest, port: u16, ctx: &RouteCtx) -> Route {
    if let Dest::Domain(name) = dest {
        if Kind::Tor.native_suffixes().iter().any(|s| has_suffix(name, s)) {
            return Route::Native { host: name.clone(), port };
        }
    }
    if let Some(twin) = twin_for(dest, port, Kind::Tor, ctx) {
        return twin;
    }
    Route::Exit { host: dest.host(), port }
}

pub fn route_i2p(dest: &Dest, port: u16, ctx: &RouteCtx) -> Route {
    if let Dest::Domain(name) = dest {
        if Kind::I2p.native_suffixes().iter().any(|s| has_suffix(name, s)) {
            return Route::Native { host: name.clone(), port };
        }
    }
    if let Some(twin) = twin_for(dest, port, Kind::I2p, ctx) {
        return twin;
    }
    match dest {
        Dest::Ip(_) => Route::Refuse(Refusal::IpLiteral),
        Dest::Domain(name) if is_local_name(name) => Route::Refuse(Refusal::LocalName),
        Dest::Domain(name) => {
            let exit = ctx
                .config
                .downcast_ref::<super::i2p_config::I2pConfig>()
                .map(|c| c.exit)
                .unwrap_or_default();
            match exit {
                ExitPolicy::Allow => Route::Exit { host: name.clone(), port },
                ExitPolicy::Off => Route::Refuse(Refusal::ExitOff),
            }
        }
    }
}

const RELAY_SCHEME: &str = "Use wss://, or ws:// for an .i2p or .onion address.";

/// A relay URL the app can keep: `wss://` for any host, plain `ws://` only for an `.i2p` or
/// `.onion` host, where the network itself encrypts end to end. Returns the URL without a
/// trailing slash.
pub fn validate_relay_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    let (rest, plain) = match (trimmed.strip_prefix("wss://"), trimmed.strip_prefix("ws://")) {
        (Some(r), _) => (r, false),
        (None, Some(r)) => (r, true),
        _ => return Err(RELAY_SCHEME.into()),
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err("Relay URL must include a host".into());
    }
    if plain {
        let host = url::Url::parse(trimmed).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase));
        if !host.is_some_and(|h| (h.ends_with(".i2p") && h.len() > 4) || is_onion(&h)) {
            return Err(RELAY_SCHEME.into());
        }
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}
