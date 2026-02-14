//! Error types for Mini Apps

#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
    Tauri(tauri::Error),
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    MiniAppNotFound(String),
    InstanceNotFoundByLabel(String),
    InvalidPackage(String),
    FileNotFound(String),
    ManifestParseError(String),
    BlackholeProxyUnavailable,
    Anyhow(anyhow::Error),
    RealtimeChannelAlreadyActive,
    RealtimeChannelNotActive,
    RealtimeDataTooLarge(usize),
    RealtimeError(String),
    DatabaseError(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Tauri(e) => write!(f, "{}", e),
            Error::Io(e) => write!(f, "{}", e),
            Error::Zip(e) => write!(f, "{}", e),
            Error::MiniAppNotFound(s) => write!(f, "Mini App not found: {}", s),
            Error::InstanceNotFoundByLabel(s) => write!(f, "Mini App instance not found by window label: {}", s),
            Error::InvalidPackage(s) => write!(f, "Invalid Mini App package: {}", s),
            Error::FileNotFound(s) => write!(f, "File not found in Mini App: {}", s),
            Error::ManifestParseError(s) => write!(f, "Failed to parse manifest: {}", s),
            Error::BlackholeProxyUnavailable => write!(f, "Blackhole proxy unavailable for network isolation"),
            Error::Anyhow(e) => write!(f, "{}", e),
            Error::RealtimeChannelAlreadyActive => write!(f, "Realtime channel already active - call leave() first"),
            Error::RealtimeChannelNotActive => write!(f, "Realtime channel not active - call joinRealtimeChannel() first"),
            Error::RealtimeDataTooLarge(size) => write!(f, "Realtime data too large: {} bytes (max 128000)", size),
            Error::RealtimeError(s) => write!(f, "Realtime channel error: {}", s),
            Error::DatabaseError(s) => write!(f, "Database error: {}", s),
        }
    }
}

impl std::error::Error for Error {}

impl From<tauri::Error> for Error {
    fn from(e: tauri::Error) -> Self { Error::Tauri(e) }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self { Error::Io(e) }
}

impl From<zip::result::ZipError> for Error {
    fn from(e: zip::result::ZipError) -> Self { Error::Zip(e) }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self { Error::Anyhow(e) }
}

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}
