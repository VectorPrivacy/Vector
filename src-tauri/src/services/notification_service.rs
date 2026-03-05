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
    GroupMessage,
    GroupInvite,
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
    /// Optional cached avatar file path for the group (MLS groups only)
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

    /// Create a group message notification (works for both text and file attachments)
    pub fn group_message(sender_name: String, group_name: String, content: String, avatar_path: Option<String>, group_avatar_path: Option<String>, chat_id: String) -> Self {
        Self {
            notification_type: NotificationType::GroupMessage,
            title: format!("{} - {}", sender_name, group_name),
            body: content,
            group_name: Some(group_name),
            sender_name: Some(sender_name),
            avatar_path,
            group_avatar_path,
            chat_id: Some(chat_id),
        }
    }

    /// Create a group invite notification
    #[allow(dead_code)]
    pub fn group_invite(group_name: String, inviter_name: String, avatar_path: Option<String>) -> Self {
        Self {
            notification_type: NotificationType::GroupInvite,
            title: group_name.clone(),
            body: format!("Invited by {}", inviter_name),
            group_name: Some(group_name),
            sender_name: Some(inviter_name),
            avatar_path,
            group_avatar_path: None,
            chat_id: None, // No chat to navigate to yet (pending welcome)
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

/// Replace `@npub1...` mentions in message content with `@DisplayName`.
/// Prioritises nickname > name > leaves raw npub unchanged.
///
/// Operates on `&str` slices to stay UTF-8 safe — npub1 + 58 bech32 chars are
/// always ASCII, so we anchor on byte offsets only within the ASCII portion and
/// copy surrounding text (which may contain emoji / multibyte) via `&content[..]`.
pub fn resolve_mention_display_names(content: &str, state: &crate::state::ChatState) -> String {
    // npub = "npub1" (5) + 58 bech32 chars = 63 ASCII bytes; with '@' prefix = 64
    const MENTION_LEN: usize = 64; // '@' + 63
    const NPUB_LEN: usize = 63;
    const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len);
    let mut cursor = 0; // byte offset of uncopied content

    // Scan for '@npub1' anchors
    let mut i = 0;
    while i + MENTION_LEN <= len {
        if bytes[i] == b'@' && &bytes[i + 1..i + 6] == b"npub1" {
            // Validate 58 bech32 chars after 'npub1'
            let npub_start = i + 1;
            let npub_end = npub_start + NPUB_LEN;
            let valid = bytes[npub_start + 5..npub_end]
                .iter()
                .all(|b| BECH32.contains(&b.to_ascii_lowercase()));
            if valid {
                // Copy any text before this mention verbatim (UTF-8 safe)
                result.push_str(&content[cursor..i]);

                let npub = &content[npub_start..npub_end];
                if let Some(profile) = state.get_profile(npub) {
                    let name = if !profile.nickname.is_empty() {
                        &profile.nickname
                    } else if !profile.name.is_empty() {
                        &profile.name
                    } else {
                        npub
                    };
                    result.push('@');
                    result.push_str(name);
                } else {
                    result.push_str(&content[i..npub_end]);
                }
                cursor = npub_end;
                i = npub_end;
                continue;
            }
        }
        i += 1;
    }

    // Append remaining content after last match (or entire string if no matches)
    result.push_str(&content[cursor..]);
    result
}

/// Show an OS notification with generic notification data
pub fn show_notification_generic(data: NotificationData) {
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

