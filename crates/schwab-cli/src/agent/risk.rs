//! Options sleeve risk: drawdown halt from high-water mark.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::rules::RulesConfig;

use super::state::{AgentState, TrackedPosition};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawdownStatus {
    pub current_equity_usd: f64,
    pub peak_equity_usd: f64,
    pub drawdown_pct: f64,
    pub halt_threshold_pct: f64,
    pub halted: bool,
    pub sleeve_base_usd: f64,
    pub realized_pnl_usd: f64,
    pub unrealized_pnl_usd: f64,
}

/// Sleeve base for HWM: explicit `drawdown_sleeve_usd`, else sim budget, else portfolio risk cap.
pub fn sleeve_base_usd(state: &AgentState, rules: &RulesConfig) -> f64 {
    if let Some(v) = rules.risk.drawdown_sleeve_usd.filter(|v| *v > 0.0) {
        return v;
    }
    if let Some(sim) = &state.sim {
        if sim.starting_budget_usd > 0.0 {
            return sim.starting_budget_usd;
        }
    }
    rules.risk.max_portfolio_risk_usd.max(0.01)
}

pub fn realized_pnl_usd(state: &AgentState) -> f64 {
    if let Some(sim) = &state.sim {
        return sim.realized_pnl_usd;
    }
    state.cumulative_realized_pnl_usd
}

/// Unrealized $ from monitor snapshots (`profit_pct` × credit × 100 × contracts).
pub fn unrealized_pnl_from_monitors(monitored: &[Value]) -> f64 {
    monitored.iter().map(unrealized_from_snapshot).sum()
}

fn unrealized_from_snapshot(snap: &Value) -> f64 {
    let profit_pct = snap.get("profit_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let credit = snap
        .get("entry_credit")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let contracts = snap
        .get("contracts")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as f64;
    if credit <= 0.0 {
        return 0.0;
    }
    (profit_pct / 100.0) * credit * 100.0 * contracts
}

pub fn compute_sleeve_equity(state: &AgentState, rules: &RulesConfig, monitored: &[Value]) -> f64 {
    let base = sleeve_base_usd(state, rules);
    let realized = realized_pnl_usd(state);
    let unrealized = unrealized_pnl_from_monitors(monitored);
    base + realized + unrealized
}

/// Update HWM / drawdown halt. Clears only prior drawdown-sourced halt reasons.
pub fn update_drawdown(
    state: &mut AgentState,
    rules: &RulesConfig,
    monitored: &[Value],
) -> DrawdownStatus {
    let sleeve_base = sleeve_base_usd(state, rules);
    let realized = realized_pnl_usd(state);
    let unrealized = unrealized_pnl_from_monitors(monitored);
    let current = sleeve_base + realized + unrealized;

    if state.sleeve_peak_equity_usd <= 0.0 {
        state.sleeve_peak_equity_usd = current.max(sleeve_base);
    }
    if current > state.sleeve_peak_equity_usd {
        state.sleeve_peak_equity_usd = current;
    }

    let peak = state.sleeve_peak_equity_usd.max(0.01);
    let drawdown_pct = if current >= peak {
        0.0
    } else {
        ((peak - current) / peak) * 100.0
    };

    let halt_threshold = rules.risk.max_drawdown_halt_pct.unwrap_or(0.0);
    let halted = halt_threshold > 0.0 && drawdown_pct >= halt_threshold;

    if halted {
        state.trading_halted_reason = Some(format!(
            "drawdown halt: {:.1}% >= {:.1}%",
            drawdown_pct, halt_threshold
        ));
    } else if state
        .trading_halted_reason
        .as_deref()
        .is_some_and(|r| r.starts_with("drawdown halt"))
    {
        state.trading_halted_reason = None;
    }

    DrawdownStatus {
        current_equity_usd: current,
        peak_equity_usd: peak,
        drawdown_pct,
        halt_threshold_pct: halt_threshold,
        halted,
        sleeve_base_usd: sleeve_base,
        realized_pnl_usd: realized,
        unrealized_pnl_usd: unrealized,
    }
}

pub fn drawdown_to_json(status: &DrawdownStatus) -> Value {
    serde_json::to_value(status).unwrap_or(json!({}))
}

/// Credit-spread realized PnL in USD.
pub fn credit_spread_pnl_usd(entry_credit: f64, debit_to_close: f64, contracts: u32) -> f64 {
    (entry_credit - debit_to_close) * 100.0 * contracts.max(1) as f64
}

/// Record live (non-sim) realized PnL when a position closes.
pub fn record_live_realized_pnl(
    state: &mut AgentState,
    tracked: &TrackedPosition,
    debit_to_close: f64,
) {
    if state.sim.is_some() {
        return; // sim ledger owns realized PnL
    }
    let entry = tracked.entry_credit.unwrap_or(0.0);
    let pnl = credit_spread_pnl_usd(entry, debit_to_close, tracked.contracts);
    state.cumulative_realized_pnl_usd += pnl;
}

    #[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::rules::RulesConfig;
    use super::super::state::AgentState;

    #[test]
    fn drawdown_halt_triggers_and_clears() {
        let rules: RulesConfig = serde_json::from_value(json!({
            "version": 1,
            "agent_id": "t",
            "accounts": [{"hash": "H", "enabled": true}],
            "watchlist": ["SPY"],
            "risk": {
                "max_portfolio_risk_usd": 500,
                "max_risk_per_trade_usd": 500,
                "max_trades_per_day": 1,
                "allowed_underlyings": ["SPY"],
                "blocked_events": [],
                "max_drawdown_halt_pct": 10.0,
                "drawdown_sleeve_usd": 1000.0
            }
        }))
        .unwrap();

        let mut state = AgentState {
            agent_id: "t".into(),
            cumulative_realized_pnl_usd: -150.0,
            ..Default::default()
        };
        let status = update_drawdown(&mut state, &rules, &[]);
        assert!(status.halted, "dd={}", status.drawdown_pct);
        assert!(state
            .trading_halted_reason
            .as_deref()
            .unwrap()
            .starts_with("drawdown halt"));

        state.cumulative_realized_pnl_usd = 50.0;
        let recovered = update_drawdown(&mut state, &rules, &[]);
        assert!(!recovered.halted);
        assert!(state.trading_halted_reason.is_none());
    }

    #[test]
    fn unrealized_from_monitor_snapshot() {
        let snap = json!({
            "profit_pct": 50.0,
            "entry_credit": 0.40,
            "contracts": 2
        });
        // 0.5 * 0.40 * 100 * 2 = 40
        assert!((unrealized_from_snapshot(&snap) - 40.0).abs() < 1e-9);
    }
}
