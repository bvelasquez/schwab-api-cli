//! Daily-bar exit simulation for swing backtests (intrabar stop/target via high/low).

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

use crate::agent::state::{save_backtest_state, SwingPosition, TraderState};
use crate::backtest::cache::{BacktestCache, StoredCandle};
use crate::closure::{exit_reason_for_position_at, has_working_broker_oco};
use crate::journal;
use crate::market_ctx::MarketCtx;
use crate::rules::TraderRules;
use crate::sim::{ensure_ledger, sim_fill_price, snapshot_equity_at};
use crate::technical::fetch_technical_snapshot;

pub fn exit_reason_for_bar(
    rules: &TraderRules,
    pos: &SwingPosition,
    bar: &StoredCandle,
    now: DateTime<Utc>,
) -> Option<&'static str> {
    if !has_working_broker_oco(pos) {
        if bar.low <= pos.stop_price {
            return Some("stop_loss");
        }
        if bar.high >= pos.profit_limit {
            return Some("profit_target");
        }
    }
    exit_reason_for_position_at(rules, pos, bar.close, now)
}

pub async fn process_backtest_trailing(
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    market: &MarketCtx,
) -> Result<Vec<Value>> {
    if !rules.playbook.exit.trailing.enabled || state.open_positions.is_empty() {
        return Ok(vec![]);
    }

    let positions: Vec<SwingPosition> = state.open_positions.values().cloned().collect();
    let mut updates = Vec::new();
    for pos in positions {
        let snap = fetch_technical_snapshot(market, rules, &pos.symbol).await?;
        let last = snap.last;
        if last <= 0.0 || pos.entry_price <= 0.0 {
            continue;
        }
        let profit_pct = ((last - pos.entry_price) / pos.entry_price) * 100.0;
        if profit_pct < rules.playbook.exit.trailing.activate_after_profit_pct {
            continue;
        }
        let atr = snap.atr_14.unwrap_or(0.0);
        if atr <= 0.0 {
            continue;
        }
        let new_stop = last - rules.playbook.exit.trailing.trail_atr_multiple * atr;
        if new_stop <= pos.stop_price {
            continue;
        }
        let old_stop = pos.stop_price;
        if let Some(p) = state.open_positions.get_mut(&pos.position_id) {
            p.stop_price = new_stop;
            p.exit_plan_version += 1;
        }
        let event = json!({
            "symbol": pos.symbol,
            "position_id": pos.position_id,
            "action": "sim_trailing_stop_tightened",
            "old_stop": old_stop,
            "new_stop": new_stop,
            "profit_pct": profit_pct,
            "last": last,
        });
        updates.push(event.clone());
        journal::append_backtest_event(rules_path, market.as_of(), "sim_trailing_stop_updated", event)?;
    }
    if !updates.is_empty() {
        save_backtest_state(rules_path, state)?;
    }
    Ok(updates)
}

pub async fn process_backtest_exits(
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    market: &MarketCtx,
    cache: &BacktestCache,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Vec<Value>> {
    ensure_ledger(state, rules);
    let _ = process_backtest_trailing(rules_path, rules, state, market).await?;

    let regime_class = state
        .last_regime
        .as_ref()
        .and_then(|r| r.get("class"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let positions: Vec<SwingPosition> = state.open_positions.values().cloned().collect();
    let mut exits = Vec::new();

    for pos in positions {
        if pos.opened_at.with_timezone(&crate::market_session::trading_tz(&rules.schedule.timezone))
            .date_naive()
            >= day
        {
            continue;
        }

        let Some(bar) = cache.bar_on_date(&pos.symbol, day) else {
            continue;
        };

        if let Some(p) = state.open_positions.get_mut(&pos.position_id) {
            p.market_value_usd = p.quantity * bar.close;
        }

        let Some(reason) = exit_reason_for_bar(rules, &pos, &bar, now) else {
            continue;
        };

        let exit_price = sim_fill_price(reason, &pos, bar.close);
        let proceeds = pos.quantity * exit_price;
        let pnl = proceeds - (pos.quantity * pos.entry_price);
        let pnl_pct = if pos.entry_price > 0.0 {
            ((exit_price / pos.entry_price) - 1.0) * 100.0
        } else {
            0.0
        };
        let hold_minutes = (now - pos.opened_at).num_minutes().max(0) as u32;
        let hold_days = (now - pos.opened_at).num_days().max(0) as u32;

        if let Some(ledger) = state.sim.as_mut() {
            ledger.cash_usd += proceeds;
            ledger.closed_trades.push(crate::sim::ClosedSimTrade {
                trade_id: pos.position_id.clone(),
                symbol: pos.symbol.clone(),
                quantity: pos.quantity,
                entry_price: pos.entry_price,
                exit_price,
                opened_at: pos.opened_at,
                closed_at: now,
                pnl_usd: pnl,
                pnl_pct,
                exit_reason: reason.to_string(),
                hold_days,
                hold_minutes,
                stop_price_at_exit: pos.stop_price,
                profit_limit_at_exit: pos.profit_limit,
                active_profile: state.active_profile.clone(),
                regime_class: regime_class.clone(),
            });
        }

        state.open_positions.remove(&pos.position_id);
        state.closed_trades_since_learn += 1;
        exits.push(json!({
            "symbol": pos.symbol,
            "trade_id": pos.position_id,
            "exit_reason": reason,
            "exit_price": exit_price,
            "bar_high": bar.high,
            "bar_low": bar.low,
            "bar_close": bar.close,
            "pnl_usd": pnl,
            "pnl_pct": pnl_pct,
        }));

        journal::append_backtest_event(
            rules_path,
            now,
            "sim_exit_filled",
            json!({
                "trade_id": pos.position_id,
                "symbol": pos.symbol,
                "quantity": pos.quantity,
                "entry_price": pos.entry_price,
                "exit_reason": reason,
                "exit_price": exit_price,
                "pnl_usd": pnl,
                "pnl_pct": pnl_pct,
                "hold_days": hold_days,
                "hold_minutes": hold_minutes,
                "stop_price": pos.stop_price,
                "profit_limit": pos.profit_limit,
                "active_profile": state.active_profile,
                "regime_class": regime_class,
                "backtest_day": day.to_string(),
            }),
        )?;
    }

    snapshot_equity_at(state, rules, now);
    save_backtest_state(rules_path, state)?;
    Ok(exits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn stop_triggers_on_intrabar_low() {
        let pos = SwingPosition {
            position_id: "T|2024-01-02".into(),
            symbol: "TEST".into(),
            account_hash: "x".into(),
            quantity: 1.0,
            entry_price: 100.0,
            opened_at: Utc.with_ymd_and_hms(2024, 1, 1, 20, 59, 0).unwrap(),
            stop_price: 95.0,
            profit_limit: 110.0,
            stop_risk_usd: 5.0,
            market_value_usd: 100.0,
            oco_order_id: None,
            exit_plan_version: 1,
        };
        let bar = StoredCandle {
            datetime_ms: 1_704_240_000_000,
            open: 100.0,
            high: 101.0,
            low: 94.0,
            close: 99.0,
            volume: 1_000_000.0,
        };
        let rules = crate::rules::TraderRules::default();
        let now = Utc.with_ymd_and_hms(2024, 1, 2, 20, 59, 0).unwrap();
        assert_eq!(
            exit_reason_for_bar(&rules, &pos, &bar, now),
            Some("stop_loss")
        );
    }
}
