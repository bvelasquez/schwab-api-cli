use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::options::types::StrategyKind;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentState {
    pub agent_id: String,
    pub last_tick: Option<DateTime<Utc>>,
    pub trades_today: u32,
    pub trades_day: Option<NaiveDate>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
