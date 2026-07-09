//! Paper options simulation: virtual fills from live chain marks (no broker orders).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::options::validate::estimate_order_margin;
use crate::options::StrategyKind;
use crate::rules::RulesConfig;

use super::exits::SpreadMark;
use super::journal;
use super::state::{AgentState, TrackedPosition};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SimLedger {
    pub starting_budget_usd: f64,
    #[serde(default)]
    pub realized_pnl_usd: f64,
    #[serde(default)]
    pub closed_trades: Vec<ClosedSimTrade>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedSimTrade {
    pub trade_id: String,
    pub position_id: String,
    pub underlying: String,
    pub expiry: String,
    pub strategy: String,
    pub contracts: u32,
    pub entry_credit: f64,
    pub exit_debit: f64,
    pub opened_at: DateTime<Utc>,
    pub closed_at: DateTime<Utc>,
    pub pnl_usd: f64,
    pub pnl_pct: f64,
    pub exit_reason: String,
    pub hold_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimStats {
    pub starting_budget_usd: f64,
    pub realized_pnl_usd: f64,
    pub open_risk_usd: f64,
    pub open_positions: usize,
    pub closed_trades: usize,
    pub roi_pct: f64,
    pub win_rate_pct: f64,
    pub avg_win_usd: f64,
    pub avg_loss_usd: f64,
    pub max_drawdown_pct: f64,
    pub expectancy_usd: f64,
    #[serde(default)]
    pub exit_reason_counts: HashMap<String, u32>,
}

pub fn ensure_ledger<'a>(state: &'a mut AgentState, rules: &RulesConfig) -> &'a mut SimLedger {
    if state.sim.is_none() {
        let start = rules
            .simulation
            .as_ref()
            .map(|s| s.starting_budget_usd)
            .unwrap_or(rules.risk.max_portfolio_risk_usd);
        state.sim = Some(SimLedger {
            starting_budget_usd: start,
            realized_pnl_usd: 0.0,
            closed_trades: vec![],
        });
    }
    state.sim.as_mut().expect("sim ledger")
}

pub fn compute_stats(state: &AgentState, rules: &RulesConfig) -> SimStats {
    let ledger = state.sim.as_ref();
    let starting = ledger
        .map(|l| l.starting_budget_usd)
        .unwrap_or_else(|| {
            rules
                .simulation
                .as_ref()
                .map(|s| s.starting_budget_usd)
                .unwrap_or(rules.risk.max_portfolio_risk_usd)
        });
    let realized = ledger.map(|l| l.realized_pnl_usd).unwrap_or(0.0);
    let closed = ledger.map(|l| l.closed_trades.as_slice()).unwrap_or(&[]);
    let wins: Vec<f64> = closed.iter().filter(|t| t.pnl_usd > 0.0).map(|t| t.pnl_usd).collect();
    let losses: Vec<f64> = closed
        .iter()
        .filter(|t| t.pnl_usd < 0.0)
        .map(|t| t.pnl_usd)
        .collect();
    let win_rate = if closed.is_empty() {
        0.0
    } else {
        (wins.len() as f64 / closed.len() as f64) * 100.0
    };
    let avg_win = if wins.is_empty() {
        0.0
    } else {
        wins.iter().sum::<f64>() / wins.len() as f64
    };
    let avg_loss = if losses.is_empty() {
        0.0
    } else {
        losses.iter().sum::<f64>() / losses.len() as f64
    };
    let expectancy = if closed.is_empty() {
        0.0
    } else {
        closed.iter().map(|t| t.pnl_usd).sum::<f64>() / closed.len() as f64
    };
    let mut exit_reason_counts = HashMap::new();
    for t in closed {
        *exit_reason_counts.entry(t.exit_reason.clone()).or_insert(0) += 1;
    }
    let mut peak = starting;
    let mut equity = starting;
    let mut max_dd = 0.0f64;
    for t in closed {
        equity += t.pnl_usd;
        peak = peak.max(equity);
        if peak > 0.0 {
            let dd = ((peak - equity) / peak) * 100.0;
            max_dd = max_dd.max(dd);
        }
    }
    SimStats {
        starting_budget_usd: starting,
        realized_pnl_usd: realized,
        open_risk_usd: state.open_risk_usd(),
        open_positions: state.open_positions.len(),
        closed_trades: closed.len(),
        roi_pct: if starting > 0.0 {
            (realized / starting) * 100.0
        } else {
            0.0
        },
        win_rate_pct: win_rate,
        avg_win_usd: avg_win,
        avg_loss_usd: avg_loss,
        max_drawdown_pct: max_dd,
        expectancy_usd: expectancy,
        exit_reason_counts,
    }
}

pub fn reset_sim(state: &mut AgentState, rules: &RulesConfig) {
    let start = rules
        .simulation
        .as_ref()
        .map(|s| s.starting_budget_usd)
        .unwrap_or(rules.risk.max_portfolio_risk_usd);
    state.open_positions.clear();
    state.pending_orders.clear();
    state.pending_order_ids.clear();
    state.trades_today = 0;
    state.sim = Some(SimLedger {
        starting_budget_usd: start,
        realized_pnl_usd: 0.0,
        closed_trades: vec![],
    });
}

