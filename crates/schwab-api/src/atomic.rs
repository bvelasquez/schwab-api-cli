use std::path::{Path, PathBuf};

/// Sibling temp path used for atomic replace (`foo.json` → `foo.json.tmp`).
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Write `data` to `path` via a sibling temp file + rename so a crash or ENOSPC
/// cannot truncate the destination to an empty file.
pub fn write_atomic_sync(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(path);
    let result = (|| {
        std::fs::write(&tmp, data.as_ref())?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Async variant of [`write_atomic_sync`].
pub async fn write_atomic(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = tmp_path(path);
    let result = async {
        tokio::fs::write(&tmp, data.as_ref()).await?;
        tokio::fs::rename(&tmp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmp_path_appends_suffix() {
        assert_eq!(
            tmp_path(Path::new("/tmp/tokens.json")),
            PathBuf::from("/tmp/tokens.json.tmp")
        );
    }

    #[test]
    fn write_atomic_sync_replaces_existing() {
        let dir = std::env::temp_dir().join(format!(
            "schwab-atomic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens.json");
        std::fs::write(&path, b"old").unwrap();
        write_atomic_sync(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"ok\":true}");
        assert!(!tmp_path(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
