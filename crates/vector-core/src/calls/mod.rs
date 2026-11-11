//! Calls. The platform-free half (wire format, jitter buffer, rate control) is the
//! `vector-calls` crate, re-exported here; with the `calls` feature, the session and
//! its video track run here too, over whatever a client plugs into [`platform`](crate::calls::platform).

pub use vector_calls::*;

#[cfg(feature = "calls")]
pub mod platform;
#[cfg(feature = "calls")]
pub mod session;
#[cfg(feature = "calls")]
pub mod settings;
#[cfg(feature = "calls")]
pub mod video;
#[cfg(feature = "calls")]
pub mod voice;

/// The Main Menu's switches: remembered for every call, and applied to the one under way.
#[cfg(feature = "calls")]
pub async fn set_mic(s: settings::MicState) -> Result<(), String> {
    settings::remember_mic(s);
    if voice::snapshot().is_some() {
        voice::set_deafened(s.deafened)?;
        voice::set_muted(s.muted)?;
    }
    if session::snapshot().is_some() {
        session::set_deafened(s.deafened).await?;
        if !s.deafened {
            session::set_muted(s.muted).await?;
        }
    }
    Ok(())
}
