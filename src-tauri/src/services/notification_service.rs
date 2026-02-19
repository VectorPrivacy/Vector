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
}

impl NotificationData {
    /// Create a DM notification (works for both text and file attachments)
    pub fn direct_message(sender_name: String, content: String, avatar_path: Option<String>) -> Self {
        Self {
            notification_type: NotificationType::DirectMessage,
            title: sender_name.clone(),
            body: content,
            group_name: None,
            sender_name: Some(sender_name),
            avatar_path,
        }
    }

    /// Create a group message notification (works for both text and file attachments)
    pub fn group_message(sender_name: String, group_name: String, content: String, avatar_path: Option<String>) -> Self {
        Self {
            notification_type: NotificationType::GroupMessage,
            title: format!("{} - {}", sender_name, group_name),
            body: content,
            group_name: Some(group_name),
            sender_name: Some(sender_name),
            avatar_path,
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
        }
    }
}

/// Show an OS notification with generic notification data
pub fn show_notification_generic(data: NotificationData) {
    // On Android, always use our native JNI notification path.
    // Tauri's notification plugin is unreliable on Android (requires Activity).
    // post_notification_jni checks is_activity_in_foreground() to suppress
    // notifications when the user is actively using the app.
    #[cfg(target_os = "android")]
    {
        crate::android::background_sync::post_notification_jni(&data.title, &data.body, data.avatar_path.as_deref());
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

