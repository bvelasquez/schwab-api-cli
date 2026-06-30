use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::paths::{journal_path, sim_journal_path};

pub fn append_event(rules_path: &Path, simulate: bool, event_type: &str, payload: Value) -> Result<()> {
    append_event_at(rules_path, simulate, Utc::now(), event_type, payload)
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
        .open(&path)
        .with_context(|| format!("open journal {}", path.display()))?;
    writeln!(file, "{line}")?;
    Ok(())
}
