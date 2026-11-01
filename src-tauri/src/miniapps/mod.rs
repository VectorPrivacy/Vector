//! Mini Apps (WebXDC) implementation for Vector
//!
//! Runs WebXDC apps (`.xdc` packages) inside Vector, each in an isolated webview.
//!
//! ## Marketplace
//!
//! The marketplace module provides a decentralized app store using Nostr
//! for metadata and Blossom for file storage.

pub(crate) mod error;
pub(crate) mod scheme;
pub(crate) mod state;
pub(crate) mod commands;
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
pub(crate) mod network_isolation;
#[cfg(not(target_os = "android"))]
pub(crate) mod isolated;
#[cfg(target_os = "macos")]
pub(crate) mod pointer_lock;
pub(crate) mod realtime;
pub(crate) mod rt_ws;
pub(crate) mod marketplace;
pub(crate) use vector_core::webxdc_permissions as permissions;