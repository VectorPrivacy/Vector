//! Business logic services for the Vector application.
//!
//! This module contains core business logic separated from Tauri commands:
//! - `event_handler`: Main event dispatcher for handling incoming Nostr events
//! - `subscription_handler`: Live subscription handling for real-time events
//! - `notification_service`: OS notification handling
//!
//! Services are used by command handlers and can be unit tested independently.

pub mod event_handler;
pub mod subscription_handler;
pub mod notification_service;

pub(crate) use event_handler::handle_event;
pub(crate) use event_handler::handle_webxdc_peer_advertisement;
pub(crate) use subscription_handler::start_subscriptions;
pub(crate) use notification_service::{NotificationData, show_notification_generic};
