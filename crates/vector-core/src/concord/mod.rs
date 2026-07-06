//! Concord — Vector's serverless communities protocol.
//!
//! Versioned side by side: [`v1`] is the original in-house protocol (spec in
//! `docs/concord/`); v2 (the public CORD spec) will live beside it. The
//! communities surface reads from every version but new communities are only
//! created on the newest one.
//!
//! `vector_core::community` is a crate-root alias for [`v1`] so existing
//! consumers (src-tauri, vector-sdk, vector-agent, concord-cli) keep working
//! unchanged.

pub mod v1;
pub mod v2;
