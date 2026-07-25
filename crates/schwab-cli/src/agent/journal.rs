use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::paths::{backtest_journal_path, journal_path, sim_journal_path};

pub fn append_event(rules_path: &Path, simulate: bool, event_type: &str, payload: Value) -> Result<()> {
    append_event_at(rules_path, simulate, Utc::now(), event_type, payload)
}

pub fn append_backtest_event_at(
    rules_path: &Path,
    at: DateTime<Utc>,
    event_type: &str,
    payload: Value,
) -> Result<()> {
    write_journal_line(&backtest_journal_path(rules_path), at, event_type, payload)
}

pub fn append_event_at(
    rules_path: &Path,
    simulate: bool,
    at: DateTime<Utc>,
    event_type: &str,
    payload: Value,
) -> Result<()> {
    let path = if simulate {
        sim_journal_path(rules_path)
    } else {
        journal_path(rules_path)
    };
    write_journal_line(&path, at, event_type, payload)
}

fn write_journal_line(
    path: &Path,
    at: DateTime<Utc>,
    event_type: &str,
    payload: Value,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::json!({
        "ts": at.to_rfc3339(),
        "type": event_type,
        "payload": payload,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open journal {}", path.display()))?;
    writeln!(file, "{line}")?;
    Ok(())
}

pub fn read_all_backtest(rules_path: &Path) -> Result<Vec<Value>> {
    let path = backtest_journal_path(rules_path);
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read backtest journal {}", path.display()))?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

pub fn clear_backtest_journal(rules_path: &Path) -> Result<()> {
    let path = backtest_journal_path(rules_path);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("clear backtest journal {}", path.display()))?;
    }
    Ok(())
}
