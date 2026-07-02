//! Broker ↔ local state reconciliation (positions, pending buys, OCO status).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use schwab_api::TraderApi;
use serde::Serialize;
use serde_json::{json, Value};

use crate::agent::state::{position_id, save_state, PendingBuy, SwingPosition, TraderState, UnbracketedPosition};
use crate::capital::exit_prices;
use crate::config::TraderRuntime;
use crate::journal;
use crate::orders::{place_oco_bracket_with_retry, poll_oco_status, OcoStatus};
use crate::rules::TraderRules;
use schwab_cli::order_status::{order_filled_quantity, order_status};

#[derive(Debug, Clone, Serialize, Default)]
pub struct ReconcileReport {
    pub adopted_positions: Vec<String>,
    pub removed_positions: Vec<String>,
    pub pending_buys_resolved: u32,
    pub oco_filled: Vec<String>,
    pub brackets_recovered: u32,
    pub mismatches: Vec<Value>,
}

#[derive(Debug, Clone)]
struct LiveEquityPosition {
    symbol: String,
    quantity: f64,
    average_price: f64,
    market_value: f64,
}

pub async fn reconcile_tick(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<TraderApi>,
    account_hash: &str,
) -> Result<ReconcileReport> {
    if runtime.dry_run || runtime.simulate {
        return Ok(ReconcileReport::default());
    }

    let mut report = ReconcileReport::default();
    let live = fetch_equity_positions(api, account_hash, rules).await?;
    let live_by_symbol: HashMap<String, LiveEquityPosition> = live
        .into_iter()
        .map(|p| (p.symbol.clone(), p))
        .collect();

    // Resolve pending buys
    let pending: Vec<PendingBuy> = state.pending_buys.clone();
    for pending_buy in pending {
        let order = api
            .orders()
            .get(account_hash, &pending_buy.order_id)
            .await?;
        let order_val = order;
        let status = order_status(&order_val).unwrap_or_default();

        if status == "FILLED" {
            report.pending_buys_resolved += 1;
            state.pending_buys.retain(|p| p.order_id != pending_buy.order_id);
            let filled_qty = order_filled_quantity(&order_val).unwrap_or(0.0);
            if filled_qty > 0.0 {
                adopt_filled_buy(
                    runtime,
                    rules_path,
                    rules,
                    state,
                    api,
                    account_hash,
                    &pending_buy.symbol,
                    filled_qty,
                    &order_val,
                    &mut report,
                )
                .await?;
            }
        } else if matches!(status.as_str(), "CANCELED" | "CANCELLED" | "REJECTED" | "EXPIRED") {
            state.pending_buys.retain(|p| p.order_id != pending_buy.order_id);
            report.pending_buys_resolved += 1;
        }
    }

    // Poll OCO orders — remove positions when bracket filled
    let positions: Vec<SwingPosition> = state.open_positions.values().cloned().collect();
    for pos in positions {
        if let Some(oco_id) = &pos.oco_order_id {
            match poll_oco_status(api, account_hash, oco_id).await {
                Ok((OcoStatus::FilledExit, _)) => {
                    state.open_positions.remove(&pos.position_id);
                    state.closed_trades_since_learn += 1;
                    report.oco_filled.push(pos.symbol.clone());
                    let _ = journal::append_event(
                        rules_path,
                        "exit_reconciled",
                        json!({
                            "symbol": pos.symbol,
                            "reason": "oco_filled",
                            "oco_order_id": oco_id,
                        }),
                    );
                }
                Ok((OcoStatus::Canceled, _)) => {
                    report.mismatches.push(json!({
                        "type": "oco_canceled",
                        "symbol": pos.symbol,
                        "oco_order_id": oco_id,
                    }));
                    state.reconcile_mismatch_count += 1;
                }
                _ => {}
            }
        }
    }

    // State positions missing at broker (already closed)
    let open_ids: Vec<String> = state.open_positions.keys().cloned().collect();
    for pos_id in open_ids {
        let pos = match state.open_positions.get(&pos_id) {
            Some(p) => p.clone(),
            None => continue,
        };
        if !live_by_symbol.contains_key(&pos.symbol) {
            state.open_positions.remove(&pos_id);
            state.closed_trades_since_learn += 1;
            report.removed_positions.push(pos.symbol.clone());
            let _ = journal::append_event(
                rules_path,
                "exit_reconciled",
                json!({
                    "symbol": pos.symbol,
                    "reason": "broker_position_gone",
                }),
            );
        } else if let Some(live_pos) = live_by_symbol.get(&pos.symbol) {
            if (live_pos.quantity - pos.quantity).abs() > 0.01 {
                report.mismatches.push(json!({
                    "type": "quantity_mismatch",
                    "symbol": pos.symbol,
                    "state_qty": pos.quantity,
                    "broker_qty": live_pos.quantity,
                }));
                state.reconcile_mismatch_count += 1;
                if let Some(p) = state.open_positions.get_mut(&pos_id) {
                    p.quantity = live_pos.quantity;
                    p.market_value_usd = live_pos.market_value;
                }
            }
        }
    }

    // Broker positions not in state — adopt
    for (symbol, live_pos) in &live_by_symbol {
        if state.has_open_symbol(symbol) {
            continue;
        }
        let pos_id = position_id(symbol, &rules.schedule.timezone);
        let (profit_limit, stop_px, _) = exit_prices(live_pos.average_price, rules, None);
        state.open_positions.insert(
            pos_id.clone(),
            SwingPosition {
                position_id: pos_id,
                symbol: symbol.clone(),
                account_hash: account_hash.to_string(),
                quantity: live_pos.quantity,
                entry_price: live_pos.average_price,
                opened_at: Utc::now(),
                stop_price: stop_px,
                profit_limit,
                stop_risk_usd: live_pos.quantity * (live_pos.average_price - stop_px).max(0.0),
                market_value_usd: live_pos.market_value,
                oco_order_id: None,
                exit_plan_version: 1,
            },
        );
        state.unbracketed_positions.insert(
            symbol.clone(),
            UnbracketedPosition {
                symbol: symbol.clone(),
                account_hash: account_hash.to_string(),
                quantity: live_pos.quantity,
                entry_price: live_pos.average_price,
                fill_order_id: String::new(),
                detected_at: Utc::now(),
                bracket_attempts: 0,
            },
        );
        state.trading_halted_reason = Some(format!("unbracketed position adopted: {symbol}"));
        report.adopted_positions.push(symbol.clone());
        report.mismatches.push(json!({
            "type": "adopted_broker_position",
            "symbol": symbol,
        }));
        state.reconcile_mismatch_count += 1;
    }

    // Recover brackets for unbracketed positions
    let unbracketed: Vec<UnbracketedPosition> = state.unbracketed_positions.values().cloned().collect();
    for mut ub in unbracketed {
        if !rules.playbook.exit.use_oco_at_entry {
            state.unbracketed_positions.remove(&ub.symbol);
            continue;
        }
        ub.bracket_attempts += 1;
        let (profit_limit, stop_px, stop_limit_px) = exit_prices(ub.entry_price, rules, None);
        match place_oco_bracket_with_retry(
            runtime,
            api,
            account_hash,
            &ub.symbol,
            ub.quantity,
            profit_limit,
            stop_px,
            stop_limit_px,
            &rules.execution.oco_duration,
            rules.execution.place_bracket_within_seconds,
            3,
        )
        .await
        {
            Ok(bracket) => {
                let oco_id = bracket
                    .order
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(pos) = state
                    .open_positions
                    .values_mut()
                    .find(|p| p.symbol == ub.symbol)
                {
                    pos.oco_order_id = oco_id.clone();
                    pos.stop_price = stop_px;
                    pos.profit_limit = profit_limit;
                    pos.exit_plan_version += 1;
                }
                state.unbracketed_positions.remove(&ub.symbol);
                if state
                    .trading_halted_reason
                    .as_deref()
                    .is_some_and(|r| r.contains(&ub.symbol))
                {
                    state.trading_halted_reason = None;
                }
                report.brackets_recovered += 1;
                let _ = journal::append_event(
                    rules_path,
                    "bracket_recovered",
                    json!({
                        "symbol": ub.symbol,
                        "oco_order_id": oco_id,
                        "attempts": bracket.attempts,
                    }),
                );
            }
            Err(err) => {
                let sym = ub.symbol.clone();
                let attempts = ub.bracket_attempts;
                state.unbracketed_positions.insert(
                    sym.clone(),
                    UnbracketedPosition {
                        bracket_attempts: attempts,
                        ..ub
                    },
                );
                if attempts >= 5 {
                    state.trading_halted_reason =
                        Some(format!("bracket recovery failed for {sym}: {err}"));
                }
            }
        }
    }

    if !report.mismatches.is_empty() || report.brackets_recovered > 0 {
        save_state(rules_path, state)?;
    }

    Ok(report)
}

