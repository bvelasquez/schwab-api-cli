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
    pub pending_order_ids: Vec<String>,
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
        "pending_orders": state.pending_order_ids.len(),
        "recent_actions": state.last_actions.iter().rev().take(10).collect::<Vec<_>>(),
    })
}
