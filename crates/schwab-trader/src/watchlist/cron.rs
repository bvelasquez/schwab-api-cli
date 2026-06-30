//! Generate cron entries and wrapper scripts for weekly watchlist refresh.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct WatchlistCronPlan {
    pub schedule: String,
    pub rules_file: PathBuf,
    pub script_path: PathBuf,
    pub log_path: PathBuf,
    pub crontab_line: String,
    pub script_body: String,
}

pub fn default_watchlist_schedule() -> &'static str {
    // 18:00 ET Sunday — weekly thematic refresh
    "0 18 * * 0"
}

pub fn build_watchlist_cron_plan(
    rules_path: &Path,
    schedule: Option<&str>,
    project_root: Option<&Path>,
) -> Result<WatchlistCronPlan> {
    let rules_file = fs::canonicalize(rules_path).unwrap_or_else(|_| rules_path.to_path_buf());
    let rules_dir = rules_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = project_root
        .map(Path::to_path_buf)
        .unwrap_or(rules_dir.clone());

    let stem = rules_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("trader");
    let script_path = rules_dir.join(format!("watchlist-refresh-{stem}.sh"));
    let log_path = rules_dir.join(format!("watchlist-refresh-{stem}.log"));

    let exe = std::env::current_exe().context("resolve schwab-trader binary path")?;
    let schedule = schedule
        .map(str::to_string)
        .unwrap_or_else(|| default_watchlist_schedule().to_string());

    let script_body = format!(
        r#"#!/usr/bin/env bash
# Auto-generated watchlist refresh for {rules}
set -euo pipefail
cd "{root}"
export PATH="${{PATH}}:{exe_parent}"
LOG="{log}"
echo "=== $(date -Iseconds) watchlist build start ===" >> "$LOG"
"{exe}" watchlist build \
  --rules-file "{rules}" \
  --write \
  --target thematic \
  --json >> "$LOG" 2>&1
echo "=== $(date -Iseconds) watchlist build done ===" >> "$LOG"
"#,
        rules = rules_file.display(),
        root = root.display(),
        exe_parent = exe.parent().unwrap_or(Path::new(".")).display(),
        exe = exe.display(),
        log = log_path.display(),
    );

    let crontab_line = format!(
        "{schedule} {script} # schwab-trader watchlist refresh {stem}",
        script = script_path.display(),
    );

    Ok(WatchlistCronPlan {
        schedule,
        rules_file,
        script_path,
        log_path,
        crontab_line,
        script_body,
    })
}

pub fn write_refresh_script(plan: &WatchlistCronPlan) -> Result<()> {
    fs::write(&plan.script_path, &plan.script_body)
        .with_context(|| format!("write {}", plan.script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&plan.script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plan.script_path, perms)?;
    }
    Ok(())
}

pub fn plan_to_json(plan: &WatchlistCronPlan) -> Value {
    json!({
        "schedule": plan.schedule,
        "rules_file": plan.rules_file,
        "script_path": plan.script_path,
        "log_path": plan.log_path,
        "crontab_line": plan.crontab_line,
    })
}
