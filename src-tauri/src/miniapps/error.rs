//! Error types for Mini Apps

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum Error {
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
    
    #[error(transparent)]
    Io(#[from] std::io::Error),
    
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    
    #[error("Mini App not found: {0}")]
    MiniAppNotFound(String),
    
    #[error("Mini App instance not found by window label: {0}")]
    InstanceNotFoundByLabel(String),
    
    #[error("Invalid Mini App package: {0}")]
    InvalidPackage(String),
    
    #[error("File not found in Mini App: {0}")]
    FileNotFound(String),
    
    #[error("Failed to parse manifest: {0}")]
    ManifestParseError(String),
    
    #[error("Blackhole proxy unavailable for network isolation")]
    BlackholeProxyUnavailable,
    
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
    
    // Realtime channel errors
    #[error("Realtime channel already active - call leave() first")]
    RealtimeChannelAlreadyActive,
    
    #[error("Realtime channel not active - call joinRealtimeChannel() first")]
    RealtimeChannelNotActive,
    
    #[error("Realtime data too large: {0} bytes (max 128000)")]
    RealtimeDataTooLarge(usize),
    
    #[error("Realtime channel error: {0}")]
    RealtimeError(String),
}

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}