pub fn record_sim_entry(
    rules_path: &Path,
    state: &mut AgentState,
    rules: &RulesConfig,
    account_hash: &str,
    kind: StrategyKind,
    signal: &Value,
) -> Result<Value> {
    if state.trades_capacity_used() >= rules.risk.max_trades_per_day {
        return Ok(json!({
            "fill_status": "SKIPPED",
            "reason": "max_trades_per_day reached",
            "mode": "simulate",
        }));
    }

    let params = signal
        .get("params")
        .cloned()
        .context("signal missing params")?;
    let margin = estimate_order_margin(&json!({}), kind, &params)?;
    if margin > rules.risk.max_risk_per_trade_usd {
        return Ok(json!({
            "fill_status": "SKIPPED",
            "reason": "max_risk_per_trade_usd exceeded",
            "required_margin_usd": margin,
            "mode": "simulate",
        }));
    }
    let reserved = state.reserved_risk_usd();
    if reserved + margin > rules.risk.max_portfolio_risk_usd {
        return Ok(json!({
            "fill_status": "SKIPPED",
            "reason": "max_portfolio_risk_usd exceeded",
            "mode": "simulate",
        }));
    }

    let position_id = signal
        .get("position_id")
        .and_then(|v| v.as_str())
        .context("signal missing position_id")?
        .to_string();
    if state.open_positions.contains_key(&position_id) {
        return Ok(json!({
            "fill_status": "SKIPPED",
            "reason": "position already open",
            "position_id": position_id,
            "mode": "simulate",
        }));
    }

    let credit = signal
        .get("estimated_credit")
        .and_then(|v| v.as_f64())
        .or_else(|| params.get("limit_credit").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    let underlying = params
        .get("underlying")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let expiry = params
        .get("expiry")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let contracts = params
        .get("contracts")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
        .round()
        .max(1.0) as u32;

    ensure_ledger(state, rules);
    state.trades_today += 1;
    state.open_positions.insert(
        position_id.clone(),
        TrackedPosition {
            position_id: position_id.clone(),
            account_hash: account_hash.to_string(),
            underlying: underlying.clone(),
            expiry: expiry.clone(),
            strategy: kind.as_str().to_string(),
            opened_at: Utc::now(),
            entry_credit: Some(credit),
            max_loss_usd: margin,
            contracts,
            entry_params: Some(params.clone()),
            peak_profit_pct: None,
            entry_pop_pct: signal
                .pointer("/market_context/spread_pop_pct")
                .and_then(|v| v.as_f64()),
            entry_short_delta: signal
                .pointer("/market_context/short_delta")
                .and_then(|v| v.as_f64())
                .map(f64::abs),
        },
    );

    let detail = json!({
        "fill_status": "FILLED",
        "mode": "simulate",
        "position_id": position_id,
        "entry_credit": credit,
        "contracts": contracts,
        "max_loss_usd": margin,
        "signal": signal,
    });
    state.record_action("sim_entry", detail.clone());
    journal::append_event(rules_path, true, "sim_entry_filled", detail.clone())?;
    Ok(detail)
}

pub fn record_sim_exit(
    rules_path: &Path,
    state: &mut AgentState,
    rules: &RulesConfig,
    position_id: &str,
    exit_reason: &str,
    mark: &SpreadMark,
    signal: &Value,
) -> Result<Value> {
    let tracked = state
        .open_positions
        .remove(position_id)
        .with_context(|| format!("sim position {position_id} not found"))?;
    let entry_credit = tracked.entry_credit.unwrap_or(mark.entry_credit);
    let contracts = tracked.contracts.max(1) as f64;
    let pnl_per_spread = (entry_credit - mark.debit_to_close) * 100.0;
    let pnl_usd = pnl_per_spread * contracts;
    let pnl_pct = if entry_credit > f64::EPSILON {
        ((entry_credit - mark.debit_to_close) / entry_credit) * 100.0
    } else {
        0.0
    };
    let hold_days = (Utc::now() - tracked.opened_at).num_days().max(0) as u32;
    let trade_id = format!("sim-{}-{}", position_id, Utc::now().timestamp());

    let ledger = ensure_ledger(state, rules);
    ledger.realized_pnl_usd += pnl_usd;
    ledger.closed_trades.push(ClosedSimTrade {
        trade_id: trade_id.clone(),
        position_id: position_id.to_string(),
        underlying: tracked.underlying.clone(),
        expiry: tracked.expiry.clone(),
        strategy: tracked.strategy.clone(),
        contracts: tracked.contracts,
        entry_credit,
        exit_debit: mark.debit_to_close,
        opened_at: tracked.opened_at,
        closed_at: Utc::now(),
        pnl_usd,
        pnl_pct,
        exit_reason: exit_reason.to_string(),
        hold_days,
    });

    let detail = json!({
        "fill_status": "FILLED",
        "mode": "simulate",
        "position_id": position_id,
        "exit_reason": exit_reason,
        "pnl_usd": pnl_usd,
        "pnl_pct": pnl_pct,
        "mark": mark,
        "signal": signal,
    });
    state.record_action("sim_exit", detail.clone());
    journal::append_event(rules_path, true, "sim_exit_filled", detail.clone())?;
    Ok(detail)
}

pub fn analysis_report(state: &AgentState, rules: &RulesConfig) -> Value {
    let stats = compute_stats(state, rules);
    let per_underlying: HashMap<String, f64> = state
        .sim
        .as_ref()
        .map(|l| {
            l.closed_trades
                .iter()
                .fold(HashMap::new(), |mut acc, t| {
                    *acc.entry(t.underlying.clone()).or_insert(0.0) += t.pnl_usd;
                    acc
                })
        })
        .unwrap_or_default();
    json!({
        "mode": "simulate",
        "stats": stats,
        "per_underlying_pnl": per_underlying,
        "open_positions": state.open_positions.values().collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn spread_pnl_from_credit_and_debit() {
        let pnl_per: f64 = (1.0 - 0.4) * 100.0;
        assert!((pnl_per - 60.0).abs() < 0.01);
    }
}
