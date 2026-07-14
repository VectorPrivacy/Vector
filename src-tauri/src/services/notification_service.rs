//! OS notification service for the Vector application.
//!
//! This module provides a unified notification system that handles:
//! - Direct message notifications
//! - Group message notifications
//! - Group invite notifications
//!
//! Notifications are shown only when the app is not focused, and include
//! platform-specific handling for Android vs desktop.

#[cfg(not(target_os = "android"))]
use tauri::Manager;
#[cfg(not(target_os = "android"))]
use tauri_plugin_notification::NotificationExt;

#[cfg(not(target_os = "android"))]
use crate::audio;
#[cfg(not(target_os = "android"))]
use crate::TAURI_APP;

/// Notification type enum for different kinds of notifications
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NotificationType {
    DirectMessage,
    CommunityMessage,
}

/// How much of a message to reveal in the OS notification. Per-account setting
/// (`notif_content_privacy`), read at notify time so it applies on every path
/// including the Android background-sync service.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NotifContentPrivacy {
    /// Sender + message (default).
    Full,
    /// Sender shown, message hidden.
    HideContent,
    /// Generic "you received a message" — no sender, avatar, or group.
    HideAll,
}

/// Read the account's notification content-privacy preference. Defaults to
/// `Full` when unset or unreadable (matches historical behavior).
pub fn notif_content_privacy() -> NotifContentPrivacy {
    match crate::db::get_sql_setting("notif_content_privacy".to_string())
        .ok()
        .flatten()
        .as_deref()
    {
        Some("hide_content") => NotifContentPrivacy::HideContent,
        Some("hide_all") => NotifContentPrivacy::HideAll,
        _ => NotifContentPrivacy::Full,
    }
}

/// Generic notification data structure
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct NotificationData {
    pub notification_type: NotificationType,
    pub title: String,
    pub body: String,
    /// Optional group name for group-related notifications
    pub group_name: Option<String>,
    /// Optional sender name
    pub sender_name: Option<String>,
    /// Optional cached avatar file path for the sender
    pub avatar_path: Option<String>,
    /// Optional cached avatar file path for the group (community channels only)
    pub group_avatar_path: Option<String>,
    /// Chat identifier for notification tap navigation (npub for DMs, group_id for groups)
    pub chat_id: Option<String>,
}

impl NotificationData {
    /// Create a DM notification (works for both text and file attachments)
    pub fn direct_message(sender_name: String, content: String, avatar_path: Option<String>, chat_id: String) -> Self {
        Self {
            notification_type: NotificationType::DirectMessage,
            title: sender_name.clone(),
            body: content,
            group_name: None,
            sender_name: Some(sender_name),
            avatar_path,
            group_avatar_path: None,
            chat_id: Some(chat_id),
        }
    }

    /// Create a Community channel notification. Title mirrors the group format ("sender - community");
    /// `chat_id` is the channel id so tapping navigates to the channel.
    pub fn community_message(
        sender_name: String,
        community_name: String,
        content: String,
        avatar_path: Option<String>,
        community_avatar_path: Option<String>,
        chat_id: String,
    ) -> Self {
        Self {
            notification_type: NotificationType::CommunityMessage,
            title: format!("{} - {}", sender_name, community_name),
            body: content,
            group_name: Some(community_name),
            sender_name: Some(sender_name),
            avatar_path,
            group_avatar_path: community_avatar_path,
            chat_id: Some(chat_id),
        }
    }

    /// Rewrite the notification's visible fields per the content-privacy
    /// preference. `chat_id` is left intact so tap-to-open still works (it is
    /// not shown). Idempotent.
    pub fn apply_content_privacy(&mut self, privacy: NotifContentPrivacy) {
        match privacy {
            NotifContentPrivacy::Full => {}
            NotifContentPrivacy::HideContent => {
                // Keep sender (title) + avatar; replace the body only.
                self.body = if self.group_name.is_some() {
                    "Sent a message".to_string()
                } else {
                    "Sent you a message".to_string()
                };
            }
            NotifContentPrivacy::HideAll => {
                self.title = "Vector".to_string();
                self.body = "You received a message".to_string();
                self.sender_name = None;
                self.avatar_path = None;
                self.group_name = None;
                self.group_avatar_path = None;
            }
        }
    }
}

