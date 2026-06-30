use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::text::Line;

use crate::agent::paths::{journal_path, log_path, state_path};
use crate::agent::state::{load_state, TraderState};
use crate::journal;
use crate::rules::TraderRules;

use crate::ui::live::WatchLiveSnapshot;

#[derive(Debug, Clone)]
pub struct WatchContext {
    pub rules_path: PathBuf,
    pub rules: TraderRules,
    pub state: TraderState,
    pub log_tail: Vec<String>,
    pub journal_events: Vec<serde_json::Value>,
    pub live: Option<WatchLiveSnapshot>,
}

impl WatchContext {
    pub fn load(rules_path: &Path) -> Result<Self> {
        Self::load_with_live(rules_path, None)
    }

    pub fn load_with_live(
        rules_path: &Path,
        live: Option<WatchLiveSnapshot>,
    ) -> Result<Self> {
        let rules = TraderRules::load(rules_path)?;
        let state = load_state(rules_path, &rules.trader_id)?;
        let log_tail = tail_file(&log_path(rules_path), 40);
        let journal_events = journal::read_recent(rules_path, 20)?;
        Ok(Self {
            rules_path: rules_path.to_path_buf(),
            rules,
            state,
            log_tail,
            journal_events,
            live,
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

    pub fn llm_phase(&self) -> Option<&str> {
        self.last_tick()
            .and_then(|t| t.get("llm_phase"))
            .and_then(|v| v.as_str())
            .or_else(|| self.llm().and_then(|l| l.get("phase")).and_then(|v| v.as_str()))
    }

    pub fn session_label(&self) -> &str {
        self.last_tick()
            .and_then(|t| t.get("session"))
            .and_then(|v| v.as_str())
            .unwrap_or("—")
    }

    pub fn entry_block_reason(&self) -> Option<&str> {
        self.last_tick()
            .and_then(|t| t.get("entry_block_reason"))
            .and_then(|v| v.as_str())
    }

    pub fn market_open(&self) -> Option<bool> {
        self.last_tick()
            .and_then(|t| t.get("market_clock"))
            .and_then(|c| c.get("regular_session_open"))
            .and_then(|v| v.as_bool())
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
    let quotes = ctx
        .live
        .as_ref()
        .and_then(|l| l.last_fetch)
        .map(|t| {
            let age = (chrono::Utc::now() - t).num_seconds().max(0);
            format!(" │ quotes {age}s")
        })
        .unwrap_or_default();
    Line::from(format!(
        " schwab-trader watch │ {} │ {} │ tick {} │ {} open{quotes} ",
        ctx.rules.trader_id,
        mode,
        ctx.state.tick_count,
        ctx.state.open_positions.len(),
    ))
}
