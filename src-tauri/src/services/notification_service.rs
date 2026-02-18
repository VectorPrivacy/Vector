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
    let handle = match TAURI_APP.get() {
        Some(h) => h,
        None => return,
    };

    // Check if the app is focused — if a window exists and is focused, skip notification
    let is_focused = handle
        .webview_windows()
        .iter()
        .next()
        .and_then(|(_, w)| w.is_focused().ok())
        .unwrap_or(false);

    if is_focused {
        return;
    }

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
        // Try Tauri notification plugin first (works when Activity is alive)
        let tauri_notification_sent = handle
            .webview_windows()
            .iter()
            .next()
            .is_some()
            .then(|| {
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
                    .is_ok()
            })
            .unwrap_or(false);

        // Fallback: post notification directly via JNI (background mode, no Activity)
        if !tauri_notification_sent {
            show_native_android_notification(&data);
        }
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

/// Post a notification directly via Android's NotificationManager using JNI.
/// Used when the app is in background mode and the Tauri notification plugin
/// can't function (no Activity/WebView available).
#[cfg(target_os = "android")]
fn show_native_android_notification(data: &NotificationData) {
    use crate::android::utils::with_android_context;

    if let Err(e) = with_android_context(|env, activity| {
        // Get NotificationManager system service
        let service_name = env.new_string("notification")
            .map_err(|e| format!("Failed to create string: {:?}", e))?;
        let manager = env.call_method(
            activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[(&service_name).into()],
        )
        .map_err(|e| format!("Failed to get NotificationManager: {:?}", e))?
        .l()
        .map_err(|e| format!("Failed to convert NotificationManager: {:?}", e))?;

        // Build notification using NotificationCompat.Builder
        let channel_id = env.new_string("vector_messages")
            .map_err(|e| format!("Failed to create channel string: {:?}", e))?;

        // Create NotificationCompat.Builder(context, channelId)
        let builder = env.new_object(
            "androidx/core/app/NotificationCompat$Builder",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[activity.into(), (&channel_id).into()],
        )
        .map_err(|e| format!("Failed to create NotificationCompat.Builder: {:?}", e))?;

        // Set title
        let title = env.new_string(&data.title)
            .map_err(|e| format!("Failed to create title string: {:?}", e))?;
        env.call_method(
            &builder,
            "setContentTitle",
            "(Ljava/lang/CharSequence;)Landroidx/core/app/NotificationCompat$Builder;",
            &[(&title).into()],
        )
        .map_err(|e| format!("Failed to set title: {:?}", e))?;

        // Set body
        let body = env.new_string(&data.body)
            .map_err(|e| format!("Failed to create body string: {:?}", e))?;
        env.call_method(
            &builder,
            "setContentText",
            "(Ljava/lang/CharSequence;)Landroidx/core/app/NotificationCompat$Builder;",
            &[(&body).into()],
        )
        .map_err(|e| format!("Failed to set body: {:?}", e))?;

        // Set small icon (use the app launcher icon)
        // Get R.mipmap.ic_launcher
        let r_class = env.find_class("io/vectorapp/R$mipmap")
            .map_err(|e| format!("Failed to find R.mipmap: {:?}", e))?;
        let icon_id = env.get_static_field(r_class, "ic_launcher", "I")
            .map_err(|e| format!("Failed to get ic_launcher: {:?}", e))?
            .i()
            .map_err(|e| format!("Failed to convert icon id: {:?}", e))?;

        env.call_method(
            &builder,
            "setSmallIcon",
            "(I)Landroidx/core/app/NotificationCompat$Builder;",
            &[icon_id.into()],
        )
        .map_err(|e| format!("Failed to set small icon: {:?}", e))?;

        // Set auto-cancel
        env.call_method(
            &builder,
            "setAutoCancel",
            "(Z)Landroidx/core/app/NotificationCompat$Builder;",
            &[true.into()],
        )
        .map_err(|e| format!("Failed to set auto-cancel: {:?}", e))?;

        // Set priority HIGH
        env.call_method(
            &builder,
            "setPriority",
            "(I)Landroidx/core/app/NotificationCompat$Builder;",
            &[1i32.into()], // NotificationCompat.PRIORITY_HIGH = 1
        )
        .map_err(|e| format!("Failed to set priority: {:?}", e))?;

        // Build the notification
        let notification = env.call_method(
            &builder,
            "build",
            "()Landroid/app/Notification;",
            &[],
        )
        .map_err(|e| format!("Failed to build notification: {:?}", e))?
        .l()
        .map_err(|e| format!("Failed to convert notification: {:?}", e))?;

        // Generate a unique notification ID from the title hash
        let notification_id = (data.title.as_bytes().iter().fold(0u32, |acc, &b| {
            acc.wrapping_mul(31).wrapping_add(b as u32)
        }) & 0x7FFFFFFF) as i32;

        // Post the notification via NotificationManager.notify(id, notification)
        env.call_method(
            &manager,
            "notify",
            "(ILandroid/app/Notification;)V",
            &[notification_id.into(), (&notification).into()],
        )
        .map_err(|e| format!("Failed to post notification: {:?}", e))?;

        Ok(())
    }) {
        eprintln!("Failed to show native Android notification: {}", e);
    }
}
