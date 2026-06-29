//! Sleeve risk controls: drawdown halt, monitoring metrics.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::state::TraderState;
use crate::rules::TraderRules;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawdownStatus {
    pub current_equity_usd: f64,
    pub peak_equity_usd: f64,
    pub drawdown_pct: f64,
    pub halt_threshold_pct: f64,
    pub halted: bool,
}

/// Cash + open position market value for this trader's sleeve.
pub fn compute_sleeve_equity(state: &TraderState) -> f64 {
    if let Some(sim) = &state.sim {
        let positions_value: f64 = state
            .open_positions
            .values()
            .map(|p| p.market_value_usd.max(0.0))
            .sum();
        return sim.cash_usd + positions_value;
    }
    state.equity_deployed_usd()
}

pub fn update_drawdown(state: &mut TraderState, rules: &TraderRules) -> DrawdownStatus {
    let current = if state.sim.is_some() {
        compute_sleeve_equity(state)
    } else {
        // Live: deployed capital is the best proxy until reconcile marks positions.
        state.equity_deployed_usd()
    };

    if state.sleeve_peak_equity_usd <= 0.0 {
        state.sleeve_peak_equity_usd = current.max(rules.capital.fixed_sleeve_cap_usd);
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

    let halt_threshold = rules.risk.max_drawdown_halt_pct;
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
    }
}

pub fn drawdown_halt_reason(state: &TraderState, rules: &TraderRules) -> Option<String> {
    if rules.risk.max_drawdown_halt_pct <= 0.0 {
        return None;
    }
    let peak = state.sleeve_peak_equity_usd.max(0.01);
    let current = compute_sleeve_equity(state);
    if current >= peak {
        return None;
    }
    let drawdown_pct = ((peak - current) / peak) * 100.0;
    if drawdown_pct >= rules.risk.max_drawdown_halt_pct {
        Some(format!(
            "drawdown halt: {:.1}% drawdown (limit {:.1}%)",
            drawdown_pct, rules.risk.max_drawdown_halt_pct
        ))
    } else {
        None
    }
}

/// Sum equity deployed by other trader profiles on the same account (sibling state files).
pub fn sibling_sleeve_deployed(rules_path: &Path, rules: &TraderRules) -> f64 {
    let account_hash = rules
        .primary_account()
        .map(|a| a.hash.as_str())
        .unwrap_or_default();
    if account_hash.is_empty() {
        return 0.0;
    }

    let dir = match rules_path.parent() {
        Some(d) => d,
        None => return 0.0,
    };
    let my_state = crate::agent::paths::state_path(rules_path);
    let mut total = 0.0;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0.0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == my_state {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("trader-state-") || !name.ends_with(".json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(other): Result<TraderState, _> = serde_json::from_str(&raw) else {
            continue;
        };
        let same_account = other.open_positions.values().any(|p| p.account_hash == account_hash)
            || other
                .unbracketed_positions
                .values()
                .any(|p| p.account_hash == account_hash);
        if same_account {
            total += other.equity_deployed_usd();
        }
    }
    total
}

pub fn monitoring_metrics(state: &TraderState, rules: &TraderRules) -> Value {
    let unbracketed_count = state.unbracketed_positions.len()
        + state
            .open_positions
            .values()
            .filter(|p| p.oco_order_id.is_none() && rules.playbook.exit.use_oco_at_entry)
            .count();

    let peak = state.sleeve_peak_equity_usd.max(0.01);
    let current = compute_sleeve_equity(state);
    let drawdown_pct = if current >= peak {
        0.0
    } else {
        ((peak - current) / peak) * 100.0
    };

    json!({
        "unbracketed_count": unbracketed_count,
        "reconcile_mismatch_count": state.reconcile_mismatch_count,
        "trading_halted_reason": state.trading_halted_reason,
        "pending_buys": state.pending_buys.len(),
        "open_positions": state.open_positions.len(),
        "sleeve_peak_equity_usd": state.sleeve_peak_equity_usd,
        "drawdown_pct": drawdown_pct,
        "last_fill_to_bracket_seconds": state.last_fill_to_bracket_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::TraderRules;

    #[test]
    fn drawdown_halt_triggers() {
        let rules = TraderRules::default();
        let mut state = TraderState::default();
        state.sleeve_peak_equity_usd = 4000.0;
        state.sim = Some(crate::sim::SimLedger {
            starting_cash_usd: 4000.0,
            cash_usd: 3500.0,
            closed_trades: vec![],
            equity_snapshots: vec![],
        });
        let status = update_drawdown(&mut state, &rules);
        assert!(status.drawdown_pct >= 10.0);
        assert!(status.halted);
    }
}
