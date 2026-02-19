//! Deep Link Handler Module
//!
//! This module handles parsing and processing of deep link URLs for Vector.
//! Supported URL formats:
//! - `vector://profile/<npub>` - Opens a user's profile
//! - `https://vectorapp.io/profile/<npub>` - Web URL for mobile app links

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