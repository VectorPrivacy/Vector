//! Deep Link Handler Module
//!
//! This module handles parsing and processing of deep link URLs for Vector.
//! Supported URL formats:
//! - `vector://profile/<npub>` - Opens a user's profile
//! - `vector://emojis/pack/<naddr>` - Opens the Pack Details modal
//! - `https://vectorapp.io/profile/<npub>` - Web URL for mobile app links
//! - `https://vectorapp.io/emojis/pack/<naddr>` - Web URL for pack share links

use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime};

/// Global storage for pending deep link action (received before frontend is ready)
static PENDING_DEEP_LINK: Mutex<Option<DeepLinkAction>> = Mutex::new(None);

/// Represents a parsed deep link action to be sent to the frontend
#[derive(Debug, Clone, Serialize)]
pub struct DeepLinkAction {
    /// The type of action: "profile"
    pub action_type: String,
    /// The target identifier (npub)
    pub target: String,
}

/// Parse a deep link URL and return the action to perform
///
/// Supports both custom scheme URLs (vector://) and web URLs (https://vectorapp.io/)
///
/// # Arguments
/// * `url_str` - The URL string to parse
///
/// # Returns
/// * `Some(DeepLinkAction)` if the URL is valid and recognized
/// * `None` if the URL is invalid or not a recognized deep link
pub fn parse_deep_link(url_str: &str) -> Option<DeepLinkAction> {
    // Normalize the URL for parsing
    let url_str = url_str.trim();

    // Community invites carry secrets in the URL FRAGMENT (#…), which the path parsers below
    // strip. Catch them first and pass the whole URL through — the join flow re-parses it.
    if let Some((before, frag)) = url_str.split_once('#') {
        if !frag.is_empty() && is_invite_locator(before) {
            return Some(DeepLinkAction {
                action_type: "community_invite".to_string(),
                target: url_str.to_string(),
            });
        }
    }

    // Handle vector:// scheme
    if url_str.starts_with("vector://") {
        return parse_vector_scheme(url_str);
    }
    
    // Handle https://vectorapp.io/ URLs (for mobile app links)
    if url_str.starts_with("https://vectorapp.io/") || url_str.starts_with("http://vectorapp.io/") {
        return parse_web_url(url_str);
    }
    
    None
}

/// Parse a vector:// scheme URL
fn parse_vector_scheme(url_str: &str) -> Option<DeepLinkAction> {
    // Remove the scheme prefix
    let path = url_str.strip_prefix("vector://")?;
    parse_path_segments(path)
}

/// Parse a web URL (https://vectorapp.io/...)
fn parse_web_url(url_str: &str) -> Option<DeepLinkAction> {
    // Extract the path from the URL using simple string manipulation
    // We know the URL starts with https://vectorapp.io/ or http://vectorapp.io/
    let path = if url_str.starts_with("https://vectorapp.io/") {
        url_str.strip_prefix("https://vectorapp.io/")?
    } else {
        url_str.strip_prefix("http://vectorapp.io/")?
    };
    // Remove any query string or fragment
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);
    parse_path_segments(path)
}

/// Parse path segments and return the appropriate action
fn parse_path_segments(path: &str) -> Option<DeepLinkAction> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    
    if segments.is_empty() {
        return None;
    }
    
    match segments[0] {
        "profile" if segments.len() >= 2 => {
            let npub = segments[1];
            if validate_npub(npub) {
                Some(DeepLinkAction {
                    action_type: "profile".to_string(),
                    target: npub.to_string(),
                })
            } else {
                println!("[DeepLink] Invalid npub format: {}", npub);
                None
            }
        }
        // Pack share links — `emojis/pack/<naddr>` matches the website
        // route + the Vector app's `_sharePackToClipboard` output. Naddr
        // decoding happens on the frontend; we only sanity-check the
        // bech32 HRP + min length so a typo doesn't queue an action.
        "emojis" if segments.len() >= 3 && segments[1] == "pack" => {
            let naddr = strip_html_suffix(segments[2]);
            if validate_naddr(naddr) {
                Some(DeepLinkAction {
                    action_type: "emoji_pack".to_string(),
                    target: naddr.to_string(),
                })
            } else {
                println!("[DeepLink] Invalid naddr format: {}", naddr);
                None
            }
        }
        _ => {
            println!("[DeepLink] Unknown action: {}", segments[0]);
            None
        }
    }
}

/// Validate that a string is a valid npub (Nostr public key in bech32 format)
fn validate_npub(npub: &str) -> bool {
    // npub1 prefix + 58 characters of bech32 data = 63 total characters
    npub.starts_with("npub1") && npub.len() == 63
}

/// Lightweight naddr sanity check. Real bech32 + TLV decoding happens
/// on the frontend (vector-core has the helpers, but the parser doesn't
/// need to load them just to gate a deep link). We just verify the
/// bech32 HRP and a plausible minimum length; the frontend modal will
/// surface a friendly error if decoding fails downstream.
fn validate_naddr(naddr: &str) -> bool {
    naddr.starts_with("naddr1") && naddr.len() >= 30
}