/// Strip HTML tags and markdown formatting from message content for notification previews.
/// Returns clean plaintext suitable for OS notifications.
///
/// Used at notification call sites in `event_handler.rs` and `subscription_handler.rs`
/// to clean content *after* mention resolution but *before* passing to OS notification APIs.
pub fn strip_content_for_preview(text: &str) -> String {
    // Replace <br> variants with space before tag stripping (so we don't lose line breaks)
    let text = text.replace("<br>", " ").replace("<br/>", " ").replace("<br />", " ")
                   .replace("<BR>", " ").replace("<BR/>", " ").replace("<BR />", " ");

    // Strip remaining HTML tags: skip chars between '<' and '>'
    // Only enter tag mode when '<' is followed by a letter, '/' or '!' to avoid
    // false positives on math expressions like "3 < 5 > 2"
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' && !in_tag {
            if let Some(&next) = chars.peek() {
                if next.is_ascii_alphabetic() || next == '/' || next == '!' {
                    in_tag = true;
                    continue;
                }
            }
            result.push(ch);
        } else if ch == '>' && in_tag {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }

    let text = result;
    let mut result = String::with_capacity(text.len());

    // Process line by line for block-level markdown
    for line in text.lines() {
        let trimmed = line.trim();
        // Skip code fences
        if trimmed.starts_with("```") {
            continue;
        }
        // Skip horizontal rules
        if trimmed.chars().all(|c| c == '-' || c == ' ') && trimmed.matches('-').count() >= 3 {
            continue;
        }
        if trimmed.chars().all(|c| c == '*' || c == ' ') && trimmed.matches('*').count() >= 3 && !trimmed.contains("**") {
            continue;
        }

        let mut line_text = trimmed.to_string();

        // Strip header prefixes
        if line_text.starts_with('#') {
            line_text = line_text.trim_start_matches('#').trim_start().to_string();
        }
        // Strip blockquote prefixes
        if line_text.starts_with('>') {
            line_text = line_text[1..].trim_start().to_string();
        }

        if !result.is_empty() && !line_text.is_empty() {
            result.push(' ');
        }
        result.push_str(&line_text);
    }

    // Strip inline formatting markers
    // Bold **text** or __text__
    let result = result.replace("**", "").replace("__", "");
    // Strikethrough ~~text~~
    let result = result.replace("~~", "");
    // Spoiler ||text|| → replace hidden content with ▮▮▮
    // split("||") yields: [before, spoiler_content, after, spoiler_content, after, ...]
    // After consuming the first segment (before any ||), odd segments are spoiler content.
    let mut final_result = String::with_capacity(result.len());
    let mut parts = result.split("||");
    if let Some(first) = parts.next() {
        final_result.push_str(first);
    }
    let mut inside_spoiler = true;
    for part in parts {
        if inside_spoiler {
            final_result.push_str("▮▮▮");
        } else {
            final_result.push_str(part);
        }
        inside_spoiler = !inside_spoiler;
    }
    // Strip inline code backticks
    let final_result = final_result.replace('`', "");

    // Collapse whitespace and trim
    let mut collapsed = String::with_capacity(final_result.len());
    let mut last_was_space = false;
    for ch in final_result.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                collapsed.push(' ');
                last_was_space = true;
            }
        } else {
            collapsed.push(ch);
            last_was_space = false;
        }
    }
    collapsed.trim().to_string()
}

/// Replace mention tokens in message content with `@DisplayName`, matching what
/// the in-app renderer treats as a mention so the notification preview reads the
/// same as the chat. Recognises all three forms Vector emits: `@npub1…`,
/// `nostr:npub1…` (NIP-21), and a bare `npub1…`. Prioritises nickname > name;
/// an unknown npub (no name found) is left verbatim.
pub fn resolve_mention_display_names(content: &str, state: &crate::state::ChatState) -> String {
    resolve_mentions_with(content, |npub| {
        let p = state.get_profile(npub)?;
        if !p.nickname.is_empty() {
            Some(p.nickname.to_string())
        } else if !p.name.is_empty() {
            Some(p.name.to_string())
        } else {
            None
        }
    })
}

/// The pure scanner behind [`resolve_mention_display_names`]. `lookup` maps a
/// bare npub to its display name (`None` = unknown, leave the token untouched).
///
/// Operates on `&str` slices to stay UTF-8 safe — npub1 + 58 bech32 chars are
/// always ASCII, so we anchor on byte offsets only within the ASCII portion and
/// copy surrounding text (which may contain emoji / multibyte) via `&content[..]`.
fn resolve_mentions_with<F: Fn(&str) -> Option<String>>(content: &str, lookup: F) -> String {
    const NPUB_LEN: usize = 63; // "npub1" (5) + 58 bech32 chars
    const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len);
    let mut cursor = 0; // byte offset of uncopied content

    // Anchor on the npub itself, then swallow whatever mention prefix precedes it
    // (`@` or `nostr:`) so the whole token collapses to a single @name.
    let mut i = 0;
    while i + NPUB_LEN <= len {
        let is_npub = &bytes[i..i + 5] == b"npub1"
            && bytes[i + 5..i + NPUB_LEN]
                .iter()
                .all(|b| BECH32.contains(&b.to_ascii_lowercase()));
        if is_npub {
            let npub_end = i + NPUB_LEN;
            if let Some(name) = lookup(&content[i..npub_end]) {
                let mstart = if i >= 1 && bytes[i - 1] == b'@' {
                    i - 1
                } else if i >= 6 && &bytes[i - 6..i] == b"nostr:" {
                    i - 6
                } else {
                    i
                };
                result.push_str(&content[cursor..mstart]);
                result.push('@');
                result.push_str(&name);
                cursor = npub_end;
            }
            // Unknown npub: leave the token (and its prefix) verbatim — it stays
            // in `content[cursor..]` and is copied by a later match or the tail.
            i = npub_end;
            continue;
        }
        i += 1;
    }

    // Append remaining content after last match (or entire string if no matches)
    result.push_str(&content[cursor..]);
    result
}

