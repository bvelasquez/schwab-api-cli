use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::text::Line;

use crate::agent::paths::{journal_path, log_path, state_path};
use crate::agent::state::{load_state, TraderState};
use crate::journal;
use crate::rules::TraderRules;

#[derive(Debug, Clone)]
pub struct WatchContext {
    pub rules_path: PathBuf,
    pub rules: TraderRules,
    pub state: TraderState,
    pub log_tail: Vec<String>,
    pub journal_tail: Vec<String>,
}

impl WatchContext {
    pub fn load(rules_path: &Path) -> Result<Self> {
        let rules = TraderRules::load(rules_path)?;
        let state = load_state(rules_path, &rules.trader_id)?;
        let log_tail = tail_file(&log_path(rules_path), 40);
        let journal_tail = journal::read_recent(rules_path, 15)?
            .into_iter()
            .map(|e| {
                let ty = e
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("event");
                let payload = e.get("payload").cloned().unwrap_or_default();
                format!("{ty}: {}", payload)
            })
            .collect();
        Ok(Self {
            rules_path: rules_path.to_path_buf(),
            rules,
            state,
            log_tail,
            journal_tail,
        })
    }

    pub fn state_file(&self) -> PathBuf {
        state_path(&self.rules_path)
    }

    pub fn journal_file(&self) -> PathBuf {
        journal_path(&self.rules_path)
    }

    pub fn last_tick(&self) -> Option<&serde_json::Value> {
        self.state.last_tick_result.as_ref()
    }

    pub fn capital_check(&self) -> Option<&serde_json::Value> {
        self.last_tick()
            .and_then(|t| t.get("capital_check"))
    }

    pub fn scan(&self) -> Option<&serde_json::Value> {
        self.last_tick().and_then(|t| t.get("scan"))
    }

    pub fn llm(&self) -> Option<&serde_json::Value> {
        self.last_tick().and_then(|t| t.get("llm"))
    }
}

fn tail_file(path: &Path, max_lines: usize) -> Vec<String> {
    if !path.is_file() {
        return vec![];
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let lines: Vec<String> = raw.lines().map(str::to_string).collect();
    if lines.len() <= max_lines {
        lines
    } else {
        lines[lines.len() - max_lines..].to_vec()
    }
}

pub fn header_line(
    ctx: &WatchContext,
    _agent_mode: &str,
    dry_run: bool,
    simulate: bool,
) -> Line<'static> {
    let mode = if simulate {
        "SIM"
    } else if dry_run {
        "DRY-RUN"
    } else {
        "LIVE"
    };
    Line::from(format!(
        " schwab-trader watch │ {} │ {} │ tick {} │ {} open ",
        ctx.rules.trader_id,
        mode,
        ctx.state.tick_count,
        ctx.state.open_positions.len(),
    ))
}
