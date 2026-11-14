//! The desktop client's outbound image processing, compiled from its own sources so a picture
//! leaves either client byte for byte alike.

#[path = "../../../src-tauri/src/message/compression.rs"]
pub mod compression;

pub mod types {
    use std::sync::Arc;

    pub use vector_core::types::ImageMetadata;

    #[derive(Clone)]
    pub struct CachedCompressedImage {
        pub bytes: Arc<Vec<u8>>,
        pub extension: String,
        pub img_meta: Option<ImageMetadata>,
        pub original_size: u64,
        pub compressed_size: u64,
    }

    impl CachedCompressedImage {
        pub fn is_original_on_disk(&self) -> bool {
            self.bytes.is_empty() && self.original_size > 0
        }
    }
}

#[path = "../../../src-tauri/src/message/video_compression.rs"]
pub mod video_compression;

pub mod files {
    /// Encodes write here, inside the account's directory rather than the shared temp dir.
    pub(crate) fn private_temp_dir() -> std::path::PathBuf {
        let dir = vector_core::db::current_account_dir()
            .map(|d| d.join("outgoing").join("work"))
            .unwrap_or_else(|_| std::env::temp_dir());
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
}

/// `video_quality`: `small`, `balanced` (the default), `high` or `original`.
pub fn video_quality() -> String {
    vector_core::db::settings::get_sql_setting(video_compression::QUALITY_SETTING.into())
        .ok()
        .flatten()
        .unwrap_or_else(|| "balanced".into())
}

/// `compressVideo`: an encoder, a preset that compresses, and a clip worth the wait.
pub fn offers_video_compression(ext: &str, size: u64) -> bool {
    video_compression::is_video_extension(ext)
        && size > 1024 * 1024
        && video_compression::available()
        && video_quality() != "original"
}

/// The share of `original` a compression saves, in whole percent.
pub fn savings_percent(c: &types::CachedCompressedImage) -> u32 {
    if c.original_size > 0 && c.compressed_size < c.original_size {
        ((c.original_size - c.compressed_size) * 100 / c.original_size) as u32
    } else {
        0
    }
}

/// A picked image compressed as a default send would: resized, stripped and re-encoded.
pub fn precompress(path: &std::path::Path) -> Result<types::CachedCompressedImage, String> {
    compression::compress_image_internal(&path.to_string_lossy())
}
