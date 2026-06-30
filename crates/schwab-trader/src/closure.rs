//! Intraday closure: EOD flatten, no overnight holds, live + sim exits.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use schwab_api::TraderApi;
use serde_json::{json, Value};

use crate::agent::state::{save_state, SwingPosition, TraderState};
use crate::capital::exit_prices;
use crate::config::TraderRuntime;
use crate::journal;
use crate::market_ctx::MarketCtx;
use crate::market_session::{
    entries_blocked, must_flatten_now, must_flatten_now_at, opened_on_prior_et_day_at,
};
use crate::orders::{cancel_order, poll_oco_status, replace_oco_bracket, OcoStatus};
use crate::rules::TraderRules;
use crate::sim;
use crate::technical::fetch_technical_snapshot;
use schwab_cli::order_builder::{
    build_equity_order, parse_duration, parse_session, TradeOrderType, TradeSide,
};
use schwab_cli::order_status::{
    parse_order_id_from_location, wait_for_order, WaitCondition, WaitOptions,
};
use schwab_cli::safety::{execute_trading_order, require_trading_approval};

pub fn entry_block_reason(rules: &TraderRules) -> Option<String> {
    if entries_blocked(rules) {
        if must_flatten_now(rules) {
            return Some("EOD flatten window — no new entries".into());
        }
        return Some("entries blocked (session cutoff or market closed)".into());
    }
    None
}

/// Manual flatten reasons (cancel OCO first). Stop/target are handled by broker OCO.
pub fn is_manual_exit_reason(reason: &str) -> bool {
    matches!(reason, "time_stop" | "eod_flatten" | "overnight_flatten")
}

/// True when a live Schwab OCO is working. Sim positions use `None` or `"simulated"`.
pub fn has_working_broker_oco(pos: &SwingPosition) -> bool {
    pos.oco_order_id
        .as_deref()
        .is_some_and(|id| !id.eq_ignore_ascii_case("simulated"))
}

pub fn exit_reason_for_position(
    rules: &TraderRules,
    pos: &SwingPosition,
    last: f64,
) -> Option<&'static str> {
    exit_reason_for_position_at(rules, pos, last, Utc::now())
}

pub fn exit_reason_for_position_at(
    rules: &TraderRules,
    pos: &SwingPosition,
    last: f64,
    now: DateTime<Utc>,
) -> Option<&'static str> {
    if last <= 0.0 {
        return None;
    }

    if rules.playbook.closure.no_overnight_holds
        && opened_on_prior_et_day_at(pos.opened_at, &rules.schedule.timezone, now)
    {
        return Some("overnight_flatten");
    }
    if must_flatten_now_at(rules, now) {
        return Some("eod_flatten");
    }

    // Stop/target on close — caller may evaluate high/low separately (backtest).
    if !has_working_broker_oco(pos) {
        if last <= pos.stop_price {
            return Some("stop_loss");
        }
        if last >= pos.profit_limit {
            return Some("profit_target");
        }
    }

    let hold_minutes = (now - pos.opened_at).num_minutes().max(0) as u32;
    if rules.is_intraday() && rules.playbook.exit.time_stop_minutes > 0 {
        if hold_minutes >= rules.playbook.exit.time_stop_minutes {
            return Some("time_stop");
        }
    } else {
        let hold_days = (now - pos.opened_at).num_days().max(0) as u32;
        if hold_days < rules.playbook.holding_period.min_days {
            return None;
        }
        if hold_days >= rules.playbook.exit.time_stop_days {
            return Some("time_stop");
        }
    }

    None
}

