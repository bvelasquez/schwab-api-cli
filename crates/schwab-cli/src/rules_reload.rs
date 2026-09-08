//! Hot-reload helpers for rules YAML used by standing watch / agent loops.
//!
//! Invalid files keep the previously loaded rules (fail-closed). Code/binary
//! changes still require a rebuild and process restart.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;

/// Snapshot of watched rules files (content hash + latest mtime for logs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesFingerprint {
    pub digest: u64,
    pub latest_mtime: Option<SystemTime>,
    pub files: Vec<PathBuf>,
}

impl RulesFingerprint {
    pub fn of(paths: &[PathBuf]) -> Self {
        let mut hasher = DefaultHasher::new();
        let mut latest_mtime = None::<SystemTime>;
        for path in paths {
            path.hash(&mut hasher);
            match fs::read(path) {
                Ok(bytes) => {
                    bytes.hash(&mut hasher);
                    if let Ok(meta) = fs::metadata(path) {
                        if let Ok(mtime) = meta.modified() {
                            latest_mtime = Some(match latest_mtime {
                                Some(prev) => prev.max(mtime),
                                None => mtime,
                            });
                        }
                    }
                }
                Err(_) => {
                    // Missing include: hash a sentinel so create/delete is detected.
                    0u8.hash(&mut hasher);
                }
            }
        }
        Self {
            digest: hasher.finish(),
            latest_mtime,
            files: paths.to_vec(),
        }
    }

    pub fn mtime_debug(&self) -> String {
        match self.latest_mtime {
            Some(t) => format!("{t:?}"),
            None => "unknown".into(),
        }
    }
}

#[derive(Debug)]
pub enum ReloadAttempt<T> {
    Unchanged,
    Loaded { value: T, summary: String },
    Failed { error: String },
}

/// Tracks rules files on disk and reloads them when content changes or SIGHUP fires.
pub struct RulesReloader {
    paths: Vec<PathBuf>,
    fingerprint: RulesFingerprint,
    force: Arc<AtomicBool>,
}

impl RulesReloader {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        let fingerprint = RulesFingerprint::of(&paths);
        Self {
            paths,
            fingerprint,
            force: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Unix: SIGHUP sets the force-reload flag (does not kill the process).
    pub fn spawn_sighup_listener(&self) {
        #[cfg(unix)]
        {
            let flag = self.force.clone();
            tokio::spawn(async move {
                let mut signals =
                    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
                        Ok(s) => s,
                        Err(err) => {
                            tracing::warn!("rules reload: could not listen for SIGHUP: {err}");
                            return;
                        }
                    };
                while signals.recv().await.is_some() {
                    flag.store(true, Ordering::SeqCst);
                }
            });
        }
    }

    pub fn set_paths(&mut self, paths: Vec<PathBuf>) {
        self.paths = paths;
        self.fingerprint = RulesFingerprint::of(&self.paths);
    }

    /// Re-read hashes after this process wrote the YAML (avoid a spurious reload log).
    pub fn resync(&mut self) {
        self.fingerprint = RulesFingerprint::of(&self.paths);
    }

    pub fn take_force(&self) -> bool {
        self.force.swap(false, Ordering::SeqCst)
    }

    pub fn force_pending(&self) -> bool {
        self.force.load(Ordering::SeqCst)
    }

    pub fn disk_changed(&self) -> bool {
        RulesFingerprint::of(&self.paths) != self.fingerprint
    }

    pub fn try_load<T>(
        &mut self,
        force: bool,
        load: impl FnOnce() -> Result<T>,
    ) -> ReloadAttempt<T> {
        let next = RulesFingerprint::of(&self.paths);
        if !force && next.digest == self.fingerprint.digest {
            return ReloadAttempt::Unchanged;
        }
        match load() {
            Ok(value) => {
                let files = self
                    .paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let summary = format!(
                    "rules reloaded from [{files}] mtime={} digest={:x}",
                    next.mtime_debug(),
                    next.digest
                );
                self.fingerprint = next;
                ReloadAttempt::Loaded { value, summary }
            }
            Err(err) => {
                // Fingerprint the bad content so we log once until the file changes again.
                self.fingerprint = next;
                ReloadAttempt::Failed {
                    error: format!("{err:#}"),
                }
            }
        }
    }
}

