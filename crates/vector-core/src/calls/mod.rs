//! Calls. The platform-free half (wire format, jitter buffer, rate control) is the
//! `vector-calls` crate, re-exported here; with the `calls` feature, the session and
//! its video track run here too, over whatever a client plugs into [`platform`].

pub use vector_calls::*;

#[cfg(feature = "calls")]
pub mod platform;
#[cfg(feature = "calls")]
pub mod session;
#[cfg(feature = "calls")]
pub mod settings;
#[cfg(feature = "calls")]
pub mod video;