#[cfg(test)]
mod mention_tests {
    use super::resolve_mentions_with;

    #[test]
    fn resolves_at_nostr_and_bare_npub_forms() {
        let npub = format!("npub1{}", "q".repeat(58));
        let unknown = format!("npub1{}", "p".repeat(58));
        let known: &str = &npub;
        let lookup = |n: &str| (n == known).then(|| "Alice".to_string());

        // All three mention forms collapse to @DisplayName.
        assert_eq!(resolve_mentions_with(&format!("hey @{npub}!"), lookup), "hey @Alice!");
        assert_eq!(resolve_mentions_with(&format!("hey nostr:{npub}!"), lookup), "hey @Alice!");
        assert_eq!(resolve_mentions_with(&format!("hey {npub}!"), lookup), "hey @Alice!");

        // No name found → the raw token (prefix included) is left verbatim.
        assert_eq!(
            resolve_mentions_with(&format!("hi nostr:{unknown}"), lookup),
            format!("hi nostr:{unknown}")
        );

        // Mixed forms, multiple mentions, surrounding text preserved.
        assert_eq!(
            resolve_mentions_with(&format!("{npub} and nostr:{npub} done"), lookup),
            "@Alice and @Alice done"
        );

        // No mentions at all: content passes through unchanged.
        assert_eq!(resolve_mentions_with("just a normal line", lookup), "just a normal line");
    }
}

/// Revoke the OS notification for a chat once it's been read (opened in-app) or answered on
/// another device. Android: cancels the per-chat notification via JNI (no-op if none is showing).
/// Desktop: no-op (desktop notifications aren't persistent or handle-tracked).
pub fn cancel_chat_notification(chat_id: &str) {
    #[cfg(target_os = "android")]
    crate::android::background_sync::cancel_notification_jni(chat_id);

    #[cfg(not(target_os = "android"))]
    let _ = chat_id;
}

/// Show an OS notification with generic notification data
pub fn show_notification_generic(mut data: NotificationData) {
    // Apply the user's content-privacy preference up front so every platform
    // path inherits it. Android's background-sync service posts straight to
    // post_notification_jni, which re-applies it (the transform is idempotent).
    data.apply_content_privacy(notif_content_privacy());

    // On Android, always use our native JNI notification path.
    // Tauri's notification plugin is unreliable on Android (requires Activity).
    // post_notification_jni checks is_activity_in_foreground() to suppress
    // notifications when the user is actively using the app.
    #[cfg(target_os = "android")]
    {
        crate::android::background_sync::post_notification_jni(
            &data.title,
            &data.body,
            data.avatar_path.as_deref(),
            data.chat_id.as_deref(),
            data.sender_name.as_deref(),
            data.group_name.as_deref(),
            data.group_avatar_path.as_deref(),
        );
        return;
    }

    #[cfg(not(target_os = "android"))]
    {
        let handle = match TAURI_APP.get() {
            Some(h) => h,
            None => return,
        };

        // Check if the app is focused — skip notification if user is looking at it
        let is_focused = handle
            .webview_windows()
            .iter()
            .next()
            .and_then(|(_, w)| w.is_focused().ok())
            .unwrap_or(false);

        if is_focused {
            return;
        }

        // Play notification sound (non-blocking)
        #[cfg(desktop)]
        {
            let handle_clone = handle.clone();
            std::thread::spawn(move || {
                if let Err(e) = audio::play_notification_if_enabled(&handle_clone) {
                    eprintln!("Failed to play notification sound: {}", e);
                }
            });
        }

        handle
            .notification()
            .builder()
            .title(&data.title)
            .body(&data.body)
            .large_body(&data.body)
            .show()
            .unwrap_or_else(|e| eprintln!("Failed to send notification: {}", e));
    }
}

