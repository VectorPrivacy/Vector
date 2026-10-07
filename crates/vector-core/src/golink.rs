//! Vector links: a community, a channel or a message, named by the ids its members already hold.
//!
//! `https://vectorapp.io/go#c/<community>[/<channel>[/<message>]]`, or `#m/<message>` for a DM
//! message (each side files a DM under the other person's npub, so only the message id is shared).
//! The ids ride the fragment, which a browser never sends to a server, and a link resolves only
//! against the device's own chats.

/// The shareable form; `vector://go#…` and Vector Web's `/#go/…` carry the same payload.
pub const LINK_BASE: &str = "https://vectorapp.io/go#";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoLink {
    Community { community: String },
    Channel { community: String, channel: String },
    ChannelMessage { community: String, channel: String, message: String },
    Message { message: String },
}

fn id(s: &str) -> Option<String> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

impl GoLink {
    /// `c/<community>[/<channel>[/<message>]]` or `m/<message>`.
    pub fn parse_payload(payload: &str) -> Option<Self> {
        let mut parts = payload.trim_end_matches('/').split('/');
        let link = match parts.next()? {
            "c" => {
                let community = id(parts.next()?)?;
                match (parts.next().map(id), parts.next().map(id)) {
                    (None, _) => GoLink::Community { community },
                    (Some(channel), None) => GoLink::Channel { community, channel: channel? },
                    (Some(channel), Some(message)) => GoLink::ChannelMessage { community, channel: channel?, message: message? },
                }
            }
            "m" => GoLink::Message { message: id(parts.next()?)? },
            _ => return None,
        };
        parts.next().is_none().then_some(link)
    }

    /// Any of the three forms: the shareable link, the app scheme, or Vector Web's own address.
    pub fn parse_url(url: &str) -> Option<Self> {
        let (locator, payload) = url.trim().split_once('#')?;
        let locator = locator.trim_end_matches('/');
        let payload = match locator {
            "https://vectorapp.io/go" | "http://vectorapp.io/go" | "https://www.vectorapp.io/go" | "vector://go" => payload,
            "https://web.vectorapp.io" => payload.strip_prefix("go/")?,
            _ => return None,
        };
        Self::parse_payload(payload)
    }

    pub fn payload(&self) -> String {
        match self {
            GoLink::Community { community } => format!("c/{community}"),
            GoLink::Channel { community, channel } => format!("c/{community}/{channel}"),
            GoLink::ChannelMessage { community, channel, message } => format!("c/{community}/{channel}/{message}"),
            GoLink::Message { message } => format!("m/{message}"),
        }
    }

    pub fn url(&self) -> String {
        format!("{LINK_BASE}{}", self.payload())
    }
}

/// Whether a URL is a Vector link: never fetched for a preview, since it names nothing a web
/// page could show and the request alone would tell the server one was opened.
pub fn is_go_url(url: &str) -> bool {
    let locator = url.split(['#', '?']).next().unwrap_or("").trim_end_matches('/');
    matches!(locator, "https://vectorapp.io/go" | "http://vectorapp.io/go" | "https://www.vectorapp.io/go")
        || url.starts_with("https://web.vectorapp.io/#go/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn every_shape_round_trips_through_its_url() {
        let links = [
            GoLink::Community { community: A.into() },
            GoLink::Channel { community: A.into(), channel: B.into() },
            GoLink::ChannelMessage { community: A.into(), channel: B.into(), message: C.into() },
            GoLink::Message { message: C.into() },
        ];
        for link in links {
            assert_eq!(GoLink::parse_url(&link.url()), Some(link.clone()));
            assert_eq!(GoLink::parse_url(&format!("vector://go#{}", link.payload())), Some(link.clone()));
            assert_eq!(GoLink::parse_url(&format!("https://web.vectorapp.io/#go/{}", link.payload())), Some(link.clone()));
        }
    }

    #[test]
    fn ids_must_be_whole_hex_and_shapes_exact() {
        let upper = format!("https://vectorapp.io/go#c/{}", A.to_uppercase());
        assert_eq!(GoLink::parse_url(&upper), Some(GoLink::Community { community: A.into() }), "case folds");
        for bad in [
            format!("https://vectorapp.io/go#c/{}", &A[1..]),
            format!("https://vectorapp.io/go#c/{A}/{B}/{C}/{A}"),
            format!("https://vectorapp.io/go#m/{A}/{B}"),
            format!("https://vectorapp.io/go#x/{A}"),
            format!("https://vectorapp.io/go#c/{}g", &A[1..]),
            format!("https://evil.example/go#c/{A}"),
            format!("https://vectorapp.io/invite#c/{A}"),
            format!("https://web.vectorapp.io/#c/{A}"),
        ] {
            assert_eq!(GoLink::parse_url(&bad), None, "{bad}");
        }
    }

    #[test]
    fn only_vector_links_skip_previews() {
        assert!(is_go_url(&format!("https://vectorapp.io/go#c/{A}")));
        assert!(is_go_url("https://vectorapp.io/go"));
        assert!(is_go_url(&format!("https://web.vectorapp.io/#go/m/{A}")));
        assert!(!is_go_url("https://vectorapp.io/"));
        assert!(!is_go_url("https://vectorapp.io/gopher"));
        assert!(!is_go_url("https://example.com/go#c/x"));
    }
}
