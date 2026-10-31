//! Deep links: the places a link opens in Vector, in the one grammar every client parses.
//!
//! A place is a path: `profile/<npub>`, `emojis/pack/<naddr>`, `dm/<npub>`,
//! `settings[/<category>[/<section>]]`, `nexus[/<app>]` or `requests`. It rides `vector://<path>`,
//! `https://vectorapp.io/<path>` and Vector Web's own address, `https://web.vectorapp.io/#<path>`.
//! Vector links (golink.rs) and community invites keep their own shapes. A link only opens a
//! place: joining, launching or changing anything there still takes the user's tap.

use serde::Serialize;

use crate::golink::GoLink;

/// Vector Web's own address: a place rides its fragment, which never reaches the server.
pub const WEB_BASE: &str = "https://web.vectorapp.io/#";

/// What a link opens, as the frontend's `executeDeepLinkAction` takes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DeepLinkAction {
    /// profile | emoji_pack | chat | settings | nexus | requests | go | community_invite
    pub action_type: String,
    /// An npub, an naddr, a chat id, `category[/section]`, an app id, a Vector link's payload,
    /// or an invite's whole URL; empty for a place with nothing to name.
    pub target: String,
    /// For "chat": the message to land on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// For "chat": a call answered from a notification, taken up once it rings here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// The npub the action belongs to; dropped if a different account is live when it's taken.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

fn action(action_type: &str, target: impl Into<String>) -> DeepLinkAction {
    DeepLinkAction { action_type: action_type.to_string(), target: target.into(), ..Default::default() }
}

/// The action a URL opens, or `None` when it isn't a Vector deep link.
pub fn parse_deep_link(url: &str) -> Option<DeepLinkAction> {
    let url = url.trim();

    if let Some(link) = GoLink::parse_url(url) {
        return Some(action("go", link.payload()));
    }

    // Invites carry their secret in the fragment, which the path parser strips: the join flow
    // re-parses the whole URL.
    if let Some((before, frag)) = url.split_once('#') {
        if !frag.is_empty() && is_invite_locator(before) {
            return Some(action("community_invite", url));
        }
    }

    let path = url
        .strip_prefix(WEB_BASE)
        .or_else(|| url.strip_prefix("vector://"))
        .or_else(|| url.strip_prefix("https://vectorapp.io/"))
        .or_else(|| url.strip_prefix("http://vectorapp.io/"))?;
    parse_path(path)
}

fn parse_path(path: &str) -> Option<DeepLinkAction> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        ["profile", npub, ..] => valid_npub(npub).then(|| action("profile", *npub)),
        ["dm", npub] => valid_npub(npub).then(|| action("chat", *npub)),
        // `emojis/pack/<naddr>` matches the website's route and the app's share links.
        ["emojis", "pack", naddr, ..] => {
            let naddr = strip_html_suffix(naddr);
            valid_naddr(naddr).then(|| action("emoji_pack", naddr))
        }
        ["settings", rest @ ..] => (rest.len() <= 2 && rest.iter().all(|s| settings_id(s))).then(|| action("settings", rest.join("/"))),
        ["nexus"] => Some(action("nexus", "")),
        ["nexus", app] => crate::webxdc::is_safe_app_id(app).then(|| action("nexus", *app)),
        ["requests"] => Some(action("requests", "")),
        _ => None,
    }
}

/// npub1 and 58 bech32 characters.
fn valid_npub(npub: &str) -> bool {
    npub.starts_with("npub1") && npub.len() == 63
}

/// A plausible naddr: the pack modal decodes it and says so when it can't.
fn valid_naddr(naddr: &str) -> bool {
    naddr.starts_with("naddr1") && naddr.len() >= 30
}

