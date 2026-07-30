use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::options::types::StrategyKind;
use crate::rules::{EntryPolicyConfig, RulesConfig};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentState {
    pub agent_id: String,
    pub last_tick: Option<DateTime<Utc>>,
    pub trades_today: u32,
    pub trades_day: Option<NaiveDate>,
    /// Successful defensive rolls today (reset with `trades_day`).
    #[serde(default)]
    pub rolls_today: u32,
    pub open_positions: HashMap<String, TrackedPosition>,
    pub last_actions: Vec<AgentAction>,
    #[serde(default)]
    pub pending_order_ids: Vec<String>,
    #[serde(default)]
    pub pending_orders: Vec<PendingOrder>,
    #[serde(default)]
    pub tick_count: u64,
    #[serde(default)]
    pub last_llm_review_tick: Option<u64>,
    #[serde(default)]
    pub llm_review_count: u64,
    #[serde(default)]
    pub last_llm_summary: Option<Value>,
    #[serde(default)]
    pub last_session: Option<String>,
    #[serde(default)]
    pub regular_tick_count: u64,
    #[serde(default)]
    pub last_overnight_digest_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub open_playbook: Option<Value>,
    /// Last EQO regular-session open flag from agent tick (Schwab hours).
    #[serde(default)]
    pub last_market_open: Option<bool>,
    #[serde(default)]
    pub last_auth_reminder_level: Option<String>,
    #[serde(default)]
    pub last_auth_reminder_at: Option<DateTime<Utc>>,
    /// Last LLM review pushed to Telegram (for digest dedup).
    #[serde(default)]
    pub last_telegram_llm_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_telegram_llm_digest_key: Option<String>,
    /// Paper-trading ledger when running with --simulate (separate state file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim: Option<crate::agent::sim::SimLedger>,
    /// Set after a thesis-driven exit — optional redeploy cooldown on that underlying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redeploy_signal: Option<RedeploySignal>,
    /// Recent stop-loss exits for re-entry cooldown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_stop_loss_exits: Vec<StopLossExitRecord>,
    /// Last live entry attempt per candidate position_id (limits retry spam).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub last_entry_attempts: HashMap<String, DateTime<Utc>>,
    /// Cached LLM `proceed` for current candidate fingerprints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_proceed_cache: Option<EntryProceedCache>,
    /// Last options regime snapshot (strategy selection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_regime: Option<Value>,
    /// High-water mark for options sleeve equity (drawdown halt).
    #[serde(default)]
    pub sleeve_peak_equity_usd: f64,
    /// Cumulative realized PnL for live (non-sim) closes. Sim uses `sim.realized_pnl_usd`.
    #[serde(default)]
    pub cumulative_realized_pnl_usd: f64,
    /// When set, new entries are paused (exits continue). Cleared when condition recovers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trading_halted_reason: Option<String>,
    /// Rolling LLM entry-decision scorecard (updated live; journal is authoritative).
    #[serde(default)]
    pub llm_scorecard: crate::agent::scorecard::LlmScorecardSummary,
    /// Most recent selection-phase decision (for linking to fills).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_llm_entry_decision: Option<crate::agent::scorecard::LlmEntryDecisionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopLossExitRecord {
    pub at: DateTime<Utc>,
    pub underlying: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryProceedCache {
    pub at: DateTime<Utc>,
    pub candidate_fingerprints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedeploySignal {
    pub at: DateTime<Utc>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlying: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedPosition {
    pub position_id: String,
    pub account_hash: String,
    pub underlying: String,
    pub expiry: String,
    pub strategy: String,
    pub opened_at: DateTime<Utc>,
    pub entry_credit: Option<f64>,
    pub max_loss_usd: f64,
    /// Spread quantity (each leg at Schwab should match this count).
    #[serde(default = "default_one")]
    pub contracts: u32,
    /// Strategy params for sim marks / vertical reconstruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_params: Option<Value>,
    /// Peak unrealized profit % observed while open (thesis giveback exits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_profit_pct: Option<f64>,
    /// POP % at entry (from chain analytics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_pop_pct: Option<f64>,
    /// |short_delta| at entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_short_delta: Option<f64>,
    /// Broker order id for the resting GTC profit-target close order, if placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protective_order_id: Option<String>,
    /// Last known status of `protective_order_id` (e.g. "WORKING", "FILLED", "CANCELED").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protective_order_status: Option<String>,
    /// Consecutive failed attempts to place the protective order (drives reconcile retry/backoff).
    #[serde(default)]
    pub protective_order_attempts: u32,
    /// Successful defensive rolls applied to this lineage (carried onto replacement).
    #[serde(default)]
    pub rolls_used: u32,
    /// When the last defensive roll opened this position (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_roll_at: Option<DateTime<Utc>>,
    /// Linked `llm_entry_decision` id when entry followed a selection proceed / fail-open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_decision_id: Option<String>,
}

pub fn update_peak_profit_pct(position: &mut TrackedPosition, profit_pct: f64) {
    let peak = position.peak_profit_pct.unwrap_or(profit_pct);
    position.peak_profit_pct = Some(peak.max(profit_pct));
}

pub fn is_thesis_exit_reason(reason: &str) -> bool {
    reason.starts_with("thesis_")
}

pub fn is_stop_loss_exit_reason(reason: &str) -> bool {
    reason == "stop_loss"
}

pub fn record_stop_loss_exit(state: &mut AgentState, underlying: &str) {
    state.recent_stop_loss_exits.push(StopLossExitRecord {
        at: Utc::now(),
        underlying: underlying.to_uppercase(),
    });
    prune_stop_loss_records(&mut state.recent_stop_loss_exits, 45);
}

fn prune_stop_loss_records(records: &mut Vec<StopLossExitRecord>, keep_days: i64) {
    let cutoff = Utc::now() - chrono::Duration::days(keep_days);
    records.retain(|r| r.at >= cutoff);
}

pub fn stop_loss_re_entry_blocked(
    rules: &RulesConfig,
    state: &AgentState,
    underlying: &str,
) -> Option<i64> {
    let cfg = &rules.risk.re_entry_after_stop_loss;
    if !cfg.enabled || cfg.cooldown_days == 0 {
        return None;
    }
    let sym = underlying.to_uppercase();
    let latest = state
        .recent_stop_loss_exits
        .iter()
        .filter(|r| r.underlying.eq_ignore_ascii_case(&sym))
        .map(|r| r.at)
        .max()?;
    let elapsed_days = Utc::now().signed_duration_since(latest).num_days();
    let remaining = cfg.cooldown_days as i64 - elapsed_days;
    if remaining > 0 {
        Some(remaining)
    } else {
        None
    }
}

pub fn entry_attempt_cooldown_active(
    policy: &EntryPolicyConfig,
    state: &AgentState,
    position_id: &str,
) -> Option<i64> {
    if policy.entry_attempt_cooldown_minutes == 0 {
        return None;
    }
    let at = state.last_entry_attempts.get(position_id)?;
    let elapsed = Utc::now().signed_duration_since(*at).num_minutes();
    let limit = policy.entry_attempt_cooldown_minutes as i64;
    if elapsed < limit {
        Some(limit - elapsed)
    } else {
        None
    }
}

pub fn record_entry_attempt(state: &mut AgentState, position_id: &str) {
    state
        .last_entry_attempts
        .insert(position_id.to_string(), Utc::now());
}

pub fn candidate_fingerprint(signal: &Value) -> Option<String> {
    signal
        .get("position_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub fn entry_proceed_cache_valid(
    policy: &EntryPolicyConfig,
    cache: &EntryProceedCache,
    fingerprints: &[String],
) -> bool {
    if fingerprints.is_empty() {
        return false;
    }
    if policy.proceed_cache_minutes == 0 {
        return false;
    }
    let age = Utc::now().signed_duration_since(cache.at).num_minutes();
    if age > policy.proceed_cache_minutes as i64 {
        return false;
    }
    cache.candidate_fingerprints == fingerprints
}

impl Default for TrackedPosition {
    fn default() -> Self {
        Self {
            position_id: String::new(),
            account_hash: String::new(),
            underlying: String::new(),
            expiry: String::new(),
            strategy: String::new(),
            opened_at: Utc::now(),
            entry_credit: None,
            max_loss_usd: 0.0,
            contracts: 1,
            entry_params: None,
            peak_profit_pct: None,
            entry_pop_pct: None,
            entry_short_delta: None,
            protective_order_id: None,
            protective_order_status: None,
            protective_order_attempts: 0,
            rolls_used: 0,
            last_roll_at: None,
            llm_decision_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingOrderAction {
    Entry,
    Exit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOrder {
    pub order_id: String,
    pub account_hash: String,
    pub action: PendingOrderAction,
    pub position_id: String,
    pub reserved_risk_usd: f64,
    pub submitted_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

fn default_one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    pub at: DateTime<Utc>,
    pub action: String,
    pub detail: Value,
}

pub fn load_state(path: &Path) -> Result<AgentState> {
    if !path.exists() {
        return Ok(AgentState::default());
    }
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

pub fn save_state(path: &Path, state: &AgentState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(state)?;
    fs::write(path, content)?;
    Ok(())
}

impl AgentState {
    pub fn reset_daily_if_needed(&mut self, today: NaiveDate) {
        if self.trades_day != Some(today) {
            self.trades_today = 0;
            self.rolls_today = 0;
            self.trades_day = Some(today);
        }
    }

    pub fn record_action(&mut self, action: &str, detail: Value) {
        self.last_actions.push(AgentAction {
            at: Utc::now(),
            action: action.to_string(),
            detail,
        });
        if self.last_actions.len() > 100 {
            let drain = self.last_actions.len() - 100;
            self.last_actions.drain(0..drain);
        }
    }

    pub fn count_open_for_strategy(&self, account_hash: &str, strategy: StrategyKind) -> u32 {
        self.open_positions
            .values()
            .filter(|p| p.account_hash == account_hash && p.strategy == strategy.as_str())
            .count() as u32
    }

    pub fn count_open_for_underlying(&self, account_hash: &str, underlying: &str) -> u32 {
        self.open_positions
            .values()
            .filter(|p| {
                p.account_hash == account_hash && p.underlying.eq_ignore_ascii_case(underlying)
            })
            .count() as u32
    }

    /// Open positions whose underlying is in `symbols` (case-insensitive).
    pub fn count_open_in_symbol_set(&self, account_hash: &str, symbols: &[String]) -> u32 {
        self.open_positions
            .values()
            .filter(|p| {
                p.account_hash == account_hash
                    && symbols
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&p.underlying))
            })
            .count() as u32
    }

    pub fn pending_entry_count_in_symbol_set(&self, account_hash: &str, symbols: &[String]) -> u32 {
        self.pending_orders
            .iter()
            .filter(|p| {
                p.action == PendingOrderAction::Entry
                    && p.account_hash == account_hash
                    && position_id_underlying(&p.position_id).is_some_and(|u| {
                        symbols.iter().any(|s| s.eq_ignore_ascii_case(u))
                    })
            })
            .count() as u32
    }

    pub fn pending_entry_count_for_underlying(
        &self,
        account_hash: &str,
        underlying: &str,
    ) -> u32 {
        self.pending_orders
            .iter()
            .filter(|p| {
                p.action == PendingOrderAction::Entry
                    && p.account_hash == account_hash
                    && position_id_underlying(&p.position_id)
                        .is_some_and(|u| u.eq_ignore_ascii_case(underlying))
            })
            .count() as u32
    }

    pub fn pending_entry_count(&self) -> u32 {
        self.pending_orders
            .iter()
            .filter(|p| p.action == PendingOrderAction::Entry)
            .count() as u32
    }

    pub fn pending_count(&self) -> usize {
        self.pending_orders.len().max(self.pending_order_ids.len())
    }

    pub fn trades_capacity_used(&self) -> u32 {
        self.trades_today.saturating_add(self.pending_entry_count())
    }

    pub fn open_risk_usd(&self) -> f64 {
        self.open_positions
            .values()
            .map(|p| p.max_loss_usd.max(0.0))
            .sum()
    }

    pub fn pending_entry_risk_usd(&self) -> f64 {
        self.pending_orders
            .iter()
            .filter(|p| p.action == PendingOrderAction::Entry)
            .map(|p| p.reserved_risk_usd.max(0.0))
            .sum()
    }

    pub fn reserved_risk_usd(&self) -> f64 {
        self.open_risk_usd() + self.pending_entry_risk_usd()
    }

    pub fn has_pending_position(&self, position_id: &str) -> bool {
        self.pending_orders
            .iter()
            .any(|p| p.position_id == position_id)
    }

    pub fn add_pending_order(&mut self, pending: PendingOrder) {
        if !self
            .pending_order_ids
            .iter()
            .any(|id| id == &pending.order_id)
        {
            self.pending_order_ids.push(pending.order_id.clone());
        }
        if let Some(existing) = self
            .pending_orders
            .iter_mut()
            .find(|p| p.order_id == pending.order_id)
        {
            *existing = pending;
        } else {
            self.pending_orders.push(pending);
        }
    }

    pub fn remove_pending_order(&mut self, order_id: &str) -> Option<PendingOrder> {
        self.pending_order_ids.retain(|id| id != order_id);
        let idx = self
            .pending_orders
            .iter()
            .position(|p| p.order_id == order_id)?;
        Some(self.pending_orders.remove(idx))
    }

    pub fn clear_legacy_pending_ids(&mut self) {
        let structured: std::collections::HashSet<&str> = self
            .pending_orders
            .iter()
            .map(|p| p.order_id.as_str())
            .collect();
        self.pending_order_ids
            .retain(|id| structured.contains(id.as_str()));
    }

    pub fn total_contracts(&self) -> u32 {
        self.open_positions
            .values()
            .map(|p| p.contracts.max(1))
            .sum()
    }
}

pub fn state_summary(state: &AgentState) -> Value {
    json!({
        "agent_id": state.agent_id,
        "last_tick": state.last_tick,
        "trades_today": state.trades_today,
        "open_positions": state.open_positions.len(),
        "tick_count": state.tick_count,
        "last_llm_review_tick": state.last_llm_review_tick,
        "last_llm_summary": state.last_llm_summary,
        "last_session": state.last_session,
        "regular_tick_count": state.regular_tick_count,
        "last_overnight_digest_at": state.last_overnight_digest_at,
        "open_playbook": state.open_playbook,
        "last_market_open": state.last_market_open,
        "last_auth_reminder_at": state.last_auth_reminder_at,
        "pending_orders": state.pending_count(),
        "reserved_risk_usd": state.reserved_risk_usd(),
        "pending_orders_detail": state.pending_orders,
        "recent_actions": state.last_actions.iter().rev().take(10).collect::<Vec<_>>(),
    })
}

fn position_id_underlying(position_id: &str) -> Option<&str> {
    position_id.split('|').nth(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{ReEntryAfterStopLoss, RiskConfig, RulesConfig};

    #[test]
    fn reserved_risk_includes_pending_entries_only() {
        let mut state = AgentState::default();
        state.open_positions.insert(
            "pos".into(),
            TrackedPosition {
                position_id: "pos".into(),
                account_hash: "acct".into(),
                underlying: "IWM".into(),
                expiry: "2026-07-31".into(),
                strategy: "vertical".into(),
                opened_at: Utc::now(),
                entry_credit: Some(0.25),
                max_loss_usd: 175.0,
                contracts: 1,
                entry_params: None,
                ..Default::default()
            },
        );
        state.add_pending_order(PendingOrder {
            order_id: "entry-1".into(),
            account_hash: "acct".into(),
            action: PendingOrderAction::Entry,
            position_id: "pending-entry".into(),
            reserved_risk_usd: 170.0,
            submitted_at: Utc::now(),
            last_status: Some("WORKING".into()),
            detail: None,
        });
        state.add_pending_order(PendingOrder {
            order_id: "exit-1".into(),
            account_hash: "acct".into(),
            action: PendingOrderAction::Exit,
            position_id: "pos".into(),
            reserved_risk_usd: 0.0,
            submitted_at: Utc::now(),
            last_status: Some("WORKING".into()),
            detail: None,
        });

        assert_eq!(state.pending_entry_count(), 1);
        assert!((state.reserved_risk_usd() - 345.0).abs() < 0.01);
    }

    #[test]
    fn stop_loss_re_entry_blocked_within_cooldown() {
        let mut rules = RulesConfig {
            version: 1,
            agent_id: "t".into(),
            accounts: vec![],
            schedule: Default::default(),
            strategies: Default::default(),
            watchlist: vec![],
            entry_policy: Default::default(),
            entry_rules: Default::default(),
            exit_rules: Default::default(),
            risk: RiskConfig {
                re_entry_after_stop_loss: ReEntryAfterStopLoss {
                    enabled: true,
                    cooldown_days: 5,
                },
                ..Default::default()
            },
            regime: Default::default(),
            execution: Default::default(),
            llm: Default::default(),
            notify: Default::default(),
            simulation: None,
        };
        let mut state = AgentState::default();
        record_stop_loss_exit(&mut state, "IWM");
        assert!(stop_loss_re_entry_blocked(&rules, &state, "IWM").is_some());
        rules.risk.re_entry_after_stop_loss.enabled = false;
        assert!(stop_loss_re_entry_blocked(&rules, &state, "IWM").is_none());
    }
}
