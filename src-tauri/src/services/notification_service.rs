//! OS notification service for the Vector application.
//!
//! This module provides a unified notification system that handles:
//! - Direct message notifications
//! - Group message notifications
//! - Group invite notifications
//!
//! Notifications are shown only when the app is not focused, and include
//! platform-specific handling for Android vs desktop.

use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

#[cfg(not(target_os = "android"))]
use crate::audio;
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
}

impl NotificationData {
    /// Create a DM notification (works for both text and file attachments)
    pub fn direct_message(sender_name: String, content: String) -> Self {
        Self {
            notification_type: NotificationType::DirectMessage,
            title: sender_name.clone(),
            body: content,
            group_name: None,
            sender_name: Some(sender_name),
        }
    }

    /// Create a group message notification (works for both text and file attachments)
    pub fn group_message(sender_name: String, group_name: String, content: String) -> Self {
        Self {
            notification_type: NotificationType::GroupMessage,
            title: format!("{} - {}", sender_name, group_name),
            body: content,
            group_name: Some(group_name),
            sender_name: Some(sender_name),
        }
    }

    /// Create a group invite notification
    #[allow(dead_code)]
    pub fn group_invite(group_name: String, inviter_name: String) -> Self {
        Self {
            notification_type: NotificationType::GroupInvite,
            title: format!("Group Invite: {}", group_name),
            body: format!("Invited by {}", inviter_name),
            group_name: Some(group_name),
            sender_name: Some(inviter_name),
        }
    }
}

/// Show an OS notification with generic notification data
pub fn show_notification_generic(data: NotificationData) {
    let handle = TAURI_APP.get().unwrap();

    // Only send notifications if the app is not focused
    if !handle
        .webview_windows()
        .iter()
        .next()
        .unwrap()
        .1
        .is_focused()
        .unwrap()
    {
        // Play notification sound (non-blocking, desktop only)
        #[cfg(desktop)]
        {
            let handle_clone = handle.clone();
            std::thread::spawn(move || {
                if let Err(e) = audio::play_notification_if_enabled(&handle_clone) {
                    eprintln!("Failed to play notification sound: {}", e);
                }
            });
        }

        #[cfg(target_os = "android")]
        {
            // Determine summary based on notification type
            let summary = match data.notification_type {
                NotificationType::DirectMessage => "Private Message",
                NotificationType::GroupMessage => "Group Message",
                NotificationType::GroupInvite => "Group Invite",
            };

            handle
                .notification()
                .builder()
                .title(&data.title)
                .body(&data.body)
                .large_body(&data.body)
                .icon("ic_notification")
                .summary(summary)
                .large_icon("ic_large_icon")
                .show()
                .unwrap_or_else(|e| eprintln!("Failed to send notification: {}", e));
        }

        #[cfg(not(target_os = "android"))]
        {
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
}