/// Sleep until the next tick, returning early on SIGHUP or a watched-file change.
pub async fn wait_for_next_tick(
    duration: Duration,
    reloader: &RulesReloader,
) -> TickWaitReason {
    if duration.is_zero() {
        return TickWaitReason::Timeout;
    }
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if reloader.force_pending() {
            return TickWaitReason::Forced;
        }
        if reloader.disk_changed() {
            return TickWaitReason::FileChanged;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return TickWaitReason::Timeout;
        }
        let remaining = deadline - now;
        let slice = remaining.min(Duration::from_secs(1));
        tokio::time::sleep(slice).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickWaitReason {
    Timeout,
    FileChanged,
    Forced,
}

/// Paths to watch for the options agent (the rules file itself).
pub fn options_watch_paths(rules_path: &Path) -> Vec<PathBuf> {
    vec![rules_path.to_path_buf()]
}

/// Send SIGHUP to the pid recorded next to a rules file (background `agent run`).
pub fn send_sighup_to_pid_file(pid_file: &Path) -> Result<u32> {
    let content = fs::read_to_string(pid_file)?;
    let pid: u32 = content.trim().parse().map_err(|_| {
        anyhow::anyhow!("invalid pid in {}", pid_file.display())
    })?;

    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, libc::SIGHUP) };
        if rc != 0 {
            anyhow::bail!("failed to send SIGHUP to pid {pid}");
        }
        Ok(pid)
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        anyhow::bail!("SIGHUP reload is only supported on Unix");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn fingerprint_changes_with_content() {
        let f = write_temp("a: 1\n");
        let path = f.path().to_path_buf();
        let first = RulesFingerprint::of(&[path.clone()]);
        std::fs::write(&path, "a: 2\n").unwrap();
        let second = RulesFingerprint::of(&[path]);
        assert_ne!(first.digest, second.digest);
    }

    #[test]
    fn reload_swaps_on_valid_change_and_keeps_old_on_bad_yaml() {
        let f = write_temp("ok: true\n");
        let path = f.path().to_path_buf();
        let mut reloader = RulesReloader::new(vec![path.clone()]);

        let first = match reloader.try_load(true, || {
            let s = std::fs::read_to_string(&path)?;
            anyhow::ensure!(s.contains("ok: true"), "expected ok");
            Ok(s)
        }) {
            ReloadAttempt::Loaded { value, .. } => value,
            other => panic!("expected loaded, got {other:?}"),
        };
        assert!(first.contains("ok: true"));

        std::fs::write(&path, "ok: false\n").unwrap();
        match reloader.try_load(false, || Ok(std::fs::read_to_string(&path)?)) {
            ReloadAttempt::Loaded { value, summary } => {
                assert!(value.contains("ok: false"));
                assert!(summary.contains("rules reloaded"));
            }
            other => panic!("expected loaded, got {other:?}"),
        }

        std::fs::write(&path, ": not valid yaml [[").unwrap();
        match reloader.try_load(false, || {
            let s = std::fs::read_to_string(&path)?;
            let _: serde_yaml::Value = serde_yaml::from_str(&s)?;
            Ok(s)
        }) {
            ReloadAttempt::Failed { error } => {
                assert!(!error.is_empty());
            }
            other => panic!("expected failed, got {other:?}"),
        }

        // Same broken content: unchanged (no log spam).
        match reloader.try_load(false, || -> Result<String> { unreachable!("should not load") }) {
            ReloadAttempt::Unchanged => {}
            other => panic!("expected unchanged, got {other:?}"),
        }
    }

    #[test]
    fn force_reloads_without_content_change() {
        let f = write_temp("n: 1\n");
        let path = f.path().to_path_buf();
        let mut reloader = RulesReloader::new(vec![path.clone()]);
        let _ = reloader.try_load(true, || Ok(std::fs::read_to_string(&path)?));
        match reloader.try_load(false, || -> Result<String> { unreachable!("no change") }) {
            ReloadAttempt::Unchanged => {}
            other => panic!("{other:?}"),
        }
        match reloader.try_load(true, || Ok(std::fs::read_to_string(&path)?)) {
            ReloadAttempt::Loaded { value, .. } => assert!(value.contains("n: 1")),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn wait_wakes_when_rules_file_changes() {
        let f = write_temp("n: 1\n");
        let path = f.path().to_path_buf();
        let reloader = RulesReloader::new(vec![path.clone()]);
        let write_path = path.clone();
        let wait = wait_for_next_tick(Duration::from_secs(30), &reloader);
        let mutate = async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            std::fs::write(&write_path, "n: 2\n").unwrap();
        };
        tokio::pin!(wait);
        tokio::pin!(mutate);
        let reason = tokio::select! {
            r = &mut wait => r,
            _ = &mut mutate => {
                wait.await
            }
        };
        assert_eq!(reason, TickWaitReason::FileChanged);
    }
}
