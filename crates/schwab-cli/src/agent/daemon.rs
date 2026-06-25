use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use super::paths::{log_path, pid_path};

pub fn spawn_background(rules_path: &Path, extra_args: &[String]) -> Result<u32> {
    let pid_file = pid_path(rules_path);
    if let Ok(existing) = read_pid(&pid_file) {
        if process_alive(existing) {
            anyhow::bail!(
                "agent already running with pid {existing} (pid file: {})",
                pid_file.display()
            );
        }
    }

    let exe = std::env::current_exe().context("current exe")?;
    let log = log_path(rules_path);
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("open log {}", log.display()))?;

    let err_file = log_file
        .try_clone()
        .with_context(|| format!("clone log handle {}", log.display()))?;

    let mut cmd = Command::new(&exe);
    cmd.arg("agent")
        .arg("run")
        .arg(rules_path)
        .args(extra_args)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(err_file));

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
    fs::write(&pid_file, pid.to_string())?;
    Ok(pid)
}

pub fn stop_daemon(rules_path: &Path) -> Result<()> {
    let pid_file = pid_path(rules_path);
    let pid = read_pid(&pid_file).with_context(|| {
        format!(
            "no running agent (missing pid file at {})",
            pid_file.display()
        )
    })?;

    if !process_alive(pid) {
        fs::remove_file(&pid_file).ok();
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
            fs::remove_file(&pid_file).ok();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        fs::remove_file(&pid_file).ok();
    }

    Ok(())
}

fn read_pid(path: &Path) -> Result<u32> {
    let content = fs::read_to_string(path)?;
    content
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid pid in {}", path.display()))
}

fn process_alive(pid: u32) -> bool {
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
