//! Aggregate options backtest journal + ledger into a report.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::agent::journal;
use crate::agent::paths::backtest_state_path;
use crate::agent::sim::compute_stats;
use crate::agent::state::load_state;
use crate::rules::RulesConfig;

pub fn build_backtest_report(rules_path: &Path, rules: &RulesConfig) -> Result<Value> {
    let state = load_state(&backtest_state_path(rules_path)).unwrap_or_default();
    let stats = compute_stats(&state, rules);
    let events = journal::read_all_backtest(rules_path)?;

    let mut event_counts: HashMap<String, u32> = HashMap::new();
    let mut exit_reasons: HashMap<String, u32> = HashMap::new();
    let mut monthly_pnl: HashMap<String, f64> = HashMap::new();
    let mut trading_days = 0u32;
    let mut entries = 0u32;
    let mut rolls = 0u32;

    for e in &events {
        let Some(t) = e.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        *event_counts.entry(t.to_string()).or_insert(0) += 1;
        let payload = e.get("payload").cloned().unwrap_or(json!({}));
        match t {
            "backtest_day_summary" => {
                trading_days += 1;
            }
            "sim_entry_filled" => entries += 1,
            "defensive_roll" => rolls += 1,
            "sim_exit_filled" => {
                let reason = payload
                    .get("exit_reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                *exit_reasons.entry(reason).or_insert(0) += 1;
                let pnl = payload.get("pnl_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if let Some(ts) = e.get("ts").and_then(|v| v.as_str()) {
                    if let Some(month) = ts.get(0..7) {
                        *monthly_pnl.entry(month.to_string()).or_insert(0.0) += pnl;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(json!({
        "agent_id": rules.agent_id,
        "pricing_model": "black_scholes_vix_iv",
        "caveat": "Synthetic BS marks using VIX as IV proxy — not OPRA historical fills. Use for gate/threshold research, not absolute expectancy.",
        "ledger_stats": stats,
        "event_counts": event_counts,
        "exit_reason_counts": exit_reasons,
        "monthly_closed_pnl_usd": monthly_pnl,
        "trading_days_observed": trading_days,
        "entries": entries,
        "defensive_rolls": rolls,
        "open_positions": state.open_positions.len(),
        "closed_trades": state.sim.as_ref().map(|s| s.closed_trades.len()).unwrap_or(0),
    }))
}
