use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::paths::{backtest_state_path, state_path};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraderState {
    pub trader_id: String,
    pub last_tick: Option<DateTime<Utc>>,
    pub trades_today: u32,
    pub trades_day: Option<NaiveDate>,
    pub open_positions: HashMap<String, SwingPosition>,
    pub pending_buys: Vec<PendingBuy>,
    pub dynamic_watchlist: Vec<String>,
    pub tick_count: u64,
    pub last_llm_review_tick: Option<u64>,
    pub last_llm_summary: Option<Value>,
    pub last_web_picks: Option<Value>,
    #[serde(default)]
    pub last_actions: Vec<Value>,
    #[serde(default)]
    pub last_tick_result: Option<Value>,
    /// Paper-trading ledger when running with --simulate
    #[serde(default)]
    pub sim: Option<crate::sim::SimLedger>,
    #[serde(default)]
    pub last_learn_tick: Option<u64>,
    #[serde(default)]
    pub closed_trades_since_learn: u32,
    /// Positions filled but missing a working OCO bracket.
    #[serde(default)]
    pub unbracketed_positions: HashMap<String, UnbracketedPosition>,
    /// When set, new entries are blocked until cleared.
    #[serde(default)]
    pub trading_halted_reason: Option<String>,
    #[serde(default)]
    pub sleeve_peak_equity_usd: f64,
    #[serde(default)]
    pub reconcile_mismatch_count: u32,
    #[serde(default)]
    pub web_picks_today: u32,
    #[serde(default)]
    pub web_picks_day: Option<NaiveDate>,
    /// Seconds from fill to OCO placement on last entry (monitoring).
    #[serde(default)]
    pub last_fill_to_bracket_seconds: Option<u64>,
    #[serde(default)]
    pub last_session: Option<String>,
    #[serde(default)]
    pub regular_tick_count: u64,
    #[serde(default)]
    pub llm_review_count: u64,
    #[serde(default)]
    pub last_overnight_digest_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_premarket_digest_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub open_playbook: Option<Value>,
    /// Active regime profile name (e.g. low_vol_trend, baseline).
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub active_profile_source: Option<String>,
    #[serde(default)]
    pub active_profile_reason: Option<String>,
    #[serde(default)]
    pub last_regime: Option<Value>,
    /// Consecutive entries where max_position_pct (not risk_pct) bound sizing — sim diagnostics.
    #[serde(default)]
    pub sizing_max_pct_binding_streak: u32,
    #[serde(default)]
    pub sizing_redundant_risk_warned: bool,
    /// After a thesis exit — prioritize scan on this symbol for redeploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redeploy_signal: Option<RedeploySignal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedeploySignal {
    pub at: DateTime<Utc>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlying: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwingPosition {
    pub position_id: String,
    pub symbol: String,
    pub account_hash: String,
    pub quantity: f64,
    pub entry_price: f64,
    pub opened_at: DateTime<Utc>,
    pub stop_price: f64,
    pub profit_limit: f64,
    pub stop_risk_usd: f64,
    pub market_value_usd: f64,
    #[serde(default)]
    pub oco_order_id: Option<String>,
    #[serde(default)]
    pub exit_plan_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_profit_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_rs_vs_benchmark_30d: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnbracketedPosition {
    pub symbol: String,
    pub account_hash: String,
    pub quantity: f64,
    pub entry_price: f64,
    pub fill_order_id: String,
    pub detected_at: DateTime<Utc>,
    #[serde(default)]
    pub bracket_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingBuy {
    pub order_id: String,
    pub symbol: String,
    pub estimated_cost_usd: f64,
    pub submitted_at: DateTime<Utc>,
}

impl Default for SwingPosition {
    fn default() -> Self {
        Self {
            position_id: String::new(),
            symbol: String::new(),
            account_hash: String::new(),
            quantity: 0.0,
            entry_price: 0.0,
            opened_at: Utc::now(),
            stop_price: 0.0,
            profit_limit: 0.0,
            stop_risk_usd: 0.0,
            market_value_usd: 0.0,
            oco_order_id: None,
            exit_plan_version: 1,
            peak_profit_pct: None,
            entry_rs_vs_benchmark_30d: None,
        }
    }
}

impl TraderState {
    pub fn load(path: &Path, trader_id: &str) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self {
                trader_id: trader_id.to_string(),
                ..Default::default()
            });
        }
        let raw = fs::read_to_string(path)?;
        let mut state: TraderState = serde_json::from_str(&raw)?;
        if state.trader_id.is_empty() {
            state.trader_id = trader_id.to_string();
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(path, raw)?;
        Ok(())
    }

    pub fn equity_deployed_usd(&self) -> f64 {
        self.open_positions
            .values()
            .map(|p| p.market_value_usd.max(0.0))
            .sum()
    }

    pub fn pending_buy_usd(&self) -> f64 {
        self.pending_buys
            .iter()
            .map(|p| p.estimated_cost_usd.max(0.0))
            .sum()
    }

    pub fn open_stop_risk_usd(&self) -> f64 {
        self.open_positions
            .values()
            .map(|p| p.stop_risk_usd.max(0.0))
            .sum()
    }

    pub fn has_open_symbol(&self, symbol: &str) -> bool {
        let sym = symbol.trim().to_uppercase();
        self.open_positions
            .values()
            .any(|p| p.symbol.eq_ignore_ascii_case(&sym))
            || self
                .pending_buys
                .iter()
                .any(|p| p.symbol.eq_ignore_ascii_case(&sym))
    }

    pub fn reset_trades_day(&mut self, tz_name: &str) {
        let today = crate::market_session::trading_day(tz_name);
        if self.trades_day != Some(today) {
            self.trades_day = Some(today);
            self.trades_today = 0;
        }
    }

    pub fn reset_web_picks_day(&mut self, tz_name: &str) {
        let today = crate::market_session::trading_day(tz_name);
        if self.web_picks_day != Some(today) {
            self.web_picks_day = Some(today);
            self.web_picks_today = 0;
        }
    }

    pub fn entry_block_reason(&self, rules: &crate::rules::TraderRules) -> Option<String> {
        self.entry_block_reason_inner(rules, true)
    }

    /// Backtest/replay: skip live session clock gates (market hours, EOD cutoff).
    pub fn entry_block_reason_replay(&self, rules: &crate::rules::TraderRules) -> Option<String> {
        self.entry_block_reason_inner(rules, false)
    }

    fn entry_block_reason_inner(
        &self,
        rules: &crate::rules::TraderRules,
        check_session: bool,
    ) -> Option<String> {
        if let Some(reason) = &self.trading_halted_reason {
            return Some(reason.clone());
        }
        if let Some(reason) = crate::risk::drawdown_halt_reason(self, rules) {
            return Some(reason);
        }
        if check_session {
            if let Some(reason) = crate::closure::entry_block_reason(rules) {
                return Some(reason);
            }
        }
        if self.trades_today >= rules.risk.max_trades_per_day {
            return Some(format!(
                "max_trades_per_day reached ({}/{})",
                self.trades_today,
                rules.risk.max_trades_per_day
            ));
        }
        if self.trades_today >= rules.playbook.entry.max_new_entries_per_day {
            return Some(format!(
                "max_new_entries_per_day reached ({}/{})",
                self.trades_today,
                rules.playbook.entry.max_new_entries_per_day
            ));
        }
        if self.open_positions.len() >= rules.playbook.entry.max_positions as usize {
            return Some("max_positions reached".into());
        }
        None
    }

    pub fn summary(&self) -> Value {
        json!({
            "trader_id": self.trader_id,
            "open_positions": self.open_positions.len(),
            "equity_deployed_usd": self.equity_deployed_usd(),
            "pending_buys": self.pending_buys.len(),
            "unbracketed_positions": self.unbracketed_positions.len(),
            "trading_halted_reason": self.trading_halted_reason,
            "trades_today": self.trades_today,
            "tick_count": self.tick_count,
            "reconcile_mismatch_count": self.reconcile_mismatch_count,
        })
    }
}

pub fn position_id(symbol: &str, tz_name: &str) -> String {
    position_id_for_date(symbol, tz_name, crate::market_session::trading_day(tz_name))
}

pub fn position_id_for_date(symbol: &str, _tz_name: &str, day: NaiveDate) -> String {
    format!("{}|{}", symbol.trim().to_uppercase(), day)
}

pub fn load_state(rules_path: &Path, trader_id: &str) -> Result<TraderState> {
    let path = state_path(rules_path);
    TraderState::load(&path, trader_id)
}

pub fn load_backtest_state(rules_path: &Path, trader_id: &str) -> Result<TraderState> {
    let path = backtest_state_path(rules_path);
    TraderState::load(&path, trader_id)
}

pub fn save_state(rules_path: &Path, state: &TraderState) -> Result<()> {
    let path = state_path(rules_path);
    state.save(&path).context("save trader state")
}

pub fn save_backtest_state(rules_path: &Path, state: &TraderState) -> Result<()> {
    let path = backtest_state_path(rules_path);
    state.save(&path).context("save backtest state")
}
