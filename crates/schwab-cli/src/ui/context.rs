use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};

use crate::agent::state::AgentState;
use crate::agent::{
    daemon_status, default_state_path, load_agent_state, load_state, state_summary, DaemonStatus,
};
use crate::auth_reminder::{load_auth_reminder, AuthReminder};
use crate::market_hours::ResolvedMarketStatus;
use crate::rules::RulesConfig;
use crate::ui::market_status::{self, MarketSnapshot};

const LOG_TAIL_LINES: usize = 30;

#[derive(Debug, Clone)]
pub struct DashboardContext {
    pub rules_path: PathBuf,
    pub rules: RulesConfig,
    pub state: AgentState,
    pub state_path: PathBuf,
    pub daemon: DaemonStatus,
    pub log_tail: Vec<String>,
    pub market_status: ResolvedMarketStatus,
    pub auth_reminder: Option<AuthReminder>,
}

impl DashboardContext {
    pub fn load(rules_path: &Path) -> Result<Self> {
        Self::load_with_snapshot(rules_path, None)
    }

    pub fn load_with_snapshot(
        rules_path: &Path,
        live_market: Option<&MarketSnapshot>,
    ) -> Result<Self> {
        let rules = RulesConfig::load(rules_path)?;
        let state_path = default_state_path(rules_path);
        let state = if state_path.exists() {
            load_state(&state_path)?
        } else {
            load_agent_state(rules_path, &rules.agent_id)
        };
        let daemon = daemon_status(rules_path);
        let log_tail = tail_lines(&daemon.log_file, LOG_TAIL_LINES);
        let market_status = market_status::resolve_market_status(rules_path, &state, live_market);
        let auth_reminder = load_auth_reminder();

        Ok(Self {
            rules_path: rules_path.to_path_buf(),
            rules,
            state,
            state_path,
            daemon,
            log_tail,
            market_status,
            auth_reminder,
        })
    }

    pub fn load_with_shared_snapshot(
        rules_path: &Path,
        live_market: &Arc<Mutex<MarketSnapshot>>,
    ) -> Result<Self> {
        let snapshot = live_market.lock().ok().map(|g| g.clone());
        Self::load_with_snapshot(rules_path, snapshot.as_ref())
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
            "market_status": {
                "open": self.market_status.open,
                "source": format!("{:?}", self.market_status.source),
            },
            "auth_reminder": self.auth_reminder.as_ref().map(|r| json!({
                "level": r.level.as_str(),
                "message": r.message,
                "obtained_at": r.obtained_at,
                "access_expires_in_seconds": r.access_expires_in_seconds,
                "refresh_expires_in_seconds": r.refresh_expires_in_seconds,
                "detail": r.detail_line(),
            })),
        })
    }

    pub fn portfolio_risk_usd(&self) -> f64 {
        self.state.reserved_risk_usd()
    }

    pub fn monitor_interval_minutes(&self) -> u64 {
        self.rules.llm.monitor_interval_minutes(
            self.rules.schedule.tick_interval_seconds,
            self.min_open_position_dte(),
            self.rules.exit_rules.dte_close,
        )
    }

    pub fn min_open_position_dte(&self) -> Option<i64> {
        let today = chrono::Local::now().date_naive();
        self.state
            .open_positions
            .values()
            .filter_map(|p| {
                chrono::NaiveDate::parse_from_str(&p.expiry, "%Y-%m-%d")
                    .ok()
                    .map(|exp| crate::options::days_to_expiry(exp, today))
            })
            .min()
    }

    pub fn has_open_positions(&self) -> bool {
        !self.state.open_positions.is_empty()
    }

    /// Session the agent should be in right now (from market hours, not stale state file).
    pub fn effective_session(&self) -> &'static str {
        if self.market_status.open {
            "regular"
        } else if self.rules.schedule.overnight.enabled {
            "overnight"
        } else {
            "idle"
        }
    }

    pub fn expected_tick_interval_secs(&self) -> u64 {
        if self.market_status.open {
            self.rules.schedule.tick_interval_seconds.max(5)
        } else if self.rules.schedule.overnight.enabled {
            self.rules.schedule.overnight.tick_interval_seconds.max(300)
        } else {
            self.rules.schedule.tick_interval_seconds.max(5)
        }
    }

    pub fn last_tick_age_secs(&self) -> Option<i64> {
        self.state
            .last_tick
            .map(|at| (chrono::Utc::now() - at).num_seconds())
    }

    pub fn tick_is_stale(&self) -> bool {
        let interval = self.expected_tick_interval_secs() as i64;
        let threshold = interval * 2 + 60;
        match self.last_tick_age_secs() {
            Some(age) => age > threshold,
            None => true,
        }
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
