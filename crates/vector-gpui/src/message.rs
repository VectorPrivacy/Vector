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

    /// The desktop's picker cache, read before a content URI: uri -> (bytes, extension, name,
    /// size, when cached). Pickers here hand over files, so it stays empty.
    #[cfg(target_os = "android")]
    #[allow(clippy::type_complexity)]
    pub static ANDROID_FILE_CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, (Arc<Vec<u8>>, String, String, u64, std::time::Instant)>>,
    > = std::sync::LazyLock::new(Default::default);
}

#[cfg(target_os = "android")]
pub use vector_core::types::AttachmentFile;

#[cfg(target_os = "android")]
pub struct FileInfo {
    pub size: u64,
    pub name: String,
    pub extension: String,
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
    compression::compress_image_internal(&path.to_string_lossy()).map(crate::jxl::outbound)
}

/// The desktop client's send processing, then this client's JPEG XL. Uncompressed, the picture
/// leaves in its own format: only compressing may change it.
pub fn process_image_for_send(
    bytes: std::sync::Arc<Vec<u8>>,
    extension: &str,
    compress: bool,
    keep_metadata: bool,
    precompressed: Option<types::CachedCompressedImage>,
) -> Result<types::CachedCompressedImage, String> {
    let out = compression::process_image_for_send(bytes, extension, compress, keep_metadata, precompressed)?;
    Ok(if compress { crate::jxl::outbound(out) } else { out })
}

#[cfg(test)]
mod send_format_tests {
    fn png() -> Vec<u8> {
        let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([(x * 4) as u8, (y * 5) as u8, 90]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn an_uncompressed_picture_keeps_its_format() {
        let sent = super::process_image_for_send(std::sync::Arc::new(png()), "png", false, false, None).unwrap();
        assert_eq!(sent.extension, "png");
        assert_eq!(image::guess_format(&sent.bytes).unwrap(), image::ImageFormat::Png);
    }

    #[test]
    fn a_compressed_picture_leaves_as_jpeg_xl() {
        let sent = super::process_image_for_send(std::sync::Arc::new(png()), "png", true, false, None).unwrap();
        assert_eq!(sent.extension, "jxl");
    }
}
