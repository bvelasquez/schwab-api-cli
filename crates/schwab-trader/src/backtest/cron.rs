//! Generate cron entries and wrapper scripts for nightly backtest prefetch.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct PrefetchCronPlan {
    pub schedule: String,
    pub rules_file: PathBuf,
    pub script_path: PathBuf,
    pub log_path: PathBuf,
    pub crontab_line: String,
    pub script_body: String,
}

pub fn default_prefetch_schedule() -> &'static str {
    // 06:15 ET Mon–Fri — after US daily bars are usually available
    "15 6 * * 1-5"
}

pub fn build_prefetch_cron_plan(
    rules_path: &Path,
    schedule: Option<&str>,
    project_root: Option<&Path>,
) -> Result<PrefetchCronPlan> {
    let rules_file = fs::canonicalize(rules_path)
        .unwrap_or_else(|_| rules_path.to_path_buf());
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
    let script_path = rules_dir.join(format!("prefetch-{stem}.sh"));
    let log_path = rules_dir.join(format!("prefetch-{stem}.log"));

    let exe = std::env::current_exe().context("resolve schwab-trader binary path")?;
    let schedule = schedule
        .map(str::to_string)
        .unwrap_or_else(|| default_prefetch_schedule().to_string());

    let to = (Utc::now().date_naive() - Duration::days(1)).to_string();
    let from = (Utc::now().date_naive() - Duration::days(730)).to_string();

    let script_body = format!(
        r#"#!/usr/bin/env bash
# Auto-generated prefetch wrapper for {rules}
set -euo pipefail
cd "{root}"
export PATH="${{PATH}}:{exe_parent}"
LOG="{log}"
echo "=== $(date -Iseconds) prefetch start ===" >> "$LOG"
"{exe}" backtest prefetch \
  --rules-file "{rules}" \
  --from {from} \
  --to {to} \
  --json >> "$LOG" 2>&1
echo "=== $(date -Iseconds) prefetch done ===" >> "$LOG"
"#,
        rules = rules_file.display(),
        root = root.display(),
        exe_parent = exe.parent().unwrap_or(Path::new(".")).display(),
        exe = exe.display(),
        log = log_path.display(),
        from = from,
        to = to,
    );

    let crontab_line = format!(
        "{schedule} {script} # schwab-trader prefetch {stem}",
        schedule = schedule,
        script = script_path.display(),
        stem = stem,
    );

    Ok(PrefetchCronPlan {
        schedule,
        rules_file,
        script_path,
        log_path,
        crontab_line,
        script_body,
    })
}

pub fn write_prefetch_script(plan: &PrefetchCronPlan) -> Result<()> {
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

/// Append crontab line if not already present (non-destructive).
pub fn install_prefetch_cron(plan: &PrefetchCronPlan) -> Result<Value> {
    write_prefetch_script(plan)?;
    let existing = std::process::Command::new("crontab")
        .arg("-l")
        .output();
    let current = match existing {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    };
    if current
        .lines()
        .any(|l| l.contains(plan.script_path.to_string_lossy().as_ref()))
    {
        return Ok(json!({
            "installed": false,
            "reason": "crontab already contains this script",
            "crontab_line": plan.crontab_line,
            "script_path": plan.script_path,
        }));
    }
    let mut merged = current.trim_end().to_string();
    if !merged.is_empty() {
        merged.push('\n');
    }
    merged.push_str(&plan.crontab_line);
    merged.push('\n');

    let mut child = std::process::Command::new("crontab")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("spawn crontab -")?;
    use std::io::Write;
    child
        .stdin
        .take()
        .context("crontab stdin")?
        .write_all(merged.as_bytes())?;
    let status = child.wait().context("wait crontab")?;
    anyhow::ensure!(status.success(), "crontab install failed");
    Ok(json!({
        "installed": true,
        "crontab_line": plan.crontab_line,
        "script_path": plan.script_path,
        "log_path": plan.log_path,
        "schedule": plan.schedule,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_plan_contains_rules_and_schedule() {
        let dir = tempfile::TempDir::new().unwrap();
        let rules = dir.path().join("trader-test.yaml");
        std::fs::write(&rules, "version: 1\ntrader_id: test\n").unwrap();
        let plan = build_prefetch_cron_plan(&rules, None, Some(dir.path())).unwrap();
        assert!(plan.script_body.contains("backtest prefetch"));
        assert!(plan.crontab_line.contains("prefetch-trader-test.sh"));
    }
}