pub async fn process_closure_exits(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<TraderApi>,
    market: &MarketCtx,
    account_hash: &str,
) -> Result<Vec<Value>> {
    if state.open_positions.is_empty() {
        return Ok(vec![]);
    }

    if runtime.simulate {
        return sim::process_sim_exits(rules_path, rules, state, market).await;
    }

    let mut exits = Vec::new();

    // Trailing stop OCO replace (live only).
    if !runtime.dry_run {
        let trail_updates = process_trailing_stops(runtime, rules_path, rules, state, api, market, account_hash)
            .await?;
        exits.extend(trail_updates);
    }

    // Poll OCO fills — remove positions closed by broker bracket.
    if !runtime.dry_run {
        let oco_exits = process_oco_status_exits(rules_path, state, api, account_hash).await?;
        exits.extend(oco_exits);
    }

    if runtime.dry_run {
        return evaluate_dry_run_exits(rules, state, market).await;
    }

    let symbols: Vec<String> = state
        .open_positions
        .values()
        .map(|p| p.symbol.clone())
        .collect();

    for symbol in symbols {
        let (last, _, _) = market.quote_last_bid_ask(&symbol).await?;

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

        if let Some(p) = state.open_positions.get_mut(&pos_id) {
            p.market_value_usd = p.quantity * last.max(pos.entry_price);
        }

        let Some(reason) = exit_reason_for_position(rules, &pos, last) else {
            continue;
        };

        if !is_manual_exit_reason(reason) {
            continue;
        }

        let exit_price = last.max(0.01);

        let attempt = flatten_live_position(
            runtime,
            api,
            account_hash,
            &pos,
            exit_price,
            reason,
        )
        .await?;

        state.open_positions.remove(&pos_id);
        state.closed_trades_since_learn += 1;
        exits.push(json!({
            "symbol": pos.symbol,
            "exit_reason": reason,
            "exit_price": attempt.fill_price,
            "fill_price": attempt.fill_price,
            "manual": true,
        }));

        journal::append_event(
            rules_path,
            "exit_filled",
            json!({
                "symbol": pos.symbol,
                "exit_reason": reason,
                "fill_price": attempt.fill_price,
                "quantity": pos.quantity,
            }),
        )?;
    }

    if !exits.is_empty() {
        save_state(rules_path, state)?;
    }
    Ok(exits)
}

async fn process_oco_status_exits(
    rules_path: &Path,
    state: &mut TraderState,
    api: &Arc<TraderApi>,
    account_hash: &str,
) -> Result<Vec<Value>> {
    let positions: Vec<SwingPosition> = state.open_positions.values().cloned().collect();
    let mut exits = Vec::new();

    for pos in positions {
        let Some(oco_id) = &pos.oco_order_id else {
            continue;
        };
        let (status, _) = poll_oco_status(api, account_hash, oco_id).await?;
        if status == OcoStatus::FilledExit {
            state.open_positions.remove(&pos.position_id);
            state.closed_trades_since_learn += 1;
            exits.push(json!({
                "symbol": pos.symbol,
                "exit_reason": "oco_filled",
                "oco_order_id": oco_id,
            }));
            journal::append_event(
                rules_path,
                "exit_filled",
                json!({
                    "symbol": pos.symbol,
                    "exit_reason": "oco_filled",
                    "oco_order_id": oco_id,
                }),
            )?;
        }
    }
    Ok(exits)
}

async fn process_trailing_stops(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<TraderApi>,
    market: &MarketCtx,
    account_hash: &str,
) -> Result<Vec<Value>> {
    if !rules.playbook.exit.trailing.enabled {
        return Ok(vec![]);
    }

    let mut updates = Vec::new();
    let positions: Vec<SwingPosition> = state.open_positions.values().cloned().collect();

    for pos in positions {
        let Some(oco_id) = pos
            .oco_order_id
            .as_ref()
            .filter(|_| has_working_broker_oco(&pos))
        else {
            continue;
        };

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

        let trail = rules.playbook.exit.trailing.trail_atr_multiple;
        let new_stop = last - trail * atr;
        if new_stop <= pos.stop_price {
            continue;
        }

        let (_, _, stop_limit) = exit_prices(pos.entry_price, rules, None);
        let new_stop_limit = new_stop * 0.995;

        let bracket = replace_oco_bracket(
            runtime,
            api,
            account_hash,
            oco_id,
            &pos.symbol,
            pos.quantity,
            pos.profit_limit,
            new_stop,
            new_stop_limit.min(stop_limit),
            &rules.execution.oco_duration,
        )
        .await?;

        let new_oco_id = bracket
            .order
            .get("order_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        if let Some(p) = state.open_positions.get_mut(&pos.position_id) {
            p.stop_price = new_stop;
            p.oco_order_id = new_oco_id.clone();
            p.exit_plan_version += 1;
        }

        updates.push(json!({
            "symbol": pos.symbol,
            "action": "trailing_stop_tightened",
            "new_stop": new_stop,
            "oco_order_id": new_oco_id,
        }));

        journal::append_event(
            rules_path,
            "trailing_stop_updated",
            json!({
                "symbol": pos.symbol,
                "new_stop": new_stop,
                "profit_pct": profit_pct,
            }),
        )?;
        save_state(rules_path, state)?;
    }

    Ok(updates)
}

