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

    /// Rewrite the visible fields per the content-privacy setting. `chat_id` is never
    /// shown, so tap-to-open keeps working.
    pub fn apply_content_privacy(&mut self, privacy: vector_core::notify::ContentPrivacy) {
        let mut shown = vector_core::notify::Preview {
            title: std::mem::take(&mut self.title),
            body: std::mem::take(&mut self.body),
            sender: self.sender_name.take(),
            avatar: self.avatar_path.take(),
            group: self.group_name.take(),
            group_avatar: self.group_avatar_path.take(),
        };
        shown.apply(privacy);
        self.title = shown.title;
        self.body = shown.body;
        self.sender_name = shown.sender;
        self.avatar_path = shown.avatar;
        self.group_name = shown.group;
        self.group_avatar_path = shown.group_avatar;
    }
}

/// Strip HTML tags and markdown formatting from message content for notification previews.
/// Returns clean plaintext suitable for OS notifications.
///
/// Used at notification call sites in `event_handler.rs` and `subscription_handler.rs`
/// to clean content *after* mention resolution but *before* passing to OS notification APIs.
pub fn strip_content_for_preview(text: &str) -> String {
    // Replace <br> variants with space before tag stripping (so we don't lose line breaks)
    let text = vector_core::notify::strip_ansi(text).replace("<br>", " ").replace("<br/>", " ").replace("<br />", " ")
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

/// Revoke the OS notification for a chat once it's been read (opened in-app) or answered on
/// another device. Android: cancels the per-chat notification via JNI (no-op if none is showing).
/// Desktop: no-op (desktop notifications aren't persistent or handle-tracked).
pub fn cancel_chat_notification(chat_id: &str) {
    #[cfg(target_os = "android")]
    crate::android::background_sync::cancel_notification_jni(chat_id);

    #[cfg(not(target_os = "android"))]
    let _ = chat_id;
}

/// [`cancel_chat_notification`] for a read the *user* performed, rather than one
/// inferred from activity elsewhere.
///
/// While soft-backgrounded the frontend still runs, so a message arriving in the
/// open chat fires its markAsRead and revokes the notification for that very
/// message microseconds before it is posted. Cancelling also drops the chat's
/// MessagingStyle history, so each notification shows a single message instead of
/// the conversation. The user hasn't seen anything yet, so there is nothing to
/// revoke: `nativeOnResume` clears the active chat's notification when they
/// actually return.
pub fn cancel_chat_notification_on_user_read(chat_id: &str) {
    #[cfg(target_os = "android")]
    if !crate::android::background_sync::is_activity_in_foreground() {
        return;
    }
    cancel_chat_notification(chat_id);
}

/// Show an OS notification with generic notification data
pub fn show_notification_generic(mut data: NotificationData) {
    // Apply the user's content-privacy preference up front so every platform
    // path inherits it. Android's background-sync service posts straight to
    // post_notification_jni, which re-applies it (the transform is idempotent).
    data.apply_content_privacy(vector_core::notify::ContentPrivacy::load());

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

