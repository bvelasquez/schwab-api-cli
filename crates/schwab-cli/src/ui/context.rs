use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::agent::{
    daemon_status, default_state_path, load_agent_state, load_state, state_summary, DaemonStatus,
};
use crate::agent::state::AgentState;
use crate::rules::RulesConfig;

const LOG_TAIL_LINES: usize = 30;

#[derive(Debug, Clone)]
pub struct DashboardContext {
    pub rules_path: PathBuf,
    pub rules: RulesConfig,
    pub state: AgentState,
    pub state_path: PathBuf,
    pub daemon: DaemonStatus,
    pub log_tail: Vec<String>,
    /// Schwab option market hours (cached ~2 min); `None` if API unavailable.
    pub market_option_open: Option<bool>,
}

impl DashboardContext {
    pub fn load(rules_path: &Path) -> Result<Self> {
        let rules = RulesConfig::load(rules_path)?;
        let state_path = default_state_path(rules_path);
        let state = if state_path.exists() {
            load_state(&state_path)?
        } else {
            load_agent_state(rules_path, &rules.agent_id)
        };
        let daemon = daemon_status(rules_path);
        let log_tail = tail_lines(&daemon.log_file, LOG_TAIL_LINES);
        let market_option_open = super::market_status::fetch_option_market_open_cached();

        Ok(Self {
            rules_path: rules_path.to_path_buf(),
            rules,
            state,
            state_path,
            daemon,
            log_tail,
            market_option_open,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "rules_path": self.rules_path,
            "state_path": self.state_path,
            "daemon": {
                "running": self.daemon.running,
                "pid": self.daemon.pid,
                "pid_file": self.daemon.pid_file,
                "log_file": self.daemon.log_file,
            },
            "rules_summary": rules_summary_json(&self.rules),
            "state": state_summary(&self.state),
            "open_positions_detail": self.state.open_positions.values().collect::<Vec<_>>(),
            "log_tail": self.log_tail,
            "market_option_open": self.market_option_open,
        })
    }

    pub fn portfolio_risk_usd(&self) -> f64 {
        self.state
            .open_positions
            .values()
            .map(|p| p.max_loss_usd)
            .sum()
    }

    pub fn monitor_interval_minutes(&self) -> u64 {
        let secs = self.rules.llm.review_every_ticks.max(1)
            * self.rules.schedule.tick_interval_seconds.max(1);
        secs / 60
    }
}

pub fn rules_summary_json(rules: &RulesConfig) -> Value {
    json!({
        "agent_id": rules.agent_id,
        "watchlist": rules.watchlist,
        "tick_interval_seconds": rules.schedule.tick_interval_seconds,
        "overnight_enabled": rules.schedule.overnight.enabled,
        "vertical_enabled": rules.strategies.vertical.enabled,
        "iron_condor_enabled": rules.strategies.iron_condor.enabled,
        "llm_enabled": rules.llm.enabled,
        "selection_model": rules.llm.effective_selection_model(),
        "monitor_model": rules.llm.effective_monitor_model(),
        "review_every_ticks": rules.llm.review_every_ticks,
        "max_trades_per_day": rules.risk.max_trades_per_day,
        "max_portfolio_risk_usd": rules.risk.max_portfolio_risk_usd,
    })
}

pub fn tail_lines(path: &Path, n: usize) -> Vec<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .rev()
        .take(n)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
