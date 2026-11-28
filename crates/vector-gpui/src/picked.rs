//! Picks held in memory (Android): the picker's copy is a memfd reached by a link under the
//! cache that holds only its number, so a document that exists only in another app's memory
//! never lands on disk. The memfd carries the document's own name.

use std::path::{Path, PathBuf};

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

/// A pick is done with: a copied one's folder gone.
pub fn release(path: &Path) {
    #[cfg(target_os = "android")]
    if let Some(root) = crate::android::cache_dir().map(|c| c.join("picked")) {
        if let Some(dir) = path.parent().filter(|d| d.starts_with(&root) && *d != root) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    #[cfg(not(target_os = "android"))]
    let _ = path;
}

/// Picks for something that sends them: a held pick's bytes move into memory (Android won't
/// let an app reopen its own memfd by path), under a path that carries its own name.
pub fn adopt(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .map(|path| match take(&path) {
            Some((bytes, name)) => crate::backend::pasted::hold_named(bytes, &name),
            None => path,
        })
        .collect()
}

/// Picks for something that reads them by path: a held pick is written beside its link.
pub fn on_disk(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .map(|path| match take(&path) {
            Some((bytes, name)) => {
                let file = path.with_file_name(name.replace(['/', '\\'], "_"));
                match std::fs::write(&file, bytes) {
                    Ok(()) => file,
                    Err(e) => {
                        log::warn!("pick not written: {e}");
                        path
                    }
                }
            }
            None => path,
        })
        .collect()
}

/// A held pick's bytes and name, read through its own number; its memory is freed after.
fn take(path: &Path) -> Option<(Vec<u8>, String)> {
    let (fd, name) = held(path)?;
    let bytes = read_fd(fd);
    let _ = std::fs::remove_file(path);
    // SAFETY: the number belongs to the link alone, and the link is gone first.
    #[cfg(unix)]
    unsafe {
        libc::close(fd)
    };
    let name = Some(name)
        .filter(|n| !n.is_empty() && n != "file")
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "file".into()); // i18n-exempt: a file name
    match bytes {
        Ok(b) => Some((b, name)),
        Err(e) => {
            log::warn!("held pick unreadable: {e}");
            None
        }
    }
}

#[cfg(unix)]
fn read_fd(fd: i32) -> std::io::Result<Vec<u8>> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::FileExt;
    // SAFETY: borrowed for this read only; ManuallyDrop leaves the number open for `take`.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let len = file.metadata()?.len() as usize;
    let mut bytes = vec![0; len];
    file.read_exact_at(&mut bytes, 0)?;
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_fd(_: i32) -> std::io::Result<Vec<u8>> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::*;

    /// A pick as the Android picker hands it over: a named memfd behind a link holding its number.
    #[cfg(target_os = "linux")]
    fn held_pick(dir: &Path, name: &str, bytes: &[u8]) -> (PathBuf, i32) {
        use std::io::Write;
        use std::os::fd::{FromRawFd, IntoRawFd};
        let cname = std::ffi::CString::new(name).unwrap();
        // SAFETY: a NUL-terminated name and plain flags.
        let fd = unsafe { libc::syscall(libc::SYS_memfd_create, cname.as_ptr(), 0) } as i32;
        assert!(fd >= 0);
        // SAFETY: a fresh fd nothing else owns.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(bytes).unwrap();
        let kept = file.into_raw_fd();
        let link = dir.join("file.png");
        std::os::unix::fs::symlink(format!("/proc/self/fd/{kept}"), &link).unwrap();
        (link, kept)
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_held_pick_is_read_through_its_number_and_its_memory_goes_with_it() {
        let dir = std::env::temp_dir().join(format!("vector-picked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (link, fd) = held_pick(&dir, "Holiday photo.png", b"secret pixels");
        let [adopted] = adopt(vec![link.clone()]).try_into().unwrap();
        assert_eq!(adopted.file_name().unwrap(), "Holiday photo.png");
        assert!(!adopted.exists());
        assert_eq!(*crate::backend::pasted::read(&adopted).unwrap(), b"secret pixels");
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(std::fs::read_link(format!("/proc/self/fd/{fd}")).is_err());
        crate::backend::archive::forget(&adopted);

        let (link, _) = held_pick(&dir, "Avatar.png", b"face");
        let [written] = on_disk(vec![link.clone()]).try_into().unwrap();
        assert_eq!(written, dir.join("Avatar.png"));
        assert_eq!(std::fs::read(&written).unwrap(), b"face");
        assert!(std::fs::symlink_metadata(&link).is_err());

        let plain = dir.join("plain.txt");
        std::fs::write(&plain, b"x").unwrap();
        assert_eq!(adopt(vec![plain.clone()]), vec![plain.clone()]);
        assert_eq!(on_disk(vec![plain.clone()]), vec![plain]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