async fn evaluate_dry_run_exits(
    rules: &TraderRules,
    state: &TraderState,
    market: &MarketCtx,
) -> Result<Vec<Value>> {
    let mut would_exit = Vec::new();
    for pos in state.open_positions.values() {
        let (last, _, _) = market.quote_last_bid_ask(&pos.symbol).await?;
        if let Some(reason) = exit_reason_for_position(rules, pos, last) {
            would_exit.push(json!({
                "symbol": pos.symbol,
                "exit_reason": reason,
                "last": last,
                "dry_run": true,
                "would_flatten": is_manual_exit_reason(reason),
                "oco_managed": pos.oco_order_id.is_some() && !is_manual_exit_reason(reason),
            }));
        }
    }
    Ok(would_exit)
}

struct FlattenResult {
    fill_price: f64,
}

async fn flatten_live_position(
    runtime: &TraderRuntime,
    api: &Arc<TraderApi>,
    account_hash: &str,
    pos: &SwingPosition,
    limit_price: f64,
    reason: &str,
) -> Result<FlattenResult> {
    if let Some(oco_id) = &pos.oco_order_id {
        let _ = cancel_order(runtime, api, account_hash, oco_id, "manual flatten").await;
    }

    let order = build_equity_order(
        TradeSide::Sell,
        &pos.symbol,
        pos.quantity,
        TradeOrderType::Limit,
        Some(limit_price),
        parse_duration(Some("DAY"))?,
        parse_session(None)?,
    )?;
    runtime.safety.validate_order(&order, None, None)?;

    let schwab_rt = runtime.as_schwab_runtime();
    require_trading_approval(
        &schwab_rt,
        "trader flatten",
        &format!(
            "Flatten {} x{} @ {:.2} ({reason})",
            pos.symbol, pos.quantity, limit_price
        ),
    )?;

    let place = execute_trading_order(&schwab_rt, api, account_hash, &order).await?;
    let order_id = place
        .get("order_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            place
                .get("location")
                .and_then(|v| v.as_str())
                .and_then(parse_order_id_from_location)
        })
        .context("Missing order id after flatten sell")?;

    let wait = wait_for_order(
        api,
        account_hash,
        &order_id,
        WaitOptions {
            condition: WaitCondition::Filled,
            timeout: Duration::from_secs(120),
            interval: Duration::from_secs(2),
            proceed_on_partial_fill: false,
            requested_quantity: Some(pos.quantity),
        },
    )
    .await?;

    let fill_price = wait
        .order
        .get("price")
        .and_then(|v| v.as_f64())
        .unwrap_or(limit_price);

    Ok(FlattenResult { fill_price })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn manual_exit_reasons_are_time_based() {
        assert!(is_manual_exit_reason("time_stop"));
        assert!(is_manual_exit_reason("eod_flatten"));
        assert!(!is_manual_exit_reason("stop_loss"));
    }

    #[test]
    fn stop_target_skipped_when_oco_present() {
        let rules = TraderRules::default();
        let pos = SwingPosition {
            position_id: "AAPL|2026-01-01".into(),
            symbol: "AAPL".into(),
            account_hash: "h".into(),
            quantity: 10.0,
            entry_price: 100.0,
            opened_at: Utc::now(),
            stop_price: 96.0,
            profit_limit: 108.0,
            stop_risk_usd: 40.0,
            market_value_usd: 1000.0,
            oco_order_id: Some("123".into()),
            exit_plan_version: 1,
        };
        assert!(exit_reason_for_position(&rules, &pos, 95.0).is_none());
        assert!(exit_reason_for_position(&rules, &pos, 110.0).is_none());
    }

    #[test]
    fn min_days_blocks_time_stop() {
        let rules = TraderRules::default();
        let pos = SwingPosition {
            position_id: "AAPL|2026-01-01".into(),
            symbol: "AAPL".into(),
            account_hash: "h".into(),
            quantity: 10.0,
            entry_price: 100.0,
            opened_at: Utc::now(),
            stop_price: 96.0,
            profit_limit: 108.0,
            stop_risk_usd: 40.0,
            market_value_usd: 1000.0,
            oco_order_id: None,
            exit_plan_version: 1,
        };
        // Opened today — min_days=2 should block time_stop
        assert!(exit_reason_for_position(&rules, &pos, 100.0).is_none());
    }
}
