//! Tauri command handlers organized by domain.
//!
//! This module organizes Tauri commands into logical categories:
//! - `account`: Authentication and account management (5 commands)
//! - `attachments`: File downloads and blurhash processing (3 commands)
//! - `invites`: Invite codes and badges (4 commands)
//! - `media`: Voice recording and transcription (4 commands)
//! - `relays`: Relay management, connection, monitoring (13 commands)
//! - `sync`: Message sync, profile sync, scanning (7 commands)
//! - `system`: Platform features, storage, maintenance (4 commands)
//! - `messaging`: Message fetching, caching, unread counts (8 commands)
//! - `realtime`: Typing indicators and WebXDC peer discovery (2 commands)
//! - [future] `mls`: MLS group messaging commands
//!
//! Commands are registered in lib.rs via `generate_handler![]`.
//! Each module lists its handlers in a comment at the end of the file.

pub mod sync;
pub mod relays;
pub mod attachments;
pub mod account;
pub mod system;
pub mod invites;
pub mod media;
pub mod messaging;
pub mod mls;
pub mod realtime;
pub mod encryption;
