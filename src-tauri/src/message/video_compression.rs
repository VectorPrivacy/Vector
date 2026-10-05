//! Videos on the attachment compression pipeline: the preview starts the encode, the send takes
//! the result. Encoding runs through `crate::video` (the `video` feature); a build without it
//! never offers Compress for videos.

use super::types::CachedCompressedImage;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// Containers this build's decoders read; anything else is sent as it is.
pub(crate) fn is_video_extension(ext: &str) -> bool {
    matches!(ext, "mp4" | "mov" | "m4v" | "webm")
}

/// `name` with its extension replaced by `ext`.
pub(crate) fn renamed(name: &str, ext: &str) -> String {
    let stem = std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    format!("{stem}.{ext}")
}

/// Whether this build and device can compress video at all.
pub(crate) fn available() -> bool {
    #[cfg(feature = "video")]
    {
        crate::video::available()
    }
    #[cfg(not(feature = "video"))]
    {
        false
    }
}

#[derive(Default)]
struct Job {
    /// Fraction done in thousandths.
    progress: AtomicU32,
    cancel: AtomicBool,
}

/// Encodes in flight, by source path: the preview polls progress, closing it cancels.
static JOBS: LazyLock<Mutex<HashMap<String, Arc<Job>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Fraction done of the encode running for `path`, if one is.
pub(crate) fn progress(path: &str) -> Option<f32> {
    let jobs = JOBS.lock().ok()?;
    jobs.get(path).map(|j| j.progress.load(Ordering::Relaxed) as f32 / 1000.0)
}

/// Stop the encode running for `path`; it ends with an error at its next packet.
pub(crate) fn cancel(path: &str) {
    if let Ok(jobs) = JOBS.lock() {
        if let Some(j) = jobs.get(path) {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub(crate) fn cancel_all() {
    if let Ok(mut jobs) = JOBS.lock() {
        for (_, j) in jobs.drain() {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Compress the video at `path` for sending: an MP4 kept only when at least a tenth smaller,
/// else the original file (already under the target bitrate, it would only lose detail),
/// which is left on disk to stream rather than read into memory.
pub(crate) fn compress_file(path: &str) -> Result<CachedCompressedImage, String> {
    let original_size = std::fs::metadata(path).map_err(|e| format!("Failed to read video: {e}"))?.len();
    let extension = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_lowercase();
    let original = || CachedCompressedImage {
        bytes: Arc::new(Vec::new()),
        extension: extension.clone(),
        img_meta: None,
        original_size,
        compressed_size: original_size,
    };
    match encode(path) {
        Ok(out) if (out.len() as u64) * 10 <= original_size * 9 => Ok(CachedCompressedImage {
            compressed_size: out.len() as u64,
            bytes: Arc::new(out),
            extension: "mp4".into(),
            img_meta: None,
            original_size,
        }),
        Ok(_) => Ok(original()),
        Err(e) if e.contains("cancelled") => Err(e),
        Err(e) => {
            log_warn!("[Video] compression failed, sending the original: {e}");
            Ok(original())
        }
    }
}

#[cfg(feature = "video")]
fn encode(path: &str) -> Result<Vec<u8>, String> {
    let job = Arc::new(Job::default());
    if let Ok(mut jobs) = JOBS.lock() {
        jobs.insert(path.to_string(), job.clone());
    }
    let out = std::env::temp_dir().join(format!("vector-video-{}.mp4", &vector_core::crypto::sha256_hex(path.as_bytes())[..16]));
    let result = crate::video::compress(path.as_ref(), &out, &job.cancel, |f| job.progress.store((f * 1000.0) as u32, Ordering::Relaxed))
        .and_then(|_| std::fs::read(&out).map_err(|e| format!("read compressed video: {e}")));
    let _ = std::fs::remove_file(&out);
    if let Ok(mut jobs) = JOBS.lock() {
        // A newer encode of the same file may have replaced this one.
        if jobs.get(path).is_some_and(|j| Arc::ptr_eq(j, &job)) {
            jobs.remove(path);
        }
    }
    result
}

#[cfg(not(feature = "video"))]
fn encode(_path: &str) -> Result<Vec<u8>, String> {
    Err("video compression is not in this build".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_swaps_only_the_extension() {
        assert_eq!(renamed("SPOILER_clip.mov", "mp4"), "SPOILER_clip.mp4");
        assert_eq!(renamed("a.b.webm", "mp4"), "a.b.mp4");
        assert_eq!(renamed("noext", "mp4"), "noext.mp4");
    }

    /// Each fixture sends as whichever is smaller, and leaves no job behind.
    #[cfg(feature = "video")]
    #[test]
    #[ignore = "needs VIDEO_FIXTURES and an FFmpeg with an encoder"]
    fn fixtures_send_the_smaller_of_original_and_mp4() {
        let dir = std::path::PathBuf::from(std::env::var("VIDEO_FIXTURES").unwrap());
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let p = path.to_str().unwrap();
            let sent = compress_file(p).unwrap();
            eprintln!("{p}: {} -> {} .{}", sent.original_size, sent.compressed_size, sent.extension);
            assert!(sent.compressed_size <= sent.original_size);
            if sent.compressed_size < sent.original_size {
                assert_eq!(sent.extension, "mp4");
                assert_eq!(sent.bytes.len() as u64, sent.compressed_size);
            } else {
                assert!(sent.is_original_on_disk());
            }
            assert!(progress(p).is_none());
        }
    }
}
