//! Picks held in memory (Android): the picker's copy is a memfd reached by a link under the
//! cache that holds only its number, so a document that exists only in another app's memory
//! never lands on disk. The memfd carries the document's own name.

use std::path::Path;

#[cfg(any(target_os = "android", test))]
fn held(path: &Path) -> Option<(i32, String)> {
    let link = std::fs::read_link(path).ok()?;
    let fd: i32 = link.to_str()?.strip_prefix("/proc/self/fd/")?.parse().ok()?;
    let target = std::fs::read_link(&link).ok()?;
    let name = target.to_str()?.strip_prefix("/memfd:")?;
    Some((fd, name.strip_suffix(" (deleted)").unwrap_or(name).to_string())) // i18n-exempt: the kernel's link text
}

#[cfg(not(any(target_os = "android", test)))]
fn held(_: &Path) -> Option<(i32, String)> {
    None
}

pub fn is_held(path: &Path) -> bool {
    held(path).is_some()
}

/// A held pick's own name: its link is named only by the extension.
pub fn name(path: &Path) -> Option<String> {
    held(path).map(|(_, n)| n).filter(|n| !n.is_empty() && n != "file")
}

/// A pick is done with: a held one's memory freed, a copied one's folder gone.
pub fn release(path: &Path) {
    if let Some((fd, _)) = held(path) {
        let _ = std::fs::remove_file(path);
        // SAFETY: the number belongs to the link alone, and the link is gone first.
        #[cfg(unix)]
        unsafe {
            libc::close(fd)
        };
        #[cfg(not(unix))]
        let _ = fd;
    }
    #[cfg(target_os = "android")]
    if let Some(root) = crate::android::cache_dir().map(|c| c.join("picked")) {
        if let Some(dir) = path.parent().filter(|d| d.starts_with(&root) && *d != root) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A file at `path` for bytes made from held picks: in memory too when `memory`, else on disk.
pub fn create(path: &Path, memory: bool) -> std::io::Result<std::fs::File> {
    #[cfg(any(target_os = "android", all(test, target_os = "linux")))]
    if memory {
        use std::os::fd::{FromRawFd, IntoRawFd};
        let name = std::ffi::CString::new(path.file_name().map(|n| n.as_encoded_bytes().to_vec()).unwrap_or_default())
            .unwrap_or_default();
        // SAFETY: a NUL-terminated name and plain flags; a negative answer is an error.
        let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), libc::MFD_CLOEXEC) } as i32;
        if fd >= 0 {
            // SAFETY: a fresh fd nothing else owns.
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            let kept = file.try_clone()?.into_raw_fd();
            if std::os::unix::fs::symlink(format!("/proc/self/fd/{kept}"), path).is_ok() {
                return Ok(file);
            }
            // SAFETY: `kept` was never handed out.
            unsafe { libc::close(kept) };
        }
    }
    #[cfg(not(any(target_os = "android", all(test, target_os = "linux"))))]
    let _ = memory;
    std::fs::File::create(path)
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "linux")]
    fn a_held_file_reads_by_path_and_its_memory_goes_with_it() {
        use super::*;
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("vector-picked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Holiday photo.png");
        create(&path, true).unwrap().write_all(b"secret pixels").unwrap();
        assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&path).unwrap(), b"secret pixels");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 13);
        assert_eq!(name(&path).as_deref(), Some("Holiday photo.png"));
        let fd = held(&path).unwrap().0;
        release(&path);
        assert!(!path.exists() && std::fs::symlink_metadata(&path).is_err());
        assert!(std::fs::read_link(format!("/proc/self/fd/{fd}")).is_err());
        let plain = dir.join("plain.txt");
        create(&plain, false).unwrap().write_all(b"x").unwrap();
        assert!(!is_held(&plain) && name(&plain).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
