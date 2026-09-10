use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use super::paths::{log_path, pid_path};

/// Detach a child process (new session on Unix), redirect stdio to `log_file`, write `pid_file`.
/// Shared by the options CLI and `schwab-trader` so stop/reload stay consistent.
pub fn spawn_detached(
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    pid_file: &Path,
    log_file: &Path,
) -> Result<u32> {
    if let Ok(existing) = read_pid(pid_file) {
        if process_alive(existing) {
            anyhow::bail!(
                "agent already running with pid {existing} (pid file: {})",
                pid_file.display()
            );
        }
    }

    if let Some(parent) = log_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create log dir {}", parent.display()))?;
    }
    if let Some(parent) = pid_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create pid dir {}", parent.display()))?;
    }

    let stdout = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("open log {}", log_file.display()))?;

    let stderr = stdout
        .try_clone()
        .with_context(|| format!("clone log handle {}", log_file.display()))?;

    let mut cmd = Command::new(program.as_ref());
    cmd.args(args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd.spawn().context("spawn background agent")?;
    let pid = child.id();
    fs::write(pid_file, pid.to_string())?;
    Ok(pid)
}

pub fn spawn_background(rules_path: &Path, extra_args: &[String]) -> Result<u32> {
    let exe = std::env::current_exe().context("current exe")?;
    let mut args: Vec<std::ffi::OsString> = vec!["agent".into(), "run".into(), rules_path.into()];
    args.extend(extra_args.iter().map(std::ffi::OsString::from));
    spawn_detached(exe, &args, &pid_path(rules_path), &log_path(rules_path))
}

pub fn stop_pid_file(pid_file: &Path) -> Result<()> {
    let pid = read_pid(pid_file).with_context(|| {
        format!(
            "no running agent (missing pid file at {})",
            pid_file.display()
        )
    })?;

    if !process_alive(pid) {
        fs::remove_file(pid_file).ok();
        anyhow::bail!("agent pid {pid} is not running; removed stale pid file");
    }

    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if rc != 0 {
            anyhow::bail!("failed to send SIGTERM to pid {pid}");
        }
    }

    #[cfg(not(unix))]
    {
        anyhow::bail!("stop is only supported on Unix");
    }

    for _ in 0..20 {
        if !process_alive(pid) {
            fs::remove_file(pid_file).ok();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        fs::remove_file(pid_file).ok();
    }

    Ok(())
}

pub fn stop_daemon(rules_path: &Path) -> Result<()> {
    stop_pid_file(&pid_path(rules_path))
}

/// Signal a running agent (background `agent run` or a loop that wrote this pid file)
/// to reload rules YAML. The process stays up; invalid YAML is ignored by the loop.
pub fn request_reload(rules_path: &Path) -> Result<u32> {
    let pid_file = pid_path(rules_path);
    crate::rules_reload::send_sighup_to_pid_file(&pid_file).with_context(|| {
        format!(
            "no running agent pid at {} — for `schwab watch`, send SIGHUP to that process, or wait for the next file-poll (≤1s during sleep)",
            pid_file.display()
        )
    })
}

fn read_pid(path: &Path) -> Result<u32> {
    let content = fs::read_to_string(path)?;
    content
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid pid in {}", path.display()))
}

pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Background agent process metadata for dashboard / status UIs.
#[derive(Debug, Clone)]
pub struct DaemonStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub pid_file: std::path::PathBuf,
    pub log_file: std::path::PathBuf,
}

pub fn daemon_status_at(pid_file: std::path::PathBuf, log_file: std::path::PathBuf) -> DaemonStatus {
    match read_pid(&pid_file) {
        Ok(pid) if process_alive(pid) => DaemonStatus {
            running: true,
            pid: Some(pid),
            pid_file,
            log_file,
        },
        Ok(pid) => DaemonStatus {
            running: false,
            pid: Some(pid),
            pid_file,
            log_file,
        },
        Err(_) => DaemonStatus {
            running: false,
            pid: None,
            pid_file,
            log_file,
        },
    }
}

pub fn daemon_status(rules_path: &Path) -> DaemonStatus {
    daemon_status_at(pid_path(rules_path), log_path(rules_path))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn spawn_detached_writes_pid_and_stop_kills() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("agent-test.pid");
        let log_file = dir.path().join("agent-test.log");
        let pid = spawn_detached("sleep", ["8"], &pid_file, &log_file).unwrap();
        assert!(process_alive(pid), "child should be alive");
        assert_eq!(
            fs::read_to_string(&pid_file).unwrap().trim(),
            pid.to_string()
        );
        assert!(log_file.exists());

        let err = spawn_detached("sleep", ["8"], &pid_file, &log_file).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "duplicate spawn: {err}"
        );

        stop_pid_file(&pid_file).unwrap();
        // Reap so kill(0) does not treat a zombie as alive (parent is this test process).
        unsafe {
            libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
        }
        assert!(!process_alive(pid), "child should be stopped");
        assert!(!pid_file.exists());
    }

    #[test]
    fn stale_pid_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("stale.pid");
        let log_file = dir.path().join("stale.log");
        fs::write(&pid_file, "999999999\n").unwrap();
        let pid = spawn_detached("sleep", ["8"], &pid_file, &log_file).unwrap();
        assert_ne!(pid, 999_999_999);
        stop_pid_file(&pid_file).unwrap();
        unsafe {
            libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
        }
    }

    #[test]
    fn options_paths_stay_next_to_rules() {
        let rules = PathBuf::from("rules/options-pilot-8709.yaml");
        assert_eq!(
            pid_path(&rules),
            PathBuf::from("rules/agent-options-pilot-8709.pid")
        );
        assert_eq!(
            log_path(&rules),
            PathBuf::from("rules/agent-options-pilot-8709.log")
        );
    }
}