async fn adopt_filled_buy(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<TraderApi>,
    account_hash: &str,
    symbol: &str,
    filled_qty: f64,
    order_val: &Value,
    report: &mut ReconcileReport,
) -> Result<()> {
    if state.has_open_symbol(symbol) {
        return Ok(());
    }

    let fill_price = order_val
        .get("price")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let entry_price = if fill_price > 0.0 {
        fill_price
    } else {
        return Ok(());
    };

    let pos_id = position_id(symbol, &rules.schedule.timezone);
    let (profit_limit, stop_px, stop_limit_px) = exit_prices(entry_price, rules, None);
    let mut oco_order_id = None;

    if rules.playbook.exit.use_oco_at_entry {
        match place_oco_bracket_with_retry(
            runtime,
            api,
            account_hash,
            symbol,
            filled_qty,
            profit_limit,
            stop_px,
            stop_limit_px,
            &rules.execution.oco_duration,
            rules.execution.place_bracket_within_seconds,
            3,
        )
        .await
        {
            Ok(bracket) => {
                oco_order_id = bracket
                    .order
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                report.brackets_recovered += 1;
            }
            Err(_) => {
                state.unbracketed_positions.insert(
                    symbol.to_string(),
                    UnbracketedPosition {
                        symbol: symbol.to_string(),
                        account_hash: account_hash.to_string(),
                        quantity: filled_qty,
                        entry_price,
                        fill_order_id: order_val
                            .get("orderId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        detected_at: Utc::now(),
                        bracket_attempts: 1,
                    },
                );
                state.trading_halted_reason =
                    Some(format!("unbracketed position: {symbol} (reconcile)"));
            }
        }
    }

    state.open_positions.insert(
        pos_id.clone(),
        SwingPosition {
            position_id: pos_id,
            symbol: symbol.to_string(),
            account_hash: account_hash.to_string(),
            quantity: filled_qty,
            entry_price,
            opened_at: Utc::now(),
            stop_price: stop_px,
            profit_limit,
            stop_risk_usd: filled_qty * (entry_price - stop_px).max(0.0),
            market_value_usd: filled_qty * entry_price,
            oco_order_id,
            exit_plan_version: 1,
        },
    );
    state.trades_today += 1;
    report.adopted_positions.push(symbol.to_string());
    save_state(rules_path, state)?;
    Ok(())
}

async fn fetch_equity_positions(
    api: &Arc<TraderApi>,
    account_hash: &str,
    rules: &TraderRules,
) -> Result<Vec<LiveEquityPosition>> {
    let account = api.accounts().get(account_hash, Some("positions")).await?;
    let positions = account
        .securities_account
        .and_then(|a| a.positions)
        .unwrap_or_default();

    let mut out = Vec::new();
    for pos in positions {
        let instrument = pos.instrument.as_ref();
        let asset_type = instrument
            .and_then(|i| i.r#type.as_deref())
            .unwrap_or("");
        if !asset_type.eq_ignore_ascii_case("EQUITY") {
            continue;
        }
        let symbol = instrument
            .and_then(|i| i.symbol.as_deref())
            .unwrap_or("")
            .trim()
            .to_uppercase();
        if symbol.is_empty() || rules.is_core_holding(&symbol) {
            continue;
        }
        let qty = pos.long_quantity.unwrap_or(0.0);
        if qty <= 0.0 {
            continue;
        }
        out.push(LiveEquityPosition {
            symbol,
            quantity: qty,
            average_price: pos.average_price.unwrap_or(0.0),
            market_value: pos.market_value.unwrap_or(0.0),
        });
    }
    Ok(out)
}
