//! Paper-trading ledger: simulated fills, exits, and ROI (no Schwab orders).

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use schwab_market_data::MarketDataApi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::agent::state::{save_state, SwingPosition, TraderState};
use crate::capital::exit_prices;
use crate::closure::exit_reason_for_position;
use crate::journal;
use crate::rules::TraderRules;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimLedger {
    pub starting_cash_usd: f64,
    pub cash_usd: f64,
    #[serde(default)]
    pub closed_trades: Vec<ClosedSimTrade>,
    #[serde(default)]
    pub equity_snapshots: Vec<EquitySnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedSimTrade {
    pub trade_id: String,
    pub symbol: String,
    pub quantity: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub opened_at: DateTime<Utc>,
    pub closed_at: DateTime<Utc>,
    pub pnl_usd: f64,
    pub pnl_pct: f64,
    pub exit_reason: String,
    pub hold_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquitySnapshot {
    pub at: DateTime<Utc>,
    pub equity_usd: f64,
    pub cash_usd: f64,
    pub positions_value_usd: f64,
    pub tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimStats {
    pub starting_cash_usd: f64,
    pub current_equity_usd: f64,
    pub cash_usd: f64,
    pub open_positions: usize,
    pub closed_trades: usize,
    pub total_pnl_usd: f64,
    pub roi_pct: f64,
    pub win_rate_pct: f64,
    pub avg_win_usd: f64,
    pub avg_loss_usd: f64,
    pub max_drawdown_pct: f64,
}

pub fn ensure_ledger<'a>(state: &'a mut TraderState, rules: &TraderRules) -> &'a mut SimLedger {
    if state.sim.is_none() {
        let start = rules
            .simulation
            .as_ref()
            .map(|s| s.starting_cash_usd)
            .unwrap_or(rules.capital.fixed_sleeve_cap_usd);
        state.sim = Some(SimLedger {
            starting_cash_usd: start,
            cash_usd: start,
            closed_trades: vec![],
            equity_snapshots: vec![],
        });
    }
    state.sim.as_mut().expect("sim ledger")
}

pub fn sim_tradable_budget(ledger: &SimLedger, rules: &TraderRules, equity_deployed: f64) -> f64 {
    let cap_remaining = (rules.capital.fixed_sleeve_cap_usd - equity_deployed).max(0.0);
    let pct_budget = ledger.cash_usd * (rules.capital.max_pct_of_free_cash / 100.0);
    pct_budget.min(cap_remaining).min(ledger.cash_usd)
}

pub fn record_sim_entry(
    state: &mut TraderState,
    rules: &TraderRules,
    account_hash: &str,
    symbol: &str,
    quantity: f64,
    fill_price: f64,
    position_id: &str,
) -> Result<()> {
    let cost = quantity * fill_price;
    let ledger = ensure_ledger(state, rules);
    anyhow::ensure!(
        cost <= ledger.cash_usd + 0.01,
        "simulation: insufficient cash (${cost:.2} needed, ${:.2} available)",
        ledger.cash_usd
    );
    ledger.cash_usd -= cost;

    let (profit_limit, stop_px, _) = exit_prices(fill_price, rules);
    state.open_positions.insert(
        position_id.to_string(),
        SwingPosition {
            position_id: position_id.to_string(),
            symbol: symbol.to_string(),
            account_hash: account_hash.to_string(),
            quantity,
            entry_price: fill_price,
            opened_at: Utc::now(),
            stop_price: stop_px,
            profit_limit,
            stop_risk_usd: quantity * (fill_price - stop_px).max(0.0),
            market_value_usd: cost,
            oco_order_id: Some("simulated".into()),
            exit_plan_version: 1,
        },
    );
    Ok(())
}

pub async fn process_sim_exits(
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    market: &Arc<MarketDataApi>,
) -> Result<Vec<Value>> {
    if state.sim.is_none() && state.open_positions.is_empty() {
        return Ok(vec![]);
    }
    ensure_ledger(state, rules);

    let symbols: Vec<String> = state
        .open_positions
        .values()
        .map(|p| p.symbol.clone())
        .collect();

    let mut exits = Vec::new();
    for symbol in symbols {
        let quote_raw = market
            .quotes()
            .get_quote(&symbol, Some("quote"), None)
            .await?;
        let last = extract_last(&quote_raw, &symbol).unwrap_or(0.0);
        if last <= 0.0 {
            continue;
        }

        let pos_id = state
            .open_positions
            .values()
            .find(|p| p.symbol == symbol)
            .map(|p| p.position_id.clone());
        let Some(pos_id) = pos_id else {
            continue;
        };

        let pos = state.open_positions.get(&pos_id).cloned();
        let Some(pos) = pos else {
            continue;
        };

        // Mark to market
        if let Some(p) = state.open_positions.get_mut(&pos_id) {
            p.market_value_usd = p.quantity * last;
        }

        let hold_days = (Utc::now() - pos.opened_at).num_days().max(0) as u32;
        let exit_reason = exit_reason_for_position(rules, &pos, last);

        let Some(reason) = exit_reason else {
            continue;
        };

        let exit_price = match reason {
            "stop_loss" => pos.stop_price,
            "profit_target" => pos.profit_limit,
            _ => last,
        };
        let proceeds = pos.quantity * exit_price;
        let pnl = proceeds - (pos.quantity * pos.entry_price);
        let pnl_pct = if pos.entry_price > 0.0 {
            ((exit_price / pos.entry_price) - 1.0) * 100.0
        } else {
            0.0
        };

        if let Some(ledger) = state.sim.as_mut() {
            ledger.cash_usd += proceeds;
            ledger.closed_trades.push(ClosedSimTrade {
                trade_id: pos_id.clone(),
                symbol: pos.symbol.clone(),
                quantity: pos.quantity,
                entry_price: pos.entry_price,
                exit_price,
                opened_at: pos.opened_at,
                closed_at: Utc::now(),
                pnl_usd: pnl,
                pnl_pct,
                exit_reason: reason.to_string(),
                hold_days,
            });
        }

        state.open_positions.remove(&pos_id);
        state.closed_trades_since_learn += 1;
        exits.push(json!({
            "symbol": pos.symbol,
            "exit_reason": reason,
            "exit_price": exit_price,
            "pnl_usd": pnl,
            "pnl_pct": pnl_pct,
        }));

        journal::append_event(
            rules_path,
            "sim_exit_filled",
            json!({
                "symbol": pos.symbol,
                "exit_reason": reason,
                "exit_price": exit_price,
                "pnl_usd": pnl,
                "pnl_pct": pnl_pct,
                "hold_days": hold_days,
            }),
        )?;
    }

    snapshot_equity(state, rules);
    save_state(rules_path, state)?;
    Ok(exits)
}

pub fn snapshot_equity(state: &mut TraderState, rules: &TraderRules) {
    let Some(ledger) = state.sim.as_mut() else {
        return;
    };
    let positions_value: f64 = state
        .open_positions
        .values()
        .map(|p| p.market_value_usd)
        .sum();
    let equity = ledger.cash_usd + positions_value;
    ledger.equity_snapshots.push(EquitySnapshot {
        at: Utc::now(),
        equity_usd: equity,
        cash_usd: ledger.cash_usd,
        positions_value_usd: positions_value,
        tick: state.tick_count,
    });
    if ledger.equity_snapshots.len() > 5000 {
        let drain = ledger.equity_snapshots.len() - 5000;
        ledger.equity_snapshots.drain(0..drain);
    }
    let _ = rules;
}

pub fn compute_stats(state: &TraderState) -> Option<SimStats> {
    let ledger = state.sim.as_ref()?;
    let positions_value: f64 = state
        .open_positions
        .values()
        .map(|p| p.market_value_usd)
        .sum();
    let current_equity = ledger.cash_usd + positions_value;
    let closed = &ledger.closed_trades;
    let total_pnl: f64 = closed.iter().map(|t| t.pnl_usd).sum();
    let wins: Vec<_> = closed.iter().filter(|t| t.pnl_usd > 0.0).collect();
    let losses: Vec<_> = closed.iter().filter(|t| t.pnl_usd <= 0.0).collect();
    let win_rate = if closed.is_empty() {
        0.0
    } else {
        wins.len() as f64 / closed.len() as f64 * 100.0
    };
    let avg_win = if wins.is_empty() {
        0.0
    } else {
        wins.iter().map(|t| t.pnl_usd).sum::<f64>() / wins.len() as f64
    };
    let avg_loss = if losses.is_empty() {
        0.0
    } else {
        losses.iter().map(|t| t.pnl_usd).sum::<f64>() / losses.len() as f64
    };
    let roi = if ledger.starting_cash_usd > 0.0 {
        (current_equity / ledger.starting_cash_usd - 1.0) * 100.0
    } else {
        0.0
    };

    let mut peak = ledger.starting_cash_usd;
    let mut max_dd = 0.0f64;
    for snap in &ledger.equity_snapshots {
        if snap.equity_usd > peak {
            peak = snap.equity_usd;
        }
        if peak > 0.0 {
            let dd = (peak - snap.equity_usd) / peak * 100.0;
            max_dd = max_dd.max(dd);
        }
    }

    Some(SimStats {
        starting_cash_usd: ledger.starting_cash_usd,
        current_equity_usd: current_equity,
        cash_usd: ledger.cash_usd,
        open_positions: state.open_positions.len(),
        closed_trades: closed.len(),
        total_pnl_usd: total_pnl,
        roi_pct: roi,
        win_rate_pct: win_rate,
        avg_win_usd: avg_win,
        avg_loss_usd: avg_loss,
        max_drawdown_pct: max_dd,
    })
}

pub fn reset_ledger(state: &mut TraderState, rules: &TraderRules) {
    let start = rules
        .simulation
        .as_ref()
        .map(|s| s.starting_cash_usd)
        .unwrap_or(rules.capital.fixed_sleeve_cap_usd);
    state.open_positions.clear();
    state.pending_buys.clear();
    state.trades_today = 0;
    state.sim = Some(SimLedger {
        starting_cash_usd: start,
        cash_usd: start,
        closed_trades: vec![],
        equity_snapshots: vec![],
    });
}

fn extract_last(raw: &Value, symbol: &str) -> Option<f64> {
    raw.get(symbol)
        .and_then(|e| e.get("quote"))
        .or_else(|| raw.get("quote"))
        .and_then(|q| q.get("lastPrice"))
        .and_then(|v| v.as_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_tradable_budget_respects_cap() {
        let ledger = SimLedger {
            starting_cash_usd: 4000.0,
            cash_usd: 3000.0,
            closed_trades: vec![],
            equity_snapshots: vec![],
        };
        let rules = TraderRules::default();
        let budget = sim_tradable_budget(&ledger, &rules, 1000.0);
        assert!(budget <= 3000.0);
    }
}
