use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use std::sync::LazyLock;

pub use vector_core::types::{
    Message, Attachment, Reaction,
    ImageMetadata, AttachmentFile,
};

/// Cached compressed image data
#[derive(Clone)]
pub struct CachedCompressedImage {
    pub bytes: Arc<Vec<u8>>,
    pub extension: String,
    pub img_meta: Option<ImageMetadata>,
    pub original_size: u64,
    pub compressed_size: u64,
}

/// Global cache for pre-compressed images
pub static COMPRESSION_CACHE: LazyLock<TokioMutex<HashMap<String, Option<CachedCompressedImage>>>> =
    LazyLock::new(|| TokioMutex::new(HashMap::new()));

/// Notifiers for compression completion — waiters subscribe, compressor signals
pub static COMPRESSION_NOTIFY: LazyLock<TokioMutex<HashMap<String, Arc<tokio::sync::Notify>>>> =
    LazyLock::new(|| TokioMutex::new(HashMap::new()));

/// Cache for Android file bytes: uri -> (bytes, extension, name, size)
/// This is used to cache file bytes immediately after file selection on Android,
/// before the temporary content URI permission expires.
pub static ANDROID_FILE_CACHE: LazyLock<std::sync::Mutex<HashMap<String, (Arc<Vec<u8>>, String, String, u64)>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));