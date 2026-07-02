//! Close-all for agent-managed positions only (never whole-account flatten).

use std::path::Path;

use anyhow::{Context, Result};
use schwab_api::TraderApi;
use serde_json::{json, Value};

use crate::agent::exits::{load_live_position_groups, option_group_from_tracked};
use crate::agent::paths::load_agent_state;
use crate::agent::state::AgentState;
use crate::config::RuntimeConfig;
use crate::options::{build_close_order_for_group, OptionPositionGroup};
use crate::rules::RulesConfig;
use crate::safety::{execute_trading_order, require_trading_approval};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedOptionClose {
    pub position_id: String,
    pub account_hash: String,
    pub underlying: String,
    pub expiry: String,
    pub strategy: String,
    pub contracts: u32,
    pub group: OptionPositionGroup,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentFlattenPlan {
    pub agent_id: String,
    pub managed_closes: Vec<ManagedOptionClose>,
    pub skipped: Vec<Value>,
}

impl AgentFlattenPlan {
    pub fn is_empty(&self) -> bool {
        self.managed_closes.is_empty()
    }

    pub fn summary(&self) -> String {
        if self.managed_closes.is_empty() {
            return "no agent-managed spreads".into();
        }
        self.managed_closes
            .iter()
            .map(|c| format!("{} {} {}", c.underlying, c.expiry, c.strategy))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub async fn build_agent_options_flatten_plan(
    api: &TraderApi,
    rules: &RulesConfig,
    state: &AgentState,
) -> Result<AgentFlattenPlan> {
    let live = load_live_position_groups(api, rules).await?;
    let mut managed_closes = Vec::new();
    let mut skipped = Vec::new();

    let mut tracked: Vec<_> = state.open_positions.values().cloned().collect();
    tracked.sort_by(|a, b| a.position_id.cmp(&b.position_id));

    for pos in tracked {
        let group = live
            .get(&pos.position_id)
            .cloned()
            .or_else(|| option_group_from_tracked(&pos));
        let Some(group) = group else {
            skipped.push(json!({
                "position_id": pos.position_id,
                "underlying": pos.underlying,
                "expiry": pos.expiry,
                "reason": "no matching broker spread (already closed or not on Schwab)",
            }));
            continue;
        };
        managed_closes.push(ManagedOptionClose {
            position_id: pos.position_id.clone(),
            account_hash: pos.account_hash.clone(),
            underlying: pos.underlying.clone(),
            expiry: pos.expiry.clone(),
            strategy: pos.strategy.clone(),
            contracts: pos.contracts,
            group,
        });
    }

    Ok(AgentFlattenPlan {
        agent_id: rules.agent_id.clone(),
        managed_closes,
        skipped,
    })
}

pub async fn execute_agent_options_flatten(
    runtime: &RuntimeConfig,
    api: &TraderApi,
    rules_path: &Path,
    rules: &RulesConfig,
    plan: &AgentFlattenPlan,
) -> Result<Value> {
    if plan.is_empty() {
        return Ok(json!({
            "flattened": false,
            "agent_id": plan.agent_id,
            "message": "no agent-managed spreads to close",
            "skipped": plan.skipped,
            "closes": [],
        }));
    }

    require_trading_approval(
        runtime,
        "agent close-all",
        &format!(
            "Close {} agent-managed spread(s) for `{}` (NOT whole account): {}",
            plan.managed_closes.len(),
            plan.agent_id,
            plan.summary()
        ),
    )?;

    if runtime.dry_run {
        return Ok(json!({
            "dry_run": true,
            "agent_id": plan.agent_id,
            "rules": rules_path,
            "managed_closes": plan.managed_closes,
            "skipped": plan.skipped,
        }));
    }

    let mut closes = Vec::new();
    for target in &plan.managed_closes {
        let order = build_close_order_for_group(&target.group)?;
        let result = execute_trading_order(runtime, api, &target.account_hash, &order)
            .await
            .with_context(|| format!("close {}", target.position_id))?;
        closes.push(json!({
            "position_id": target.position_id,
            "underlying": target.underlying,
            "expiry": target.expiry,
            "strategy": target.strategy,
            "contracts": target.contracts,
            "result": result,
        }));
    }

    // Drop closed positions from agent state when not simulating.
    if !runtime.simulate {
        let mut state = load_agent_state(rules_path, &rules.agent_id);
        for target in &plan.managed_closes {
            state.open_positions.remove(&target.position_id);
        }
        let state_path = crate::agent::paths::active_state_path(rules_path, runtime.simulate);
        crate::agent::save_state(&state_path, &state)?;
    }

    Ok(json!({
        "flattened": true,
        "agent_id": plan.agent_id,
        "closes": closes,
        "skipped": plan.skipped,
    }))
}
