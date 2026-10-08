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

/// Like [`write_atomic_sync`], but the file is mode `0600` and its parent directory
/// is mode `0700`. Use for OAuth tokens and other credentials.
pub fn write_atomic_private_sync(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
            restrict_owner_dir(parent)?;
        }
    }
    let tmp = tmp_path(path);
    let result = (|| {
        write_owner_file(&tmp, data.as_ref())?;
        std::fs::rename(&tmp, path)?;
        restrict_owner_file(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Owner-only atomic write (`0600`) that does not change the parent directory mode.
/// Use for agent state files that contain account hashes.
pub fn write_atomic_owner_sync(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = tmp_path(path);
    let result = (|| {
        write_owner_file(&tmp, data.as_ref())?;
        std::fs::rename(&tmp, path)?;
        restrict_owner_file(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Async variant of [`write_atomic_private_sync`].
pub async fn write_atomic_private(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let path = path.to_path_buf();
    let data = data.as_ref().to_vec();
    tokio::task::spawn_blocking(move || write_atomic_private_sync(&path, data))
        .await
        .map_err(std::io::Error::other)?
}

/// Open `path` for append, creating it as mode `0600` when it does not exist.
/// Tightens an existing file that is group- or world-readable.
pub fn open_owner_append(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    restrict_owner_file(path)?;
    Ok(file)
}

/// Force mode `0600`. Warns when the previous mode was group- or world-readable.
pub fn restrict_owner_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            tracing::warn!(
                path = %path.display(),
                "file holding account data was group or world readable; restricting to 0600"
            );
        }
        set_mode(path, 0o600)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub fn restrict_owner_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            tracing::warn!(
                path = %path.display(),
                "credential directory was group or world accessible; restricting to 0700"
            );
        }
        set_mode(path, 0o700)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn write_owner_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    restrict_owner_file(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)
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

    #[cfg(unix)]
    #[test]
    fn private_write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "schwab-priv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("creds").join("tokens.json");
        write_atomic_private_sync(&path, b"{\"secret\":true}").unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "file mode {file_mode:o}");
        assert_eq!(dir_mode, 0o700, "dir mode {dir_mode:o}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"secret\":true}"
        );
        assert!(!tmp_path(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
