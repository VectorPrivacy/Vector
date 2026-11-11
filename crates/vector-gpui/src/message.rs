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

/// No video encoder in this client yet: a video sends as picked.
pub mod video_compression {
    pub fn is_video_extension(_: &str) -> bool {
        false
    }

    pub fn compress_file(_: &str) -> Result<super::types::CachedCompressedImage, String> {
        Err("Video compression isn't available".into())
    }

    pub fn renamed(name: &str, ext: &str) -> String {
        let stem = std::path::Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(name);
        format!("{stem}.{ext}")
    }
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
