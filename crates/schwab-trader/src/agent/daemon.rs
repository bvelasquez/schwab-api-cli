use std::path::Path;

use anyhow::{Context, Result};

use super::paths::{log_path, pid_path};

/// Forward global flags into the detached child. Never injects `--trust` / `--yes`.
pub fn background_extra_args(
    dry_run: bool,
    simulate: bool,
    trust: bool,
    yes: bool,
    json: bool,
    no_audio: bool,
) -> Vec<String> {
    let mut extra = Vec::new();
    if dry_run {
        extra.push("--dry-run".into());
    }
    if simulate {
        extra.push("--simulate".into());
    }
    if trust {
        extra.push("--trust".into());
    }
    if yes {
        extra.push("--yes".into());
    }
    if json {
        extra.push("--json".into());
    }
    if no_audio {
        extra.push("--no-audio".into());
    }
    extra
}

pub fn spawn_background(rules_path: &Path, extra_args: &[String]) -> Result<u32> {
    let exe = std::env::current_exe().context("current exe")?;
    let mut args: Vec<std::ffi::OsString> = vec!["agent".into(), "run".into(), rules_path.into()];
    args.extend(extra_args.iter().map(std::ffi::OsString::from));
    schwab_cli::agent::daemon::spawn_detached(
        exe,
        &args,
        &pid_path(rules_path),
        &log_path(rules_path),
    )
}

pub fn stop_daemon(rules_path: &Path) -> Result<()> {
    schwab_cli::agent::daemon::stop_pid_file(&pid_path(rules_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulate_is_forwarded_and_trust_is_not_implied() {
        let extra = background_extra_args(false, true, false, false, true, false);
        assert_eq!(extra, vec!["--simulate".to_string(), "--json".into()]);
        assert!(!extra.iter().any(|a| a == "--trust" || a == "--yes"));
    }

    #[test]
    fn trader_runtime_files_sit_next_to_rules() {
        let rules = Path::new("rules/trader-swing-9947.yaml");
        assert_eq!(
            pid_path(rules),
            Path::new("rules/trader-trader-swing-9947.pid")
        );
        assert_eq!(
            log_path(rules),
            Path::new("rules/trader-trader-swing-9947.log")
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawn_background_refuses_live_pid() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("trader-swing-9947.yaml");
        std::fs::write(&rules, "stub: true\n").unwrap();
        let pid_file = pid_path(&rules);
        std::fs::write(&pid_file, std::process::id().to_string()).unwrap();
        let err = spawn_background(&rules, &["--simulate".into()]).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "expected already-running, got {err}"
        );
    }
}