/// The locator half of an invite link (everything before the `#`).
///
/// A v1 link keeps its whole payload in the fragment, so the path just ends at `/invite`.
/// A v2 link carries the bundle coordinate as an naddr path segment (`/invite/<naddr>`) and is
/// honoured from ANY host: the naddr+fragment self-authenticates and the domain is never
/// contacted, so an Armada-minted link joins natively. Mirrors the frontend's
/// `isCommunityInviteUrl`.
fn is_invite_locator(before: &str) -> bool {
    if before.ends_with("/invite") {
        return true;
    }
    match before.rsplit_once("/invite/") {
        Some((_, naddr)) => validate_naddr(strip_html_suffix(naddr.trim_end_matches('/'))),
        None => false,
    }
}

/// Some link expanders / mobile launchers append `.html` to a URL that
/// looks like a file path. Strip it so the naddr decodes cleanly.
fn strip_html_suffix(s: &str) -> &str {
    s.strip_suffix(".html").unwrap_or(s)
}

/// Handle incoming deep link URLs
///
/// This function parses the URLs, stores them for later retrieval, and emits events to the frontend.
/// It should be called when the app receives deep link URLs from the OS.
///
/// # Arguments
/// * `handle` - The Tauri app handle
/// * `urls` - A vector of URL strings to process
pub fn handle_deep_link<R: Runtime>(handle: &AppHandle<R>, urls: Vec<String>) {
    for url in urls {
        println!("[DeepLink] Received URL: {}", url);
        
        if let Some(action) = parse_deep_link(&url) {
            println!("[DeepLink] Parsed action: {:?}", action);
            
            // Store the action for later retrieval (in case frontend isn't ready yet)
            if let Ok(mut pending) = PENDING_DEEP_LINK.lock() {
                *pending = Some(action.clone());
                println!("[DeepLink] Stored pending action for later retrieval");
            }
            
            // Also emit event to frontend (in case it's already listening)
            if let Err(e) = handle.emit("deep_link_action", &action) {
                println!("[DeepLink] Failed to emit event: {:?}", e);
            }
        } else {
            println!("[DeepLink] Failed to parse URL: {}", url);
        }
    }
}

/// Store a pending notification tap action (called from Android JNI when user taps a notification)
#[allow(dead_code)]
pub fn set_pending_notification_action(chat_id: &str) {
    if let Ok(mut pending) = PENDING_DEEP_LINK.lock() {
        *pending = Some(DeepLinkAction {
            action_type: "chat".to_string(),
            target: chat_id.to_string(),
        });
        println!("[DeepLink] Stored pending notification action for chat: {}", &chat_id[..chat_id.len().min(20)]);
    }
}

/// Get and clear any pending deep link action
///
/// This should be called by the frontend after login to check if there's a pending
/// deep link action that was received before the frontend was ready.
///
/// # Returns
/// * `Some(DeepLinkAction)` if there was a pending action (clears it)
/// * `None` if there was no pending action
#[tauri::command]
pub fn get_pending_deep_link() -> Option<DeepLinkAction> {
    if let Ok(mut pending) = PENDING_DEEP_LINK.lock() {
        let action = pending.take();
        if action.is_some() {
            println!("[DeepLink] Retrieved and cleared pending action");
        }
        action
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The community-invite catch runs BEFORE the path parsers strip the
    // fragment — every URL shape must survive intact.
    #[test]
    fn invite_fragment_parses_from_both_url_shapes() {
        for url in [
            "vector://invite#AgEs3q1MZz0",
            "https://vectorapp.io/invite#AgEs3q1MZz0",
        ] {
            let action = parse_deep_link(url).expect(url);
            assert_eq!(action.action_type, "community_invite");
            assert_eq!(action.target, url, "join flow re-parses the full URL");
        }
    }

    // A v2 link puts the bundle coordinate in the PATH (`/invite/<naddr>#<frag>`), so it never
    // ends with "/invite". Any host counts: the naddr+fragment self-authenticates.
    const NADDR: &str = "naddr1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";

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
        assert!(parse_deep_link("vector://invite#").is_none());
        assert!(parse_deep_link(&format!("https://vectorapp.io/invite/{NADDR}")).is_none());
    }

    // A path segment that isn't an naddr must not be mistaken for a v2 coordinate.
    #[test]
    fn invite_path_that_is_not_an_naddr_is_not_an_invite() {
        assert!(parse_deep_link("https://vectorapp.io/invite/npub1abc#frag").is_none());
        assert!(parse_deep_link("https://evil.example/invite/naddr1#frag").is_none());
    }

    #[test]
    fn fragment_on_non_invite_paths_does_not_hijack() {
        // A fragment elsewhere must not classify as an invite, and the
        // legitimate profile shape must keep working.
        assert!(parse_deep_link("vector://profile/notanpub#x").is_none());
        let npub = format!("npub1{}", "q".repeat(58));
        let action = parse_deep_link(&format!("vector://profile/{npub}")).unwrap();
        assert_eq!(action.action_type, "profile");
        assert_eq!(action.target, npub);
    }
}