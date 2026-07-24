//! Broker-resident profit-target close order for open option spreads.
//!
//! Schwab's live order API rejects a stop trigger on a multi-leg complex option order
//! (confirmed via `orders preview`: `complexOrderStrategyType: VERTICAL/IRON_CONDOR` +
//! `orderType: STOP_LIMIT` + `stopPrice` -> HTTP 400 "Stop price must be populated only for
//! stop orders" — the identical shape works fine on a single-leg option). A plain GTC complex
//! `NET_DEBIT` limit order to close at the profit target, however, is accepted structurally.
//! So only the profit-target side of exit protection can live on Schwab's servers; stop-loss
//! and DTE-close remain dependent on the agent's own tick loop (see `docs/OPTIONS_RULES.md`).

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use schwab_api::TraderApi;
use serde_json::{json, Value};

use schwab_api::models::order::{
    ComplexOrderStrategyType, OrderDuration, OrderInstruction, OrderSession, OrderTypeRequest,
};

use crate::config::RuntimeConfig;
use crate::options::{build_order_for_strategy, StrategyKind};
use crate::order_builder::{build_complex_option_order, OrderLegSpec};
use crate::safety::{execute_trading_order, require_trading_approval};

use super::state::AgentState;
use crate::rules::RulesConfig;

/// Debit (per share) at which `profit_target_pct` is captured, given the entry credit.
/// Mirrors the `profit_pct` formula in `agent::exits`: `((entry - debit) / entry) * 100`.
pub fn profit_target_debit(entry_credit: f64, profit_target_pct: f64) -> f64 {
    let target = entry_credit * (1.0 - profit_target_pct / 100.0);
    target.max(0.01)
}

fn invert_instruction(raw: &str) -> Option<OrderInstruction> {
    match raw {
        "BUY_TO_OPEN" => Some(OrderInstruction::SellToClose),
        "SELL_TO_OPEN" => Some(OrderInstruction::BuyToClose),
        _ => None,
    }
}

fn complex_strategy_from_str(raw: &str) -> ComplexOrderStrategyType {
    match raw {
        "VERTICAL" => ComplexOrderStrategyType::Vertical,
        "IRON_CONDOR" => ComplexOrderStrategyType::IronCondor,
        _ => ComplexOrderStrategyType::Custom,
    }
}