/// A Settings category or section id; an unknown one opens Settings where it would anyway.
fn settings_id(s: &str) -> bool {
    s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The locator half of an invite link (everything before the `#`).
///
/// The bundle coordinate rides as an naddr path segment (`/invite/<naddr>`) and is honoured
/// from ANY host: the naddr+fragment self-authenticates and the domain is never contacted, so
/// an Armada-minted link joins natively. Mirrors the frontend's `isCommunityInviteUrl`.
fn is_invite_locator(before: &str) -> bool {
    match before.rsplit_once("/invite/") {
        Some((_, naddr)) => valid_naddr(strip_html_suffix(naddr.trim_end_matches('/'))),
        None => false,
    }
}

/// Some link expanders and mobile launchers append `.html` to a URL that looks like a file path.
fn strip_html_suffix(s: &str) -> &str {
    s.strip_suffix(".html").unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NADDR: &str = "naddr1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";

    fn npub() -> String {
        format!("npub1{}", "q".repeat(58))
    }

    fn opens(url: &str) -> Option<(String, String)> {
        parse_deep_link(url).map(|a| (a.action_type, a.target))
    }

    #[test]
    fn every_place_opens_from_the_app_scheme_the_website_and_vector_web() {
        let npub = npub();
        let places = [
            (format!("profile/{npub}"), "profile", npub.clone()),
            (format!("dm/{npub}"), "chat", npub.clone()),
            (format!("emojis/pack/{NADDR}"), "emoji_pack", NADDR.to_string()),
            ("settings".to_string(), "settings", String::new()),
            ("settings/network".to_string(), "settings", "network".to_string()),
            ("settings/network/relays".to_string(), "settings", "network/relays".to_string()),
            ("nexus".to_string(), "nexus", String::new()),
            ("nexus/chess.v2_beta".to_string(), "nexus", "chess.v2_beta".to_string()),
            ("requests".to_string(), "requests", String::new()),
        ];
        for (path, kind, target) in places {
            for url in [format!("vector://{path}"), format!("https://vectorapp.io/{path}"), format!("{WEB_BASE}{path}")] {
                assert_eq!(opens(&url), Some((kind.to_string(), target.clone())), "{url}");
            }
        }
    }

    #[test]
    fn a_place_that_names_something_malformed_opens_nothing() {
        for url in [
            "vector://profile/notanpub".to_string(),
            format!("vector://dm/{}/extra", npub()),
            "vector://emojis/pack/naddr1short".to_string(),
            "vector://settings/network/relays/more".to_string(),
            "vector://settings/Network".to_string(),
            "vector://settings/../danger".to_string(),
            "vector://nexus/../../etc".to_string(),
            "vector://nexus/a/b".to_string(),
            "vector://requests/x".to_string(),
            "vector://unknown".to_string(),
            format!("https://evil.example/profile/{}", npub()),
            format!("https://web.vectorapp.io/profile/{}", npub()),
        ] {
            assert_eq!(parse_deep_link(&url), None, "{url}");
        }
    }

    #[test]
    fn a_query_or_fragment_after_a_place_is_ignored() {
        assert_eq!(opens("vector://settings/network?from=docs#top"), Some(("settings".into(), "network".into())));
        assert_eq!(opens(&format!("https://vectorapp.io/emojis/pack/{NADDR}.html")), Some(("emoji_pack".into(), NADDR.into())));
    }

    // A retired fragment-only link is not an invite.
    #[test]
    fn a_fragment_only_invite_link_is_not_an_invite() {
        for url in ["vector://invite#AgEs3q1MZz0", "https://vectorapp.io/invite#AgEs3q1MZz0"] {
            assert!(parse_deep_link(url).map(|a| a.action_type) != Some("community_invite".into()), "{url}");
        }
    }

    // The bundle coordinate rides in the PATH (`/invite/<naddr>#<frag>`), and the community-invite
    // catch runs BEFORE the path parser strips the fragment. Any host counts: the naddr+fragment
    // self-authenticates.
    #[test]
    fn v2_naddr_invite_parses_from_every_host() {
        for url in [
            format!("vector://invite/{NADDR}#BAHs3q1MZz0aBcDeF"),
            format!("https://vectorapp.io/invite/{NADDR}#BAHs3q1MZz0aBcDeF"),
            format!("https://armada.buzz/invite/{NADDR}#BAHs3q1MZz0aBcDeF"),
        ] {
            let action = parse_deep_link(&url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(action.action_type, "community_invite");
            assert_eq!(action.target, url, "join flow re-parses the full URL");
        }
    }

    #[test]
    fn invite_without_fragment_is_not_an_invite() {
        assert!(parse_deep_link("vector://invite").is_none());
    }

    #[test]
    fn invite_path_that_is_not_an_naddr_is_not_an_invite() {
        assert!(parse_deep_link("https://vectorapp.io/invite/npub1abc#frag").is_none());
        assert!(parse_deep_link("https://evil.example/invite/naddr1#frag").is_none());
    }

    #[test]
    fn fragment_on_non_invite_paths_does_not_hijack() {
        assert!(parse_deep_link("vector://profile/notanpub#x").is_none());
        let npub = npub();
        assert_eq!(opens(&format!("vector://profile/{npub}")), Some(("profile".into(), npub)));
    }

    #[test]
    fn a_vector_link_opens_from_every_carrier() {
        let payload = format!("c/{}/{}", "a".repeat(64), "b".repeat(64));
        for url in [format!("https://vectorapp.io/go#{payload}"), format!("vector://go#{payload}"), format!("{WEB_BASE}go/{payload}")] {
            assert_eq!(opens(&url), Some(("go".into(), payload.clone())), "{url}");
        }
        assert!(parse_deep_link("https://vectorapp.io/go#c/not-an-id").is_none());
        assert!(parse_deep_link("https://vectorapp.io/go").is_none());
    }
}
