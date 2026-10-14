use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Refuse a live order when a legacy `schwab` or `schwab-trader` process still
/// has this account id in its state file.
pub fn legacy_account_busy(rules_dir: &Path, account_id: &str) -> Result<Option<String>> {
    if !rules_dir.is_dir() || account_id.is_empty() {
        return Ok(None);
    }
    for entry in fs::read_dir(rules_dir).with_context(|| format!("read {}", rules_dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(stem) = pid_stem(name) else {
            continue;
        };
        let pid_text = fs::read_to_string(entry.path())?;
        let Some(pid) = pid_text.trim().parse::<i32>().ok() else {
            continue;
        };
        if !pid_alive(pid) {
            continue;
        }
        let state = rules_dir.join(format!("{stem}.json"));
        let body = fs::read_to_string(&state).unwrap_or_default();
        if body.contains(account_id) {
            return Ok(Some(format!(
                "legacy pid {pid} ({}) still references {account_id}",
                entry.path().display()
            )));
        }
    }
    Ok(None)
}

fn pid_stem(file_name: &str) -> Option<String> {
    let rest = file_name.strip_suffix(".pid")?;
    if let Some(stem) = rest.strip_prefix("agent-") {
        if stem.starts_with("state-") || stem.starts_with("journal-") {
            return None;
        }
        return Some(format!("agent-state-{stem}"));
    }
    if let Some(stem) = rest.strip_prefix("trader-") {
        return Some(format!("trader-state-{stem}"));
    }
    None
}

fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

pub struct AccountLock {
    path: PathBuf,
    pid: u32,
}

impl AccountLock {
    pub fn acquire(dir: &Path, account_key: &str) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let safe: String = account_key
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect();
        let path = dir.join(format!("{safe}.lock"));
        let pid = std::process::id();
        if path.is_file() {
            let existing = fs::read_to_string(&path).unwrap_or_default();
            if let Ok(owner) = existing.trim().parse::<i32>() {
                if pid_alive(owner) && owner as u32 != pid {
                    anyhow::bail!(
                        "account {account_key} is locked by agent-trader pid {owner}"
                    );
                }
            }
        }
        fs::write(&path, pid.to_string())?;
        Ok(Self { path, pid })
    }
}

impl Drop for AccountLock {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path)
            .ok()
            .as_deref()
            .map(str::trim)
            == Some(self.pid.to_string()).as_deref()
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_pid_is_not_busy() {
        let dir = std::env::temp_dir().join(format!("agent-trader-guard-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agent-demo.pid"), "999999").unwrap();
        fs::write(dir.join("agent-state-demo.json"), "account HASH999").unwrap();
        let busy = legacy_account_busy(&dir, "HASH999").unwrap();
        assert!(busy.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_pid_with_account_is_busy() {
        let dir = std::env::temp_dir().join(format!("agent-trader-guard-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("trader-demo.pid"), std::process::id().to_string()).unwrap();
        fs::write(dir.join("trader-state-demo.json"), "hash PUB123 in book").unwrap();
        let busy = legacy_account_busy(&dir, "PUB123").unwrap();
        assert!(busy.is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