/// Build a resting GTC complex `NET_DEBIT` order that closes `entry_order`'s legs at the
/// profit-target debit, by inverting each leg's opening instruction (`BUY_TO_OPEN` ->
/// `SELL_TO_CLOSE`, `SELL_TO_OPEN` -> `BUY_TO_CLOSE`) rather than re-deriving strikes/symbols.
pub fn build_profit_target_close_order(
    entry_order: &Value,
    entry_credit: f64,
    profit_target_pct: f64,
) -> Result<Value> {
    let legs_json = entry_order
        .get("orderLegCollection")
        .and_then(|v| v.as_array())
        .context("entry order missing orderLegCollection")?;
    if legs_json.len() < 2 {
        bail!("profit-target close order requires a multi-leg entry order");
    }

    let leg_specs: Vec<OrderLegSpec> = legs_json
        .iter()
        .map(|leg| {
            let raw_instruction = leg
                .get("instruction")
                .and_then(|v| v.as_str())
                .context("leg missing instruction")?;
            let instruction = invert_instruction(raw_instruction).with_context(|| {
                format!("cannot invert opening instruction `{raw_instruction}`")
            })?;
            let symbol = leg
                .pointer("/instrument/symbol")
                .and_then(|v| v.as_str())
                .context("leg missing instrument.symbol")?
                .to_string();
            let quantity = leg
                .get("quantity")
                .and_then(|v| v.as_f64())
                .context("leg missing quantity")?;
            Ok(OrderLegSpec {
                instruction,
                symbol,
                asset_type: "OPTION",
                quantity,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let complex_strategy = entry_order
        .get("complexOrderStrategyType")
        .and_then(|v| v.as_str())
        .map(complex_strategy_from_str)
        .unwrap_or(ComplexOrderStrategyType::Custom);

    let target_debit = profit_target_debit(entry_credit, profit_target_pct);

    build_complex_option_order(
        complex_strategy,
        OrderTypeRequest::NetDebit,
        leg_specs,
        Some(target_debit),
        OrderDuration::GoodTillCancel,
        OrderSession::Normal,
        None,
    )
}

#[derive(Debug, Clone)]
pub struct ProtectivePlaceResult {
    pub order: Value,
    pub order_id: Option<String>,
    pub attempts: u32,
}

async fn place_profit_target_order_once(
    runtime: &RuntimeConfig,
    trader: &TraderApi,
    account_hash: &str,
    order: &Value,
) -> Result<Value> {
    require_trading_approval(
        runtime,
        "protective order",
        &format!("Place resting profit-target close on {account_hash}"),
    )?;
    runtime.safety.validate_order(order, None, None)?;
    execute_trading_order(runtime, trader, account_hash, order).await
}

/// Place the profit-target close order with retry. Failure here does NOT block the entry
/// (the position is still opened without broker-side protection) — callers should record the
/// failure on `TrackedPosition::protective_order_attempts` and let the reconcile pass retry.
pub async fn place_profit_target_order_with_retry(
    runtime: &RuntimeConfig,
    trader: &TraderApi,
    account_hash: &str,
    entry_order: &Value,
    entry_credit: f64,
    profit_target_pct: f64,
    max_attempts: u32,
    max_seconds: u64,
) -> Result<ProtectivePlaceResult> {
    let order = build_profit_target_close_order(entry_order, entry_credit, profit_target_pct)?;

    let started = Instant::now();
    let deadline = started + Duration::from_secs(max_seconds.max(1));
    let mut attempts = 0u32;
    let mut last_err = None;

    while attempts < max_attempts.max(1) && Instant::now() < deadline {
        attempts += 1;
        match place_profit_target_order_once(runtime, trader, account_hash, &order).await {
            Ok(placed) => {
                let order_id = placed
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                return Ok(ProtectivePlaceResult {
                    order: placed,
                    order_id,
                    attempts,
                });
            }
            Err(e) => {
                last_err = Some(e);
                if Instant::now() < deadline && attempts < max_attempts.max(1) {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("profit-target order placement failed")))
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReconcileProtectiveSummary {
    pub placed: Vec<String>,
    pub failed: Vec<String>,
    /// Open positions still missing a protective order after this pass (0 attempts left
    /// pending are still counted — the reconcile pass retries again next tick).
    pub unprotected_count: usize,
}

/// Retry placing the profit-target close order for any open position missing one.
/// Runs every tick (called from `reconcile_open_positions`'s caller in `tick_once`) so a
/// placement failure right after entry — or a position that predates this feature and has
/// no `entry_params` to rebuild from — keeps getting retried rather than staying naked
/// indefinitely. Positions with no stored `entry_params` cannot be rebuilt and are counted
/// as unprotected but not retried (nothing to retry with).
pub async fn reconcile_protective_orders(
    runtime: &RuntimeConfig,
    trader: &TraderApi,
    rules: &RulesConfig,
    state: &mut AgentState,
) -> ReconcileProtectiveSummary {
    let mut summary = ReconcileProtectiveSummary::default();
    if !rules.execution.protective_order.enabled {
        return summary;
    }

    let candidate_ids: Vec<String> = state
        .open_positions
        .iter()
        .filter(|(_, p)| p.protective_order_id.is_none())
        .map(|(id, _)| id.clone())
        .collect();

    for position_id in candidate_ids {
        summary.unprotected_count += 1;
        let Some(position) = state.open_positions.get(&position_id).cloned() else {
            continue;
        };

        let rebuild = (|| -> Result<Value> {
            let entry_params = position
                .entry_params
                .clone()
                .context("no stored entry_params to rebuild order")?;
            let entry_credit = position
                .entry_credit
                .context("no entry_credit recorded")?;
            let kind = StrategyKind::parse(&position.strategy)?;
            let entry_order = build_order_for_strategy(kind, &entry_params)?;
            build_profit_target_close_order(
                &entry_order,
                entry_credit,
                rules.exit_rules.profit_target_pct,
            )
        })();

        let close_order = match rebuild {
            Ok(order) => order,
            Err(e) => {
                summary.failed.push(format!("{position_id}: {e:#}"));
                continue;
            }
        };

        match place_profit_target_order_once(runtime, trader, &position.account_hash, &close_order)
            .await
        {
            Ok(placed) => {
                let order_id = placed
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(p) = state.open_positions.get_mut(&position_id) {
                    p.protective_order_id = order_id.clone();
                    p.protective_order_status = Some("WORKING".to_string());
                    p.protective_order_attempts = 0;
                }
                summary.unprotected_count -= 1;
                summary.placed.push(position_id.clone());
                state.record_action(
                    "protective_order_reconciled",
                    json!({
                        "position_id": position_id,
                        "order_id": order_id,
                    }),
                );
            }
            Err(e) => {
                if let Some(p) = state.open_positions.get_mut(&position_id) {
                    p.protective_order_attempts = p.protective_order_attempts.saturating_add(1);
                }
                summary.failed.push(format!("{position_id}: {e:#}"));
            }
        }
    }

    summary
}

/// Cancel a resting protective order (e.g. before the mechanical tick loop closes the
/// position early on a stop/DTE/thesis exit, to avoid a duplicate-close race).
pub async fn cancel_protective_order(
    runtime: &RuntimeConfig,
    trader: &TraderApi,
    account_hash: &str,
    order_id: &str,
) -> Result<Value> {
    require_trading_approval(
        runtime,
        "cancel protective order",
        &format!("Cancel resting profit-target order {order_id}"),
    )?;
    let result = trader.orders().cancel(account_hash, order_id).await?;
    Ok(serde_json::to_value(result)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn profit_target_debit_matches_exit_formula() {
        // entry credit 0.85, 50% target -> close at 0.425 debit
        let debit = profit_target_debit(0.85, 50.0);
        assert!((debit - 0.425).abs() < 1e-9);
    }

    #[test]
    fn profit_target_debit_floors_at_one_cent() {
        // 100% target would compute to 0.0 debit; floor to a placeable minimum.
        let debit = profit_target_debit(0.85, 100.0);
        assert!((debit - 0.01).abs() < 1e-9);
    }

    #[test]
    fn builds_close_order_inverting_vertical_legs() {
        let entry_order = json!({
            "orderType": "NET_CREDIT",
            "complexOrderStrategyType": "VERTICAL",
            "orderStrategyType": "SINGLE",
            "orderLegCollection": [
                {"instruction": "SELL_TO_OPEN", "quantity": 2.0, "instrument": {"symbol": "SPY   260821P00739000", "assetType": "OPTION"}},
                {"instruction": "BUY_TO_OPEN", "quantity": 2.0, "instrument": {"symbol": "SPY   260821P00736000", "assetType": "OPTION"}}
            ]
        });

        let close = build_profit_target_close_order(&entry_order, 0.85, 50.0).unwrap();

        assert_eq!(close["orderType"], "NET_DEBIT");
        assert_eq!(close["complexOrderStrategyType"], "VERTICAL");
        assert_eq!(close["duration"], "GOOD_TILL_CANCEL");
        let legs = close["orderLegCollection"].as_array().unwrap();
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0]["instruction"], "BUY_TO_CLOSE");
        assert_eq!(legs[0]["instrument"]["symbol"], "SPY   260821P00739000");
        assert_eq!(legs[1]["instruction"], "SELL_TO_CLOSE");
        assert_eq!(legs[1]["instrument"]["symbol"], "SPY   260821P00736000");
    }

    #[test]
    fn rejects_single_leg_entry_order() {
        let entry_order = json!({
            "complexOrderStrategyType": "NONE",
            "orderLegCollection": [
                {"instruction": "BUY_TO_OPEN", "quantity": 1.0, "instrument": {"symbol": "SPY   260821P00739000", "assetType": "OPTION"}}
            ]
        });
        assert!(build_profit_target_close_order(&entry_order, 0.85, 50.0).is_err());
    }
}
