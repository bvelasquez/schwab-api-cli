use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use crate::agent::paths::journal_path;

pub fn append_event(rules_path: &Path, event_type: &str, payload: Value) -> Result<()> {
    let path = journal_path(rules_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "type": event_type,
        "payload": payload,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open journal {}", path.display()))?;
    writeln!(file, "{}", line)?;
    Ok(())
}

pub fn read_recent(rules_path: &Path, limit: usize) -> Result<Vec<Value>> {
    let path = journal_path(rules_path);
    if !path.is_file() {
        return Ok(vec![]);
    }
    let raw = std::fs::read_to_string(&path)?;
    let mut lines: Vec<Value> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if lines.len() > limit {
        lines = lines.split_off(lines.len() - limit);
    }
    Ok(lines)
}

pub fn stats_from_journal(rules_path: &Path) -> Result<Value> {
    let events = read_recent(rules_path, 10_000)?;
    let mut entries = 0u32;
    let mut exits = 0u32;
    let mut adaptations = 0u32;
    for e in &events {
        match e.get("type").and_then(|v| v.as_str()) {
            Some("entry_filled") => entries += 1,
            Some("exit_filled") => exits += 1,
            Some("rule_auto_applied") => adaptations += 1,
            _ => {}
        }
    }
    Ok(serde_json::json!({
        "events_total": events.len(),
        "entries_filled": entries,
        "exits_filled": exits,
        "rule_adaptations": adaptations,
        "journal_path": journal_path(rules_path),
    }))
}

pub fn journal_path_display(rules_path: &Path) -> PathBuf {
    journal_path(rules_path)
}
