//! Mini Apps (WebXDC) implementation for Vector
//!
//! This module provides support for running isolated web applications
//! within Vector, similar to DeltaChat's WebXDC implementation.
//!
//! ## Marketplace
//!
//! The marketplace module provides a decentralized app store using Nostr
//! for metadata and Blossom for file storage.

pub(crate) mod error;
pub(crate) mod scheme;
pub(crate) mod state;
pub(crate) mod commands;
pub(crate) mod network_isolation;
pub(crate) mod realtime;
pub(crate) mod marketplace;
pub(crate) mod permissions;