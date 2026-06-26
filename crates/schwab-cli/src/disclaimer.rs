use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;

/// Appended to `schwab --help` / `long_about`.
pub const HELP_DISCLAIMER: &str = "\n\
⚠️  USE AT YOUR OWN RISK — EXPERIMENTAL SOFTWARE\n\
This CLI can submit real trades. There is no warranty; authors are not liable for losses.\n\
Run `schwab disclaimer show` and `schwab disclaimer accept --yes` before live trading.\n\
Not financial advice. Not affiliated with Charles Schwab & Co., Inc.";

pub const FULL_DISCLAIMER: &str = r#"SCHWAB TRADER API CLI — RISK DISCLAIMER

USE AT YOUR OWN RISK. This software is EXPERIMENTAL and under active development.

• Real orders: Live trading (--trust --yes, trade plans, options agent) can move money in
  your brokerage account. Bugs, misconfiguration, API issues, and LLM errors can cause loss.

• No advice: Nothing here is financial, investment, tax, or legal advice.

• Your responsibility: You are solely responsible for orders, compliance, monitoring
  agents, and securing API credentials.

• No liability: Authors and contributors are not liable for damages or financial loss.

• No affiliation: Not endorsed by Charles Schwab & Co., Inc.

• No warranty: Provided "AS IS" under the MIT License.

Before live trading: schwab disclaimer accept --yes
"#;

const FIRST_RUN_PREFIX: &str = "⚠️  FIRST RUN — READ BEFORE TRADING\n\n";

/// Path to marker file recording explicit disclaimer acceptance.
pub fn accepted_marker_path() -> PathBuf {
    config_dir().join("disclaimer-accepted")
}

/// Path to marker file recording that the first-run banner was shown.
fn shown_marker_path() -> PathBuf {
    config_dir().join("disclaimer-shown")
}

fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SCHWAB_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("", "", "schwabinvestbot")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".schwabinvestbot"))
}

pub fn is_accepted() -> bool {
    if std::env::var_os("SCHWAB_DISCLAIMER_ACCEPTED").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    accepted_marker_path().is_file()
}

fn has_been_shown() -> bool {
    shown_marker_path().is_file()
}

pub fn mark_accepted() -> Result<PathBuf> {
    let path = accepted_marker_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
    }
    let body = format!("accepted_at={}\nversion=1\n", Utc::now().to_rfc3339());
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    // Showing acceptance implies the banner was seen.
    let _ = mark_shown();
    Ok(path)
}

fn mark_shown() -> Result<()> {
    let path = shown_marker_path();
    if path.is_file() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
    }
    fs::write(&path, Utc::now().to_rfc3339())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Print the first-run warning once per machine (stderr).
pub fn maybe_show_first_run() -> Result<()> {
    if has_been_shown() {
        return Ok(());
    }
    let mut out = std::io::stderr();
    writeln!(out, "{FIRST_RUN_PREFIX}{FULL_DISCLAIMER}")?;
    if !is_accepted() {
        writeln!(
            out,
            "Live trading is blocked until you run:\n  schwab disclaimer accept --yes\n"
        )?;
    }
    mark_shown()?;
    Ok(())
}

/// Required before any non-dry-run trading mutation.
pub fn require_accepted_for_live_trading() -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    if is_accepted() {
        return Ok(());
    }
    anyhow::bail!(
        "Live trading blocked: risk disclaimer not accepted.\n\
         Run: schwab disclaimer show\n\
         Then: schwab disclaimer accept --yes\n\
         (Or set SCHWAB_DISCLAIMER_ACCEPTED=1 for automation you control.)"
    );
}

pub fn status_json() -> serde_json::Value {
    serde_json::json!({
        "accepted": is_accepted(),
        "first_run_banner_shown": has_been_shown(),
        "accepted_marker": accepted_marker_path(),
        "accept_command": "schwab disclaimer accept --yes",
        "show_command": "schwab disclaimer show",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn with_temp_config<F: FnOnce()>(f: F) {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SCHWAB_CONFIG_DIR", dir.path());
        std::env::remove_var("SCHWAB_DISCLAIMER_ACCEPTED");
        f();
        std::env::remove_var("SCHWAB_CONFIG_DIR");
    }

    #[test]
    fn acceptance_marker_written() {
        let _g = test_guard();
        with_temp_config(|| {
            assert!(!is_accepted());
            let path = mark_accepted().unwrap();
            assert!(path.is_file());
            assert!(is_accepted());
        });
    }

    #[test]
    fn env_override_skips_marker() {
        let _g = test_guard();
        with_temp_config(|| {
            std::env::set_var("SCHWAB_DISCLAIMER_ACCEPTED", "1");
            assert!(is_accepted());
        });
    }

    #[test]
    fn first_run_only_once() {
        let _g = test_guard();
        with_temp_config(|| {
            assert!(!has_been_shown());
            maybe_show_first_run().unwrap();
            assert!(has_been_shown());
            // Second call is a no-op (no panic).
            maybe_show_first_run().unwrap();
        });
    }
}
