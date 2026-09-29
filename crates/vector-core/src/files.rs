//! Files an account keeps beside its database, on either target: the
//! filesystem natively, OPFS on the web. Async because OPFS only answers
//! through promises; natively the calls are the same std::fs ones as before.

use std::path::{Path, PathBuf};

pub async fn read(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(target_arch = "wasm32")]
    return crate::webfiles::read(path).await;
    #[cfg(not(target_arch = "wasm32"))]
    std::fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))
}

/// Replace `path` whole: a reader never sees a partial file.
pub async fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    return crate::webfiles::write(path, bytes).await;
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = path.with_file_name(format!("{name}.tmp"));
        std::fs::write(&tmp, bytes).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("Failed to commit {}: {e}", path.display()))
    }
}

pub async fn rename(from: &Path, to: &Path) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = crate::webfiles::read(from).await?;
        crate::webfiles::write(to, &bytes).await?;
        crate::webfiles::remove(from).await;
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::fs::rename(from, to).map_err(|e| format!("Failed to move {}: {e}", from.display()))
}

pub async fn remove(path: &Path) {
    #[cfg(target_arch = "wasm32")]
    crate::webfiles::remove(path).await;
    #[cfg(not(target_arch = "wasm32"))]
    let _ = std::fs::remove_file(path);
}

/// The files directly inside `dir`; empty when it doesn't exist.
pub async fn list(dir: &Path) -> Vec<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    return crate::webfiles::list(dir).await.into_iter().map(|n| dir.join(n)).collect();
    #[cfg(not(target_arch = "wasm32"))]
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
        .unwrap_or_default()
}
