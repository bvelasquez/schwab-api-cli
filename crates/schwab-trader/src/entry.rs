use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use schwab_api::TraderApi;
use schwab_market_data::MarketDataApi;
use serde::Serialize;
use serde_json::{json, Value};

use crate::agent::state::{position_id, save_state, PendingBuy, SwingPosition, TraderState, UnbracketedPosition};
use crate::capital::{
    capital_check_to_json, compute_capital_check, exit_prices,
};
use crate::config::TraderRuntime;
use crate::journal;
use crate::orders::place_oco_bracket_with_retry;
use crate::rules::TraderRules;
use crate::sim::record_sim_entry;
use crate::technical::{fetch_technical_snapshot, TechnicalSnapshot};
use schwab_cli::order_builder::{
    build_equity_order, parse_duration, parse_session, TradeOrderType, TradeSide,
};
use schwab_cli::order_status::{
    parse_order_id_from_location, wait_for_order, WaitCondition, WaitOptions,
};
use schwab_cli::portfolio::estimate_equity_buy_cost;
use schwab_cli::safety::{execute_trading_order, require_trading_approval};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    Skipped,
    DryRun,
    Simulated,
    Submitted,
    Filled,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryAttempt {
    pub status: EntryStatus,
    pub symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capital_check: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bracket: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
}

pub fn compute_entry_quantity(
    rules: &TraderRules,
    entry_price: f64,
    stop_price: f64,
    tradable_budget: f64,
) -> f64 {
    compute_position_sizing(rules, entry_price, stop_price, tradable_budget).quantity
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionSizing {
    pub quantity: f64,
    pub position_size_usd: f64,
    pub risk_pct_size_usd: f64,
    pub max_pct_size_usd: f64,
    pub budget_cap_usd: f64,
    pub binding_constraint: String,
}

pub fn compute_position_sizing(
    rules: &TraderRules,
    entry_price: f64,
    stop_price: f64,
    tradable_budget: f64,
) -> PositionSizing {
    let empty = PositionSizing {
        quantity: 0.0,
        position_size_usd: 0.0,
        risk_pct_size_usd: 0.0,
        max_pct_size_usd: 0.0,
        budget_cap_usd: tradable_budget.max(0.0),
        binding_constraint: "none".into(),
    };
    if entry_price <= 0.0 || tradable_budget <= 0.0 {
        return empty;
    }

    let sleeve_cap = rules.capital.fixed_sleeve_cap_usd;
    let risk_budget = sleeve_cap * rules.playbook.entry.position_size.risk_per_trade_pct / 100.0;
    let stop_per_share = (entry_price - stop_price).max(0.01);
    let qty_by_risk = (risk_budget / stop_per_share).floor();

    let max_pct_size_usd =
        sleeve_cap * rules.playbook.entry.position_size.max_position_pct / 100.0;
    let max_position_value = max_pct_size_usd.min(tradable_budget);
    let qty_by_position_cap = (max_position_value / entry_price).floor();
    let qty_by_budget = (tradable_budget / entry_price).floor();

    let quantity = qty_by_risk
        .min(qty_by_position_cap)
        .min(qty_by_budget)
        .max(0.0);

    let risk_pct_size_usd = qty_by_risk * entry_price;
    let budget_cap_usd = qty_by_budget * entry_price;
    let position_size_usd = quantity * entry_price;

    let binding_constraint = if quantity <= 0.0 {
        "none".to_string()
    } else if quantity == qty_by_budget && qty_by_budget <= qty_by_risk.min(qty_by_position_cap) {
        "tradable_budget".into()
    } else if quantity == qty_by_position_cap
        && qty_by_position_cap <= qty_by_risk.min(qty_by_budget)
    {
        "max_position_pct".into()
    } else {
        "risk_per_trade_pct".into()
    };

    PositionSizing {
        quantity,
        position_size_usd,
        risk_pct_size_usd,
        max_pct_size_usd,
        budget_cap_usd,
        binding_constraint,
    }
}

pub fn log_position_sizing(sizing: &PositionSizing, max_position_pct: f64) {
    if sizing.quantity <= 0.0 {
        return;
    }
    match sizing.binding_constraint.as_str() {
        "max_position_pct" => tracing::info!(
            target: "capital",
            "position_size=${:.0} (clamped by max_position_pct={max_position_pct:.1}%; risk_pct_size=${:.0})",
            sizing.position_size_usd,
            sizing.risk_pct_size_usd,
        ),
        "tradable_budget" => tracing::info!(
            target: "capital",
            "position_size=${:.0} (clamped by tradable_budget=${:.0}; risk_pct_size=${:.0})",
            sizing.position_size_usd,
            sizing.budget_cap_usd,
            sizing.risk_pct_size_usd,
        ),
        _ => tracing::info!(
            target: "capital",
            "position_size=${:.0} (risk_per_trade_pct method; within max_position_pct={max_position_pct:.1}%)",
            sizing.position_size_usd,
        ),
    }
}

fn record_sizing_streak(state: &mut TraderState, sizing: &PositionSizing, simulate: bool, rules_path: &Path) {
    if sizing.quantity <= 0.0 {
        return;
    }
    if sizing.binding_constraint == "max_position_pct" {
        state.sizing_max_pct_binding_streak += 1;
    } else {
        state.sizing_max_pct_binding_streak = 0;
    }
    if simulate
        && state.sizing_max_pct_binding_streak >= 3
        && !state.sizing_redundant_risk_warned
    {
        state.sizing_redundant_risk_warned = true;
        let msg = "risk_per_trade_pct sizing is consistently non-binding (max_position_pct clamps every recent entry); \
                   consider reconciling risk_per_trade_pct and max_position_pct in rules YAML";
        tracing::warn!(target: "capital", "{msg}");
        let _ = journal::append_event(
            rules_path,
            "sizing_config_hint",
            json!({
                "message": msg,
                "streak": state.sizing_max_pct_binding_streak,
                "risk_pct_size_usd": sizing.risk_pct_size_usd,
                "max_pct_size_usd": sizing.max_pct_size_usd,
            }),
        );
    }
}

pub async fn attempt_entry(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<TraderApi>,
    market: &Arc<MarketDataApi>,
    account_hash: &str,
    symbol: &str,
    limit_price_override: Option<f64>,
    quantity_override: Option<f64>,
    bracket: bool,
    source: &str,
) -> Result<EntryAttempt> {
    let symbol = symbol.trim().to_uppercase();

    state.reset_trades_day(&rules.schedule.timezone);
    if let Some(reason) = state.entry_block_reason(rules) {
        return Ok(skipped(&symbol, reason));
    }

    if rules.is_core_holding(&symbol) {
        return Ok(skipped(&symbol, "core_holding"));
    }
    if state.has_open_symbol(&symbol) {
        return Ok(skipped(&symbol, "already_open"));
    }
    if rules.playbook.direction != "long" {
        return Ok(skipped(&symbol, "v1 supports long entries only"));
    }

    if rules.is_blocked_symbol(&symbol) {
        return Ok(skipped(&symbol, "blocked_symbol"));
    }
    if !state.unbracketed_positions.is_empty()
        && rules.execution.require_bracket_before_entry_resume
    {
        return Ok(skipped(
            &symbol,
            "unbracketed position exists — entries halted",
        ));
    }

    let snap = fetch_technical_snapshot(market, rules, &symbol).await?;
    let limit_price = limit_price_override
        .unwrap_or_else(|| resolve_entry_limit_price(&snap, rules));
    if limit_price <= 0.0 {
        return Ok(skipped(&symbol, "could not resolve limit price"));
    }

    let capital_preview = compute_capital_check(
        api,
        rules,
        state,
        account_hash,
        None,
        None,
        runtime.simulate,
        Some(rules_path),
    )
    .await?;
    let (profit_limit, stop_price, stop_limit) = exit_prices(limit_price, rules);
    let sizing = compute_position_sizing(
        rules,
        limit_price,
        stop_price,
        capital_preview.tradable_budget_usd,
    );
    let quantity = quantity_override.unwrap_or(sizing.quantity);
    log_position_sizing(
        &if quantity_override.is_some() {
            PositionSizing {
                quantity,
                ..sizing.clone()
            }
        } else {
            sizing.clone()
        },
        rules.playbook.entry.position_size.max_position_pct,
    );
    if quantity < 1.0 {
        return Ok(skipped(
            &symbol,
            format!(
                "quantity below 1 (budget ${:.2}, price ${limit_price:.2})",
                capital_preview.tradable_budget_usd
            ),
        ));
    }

    let estimated_cost = estimate_equity_buy_cost(quantity, "LIMIT", Some(limit_price), None)?;
    let stop_risk = quantity * (limit_price - stop_price).max(0.0);
    let capital = compute_capital_check(
        api,
        rules,
        state,
        account_hash,
        Some(estimated_cost),
        Some(stop_risk),
        runtime.simulate,
        Some(rules_path),
    )
    .await?;

    if !capital.passed {
        return Ok(skipped(
            &symbol,
            capital
                .reject_reason
                .clone()
                .unwrap_or_else(|| "capital_check failed".into()),
        ));
    }

    if runtime.simulate && quantity_override.is_none() {
        record_sizing_streak(state, &sizing, true, rules_path);
    }

    let order = build_equity_order(
        TradeSide::Buy,
        &symbol,
        quantity,
        TradeOrderType::Limit,
        Some(limit_price),
        parse_duration(Some("DAY"))?,
        parse_session(None)?,
    )?;
    runtime.safety.validate_order(&order, None, None)?;

    let bracket_preview = json!({
        "profit_limit": profit_limit,
        "stop_price": stop_price,
        "stop_limit": stop_limit,
        "oco_duration": rules.execution.oco_duration,
        "enabled": bracket && rules.playbook.exit.use_oco_at_entry,
    });

    journal::append_event(
        rules_path,
        "entry_signal",
        json!({
            "source": source,
            "symbol": symbol,
            "quantity": quantity,
            "limit_price": limit_price,
            "position_sizing": sizing,
            "capital_check": capital_check_to_json(&capital),
            "bracket_preview": bracket_preview,
            "dry_run": runtime.dry_run,
            "simulate": runtime.simulate,
        }),
    )?;

    if runtime.dry_run {
        return Ok(EntryAttempt {
            status: EntryStatus::DryRun,
            symbol,
            reason: None,
            quantity: Some(quantity),
            limit_price: Some(limit_price),
            order: Some(order),
            capital_check: Some(capital_check_to_json(&capital)),
            bracket: Some(bracket_preview),
            fill: None,
            order_id: None,
        });
    }

    if runtime.simulate {
        let pos_id = position_id(&symbol, &rules.schedule.timezone);
        record_sim_entry(
            state,
            rules,
            account_hash,
            &symbol,
            quantity,
            limit_price,
            &pos_id,
        )?;
        state.trades_today += 1;
        save_state(rules_path, state)?;

        journal::append_event(
            rules_path,
            "sim_entry_filled",
            json!({
                "source": source,
                "symbol": symbol,
                "quantity": quantity,
                "fill_price": limit_price,
                "capital_check": capital_check_to_json(&capital),
                "bracket_preview": bracket_preview,
            }),
        )?;

        return Ok(EntryAttempt {
            status: EntryStatus::Simulated,
            symbol,
            reason: None,
            quantity: Some(quantity),
            limit_price: Some(limit_price),
            order: Some(order),
            capital_check: Some(capital_check_to_json(&capital)),
            bracket: Some(bracket_preview),
            fill: Some(json!({
                "simulated": true,
                "fill_price": limit_price,
                "quantity": quantity,
            })),
            order_id: Some(format!("sim-{pos_id}")),
        });
    }

    let schwab_rt = runtime.as_schwab_runtime();
    require_trading_approval(
        &schwab_rt,
        "trader entry",
        &format!("Buy {quantity} {symbol} @ {limit_price:.2}"),
    )?;

    state.pending_buys.push(PendingBuy {
        order_id: "pending".into(),
        symbol: symbol.clone(),
        estimated_cost_usd: estimated_cost,
        submitted_at: Utc::now(),
    });
    save_state(rules_path, state)?;

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
        .context("Missing order id after buy")?;

    if let Some(pending) = state.pending_buys.last_mut() {
        pending.order_id = order_id.clone();
    }

    let wait = wait_for_order(
        api,
        account_hash,
        &order_id,
        WaitOptions {
            condition: WaitCondition::Filled,
            timeout: Duration::from_secs(rules.execution.fill_timeout_seconds),
            interval: Duration::from_secs(2),
            proceed_on_partial_fill: false,
            requested_quantity: Some(quantity),
        },
    )
    .await?;

    if !wait.met {
        journal::append_event(
            rules_path,
            "entry_fill_timeout",
            json!({
                "symbol": symbol,
                "order_id": order_id,
                "final_status": wait.final_status,
            }),
        )?;
        return Ok(EntryAttempt {
            status: EntryStatus::Submitted,
            symbol,
            reason: Some("fill wait timeout — pending reconcile".into()),
            quantity: Some(quantity),
            limit_price: Some(limit_price),
            order: Some(order),
            capital_check: Some(capital_check_to_json(&capital)),
            bracket: None,
            fill: Some(serde_json::to_value(&wait)?),
            order_id: Some(order_id),
        });
    }

    state.pending_buys.retain(|p| p.order_id != order_id);

    let fill_price = wait
        .order
        .get("price")
        .and_then(|v| v.as_f64())
        .unwrap_or(limit_price);
    let filled_qty = match schwab_cli::order_status::order_filled_quantity(&wait.order) {
        Some(q) if q > 0.0 => q,
        _ => {
            journal::append_event(
                rules_path,
                "entry_no_fill_qty",
                json!({ "symbol": symbol, "order_id": order_id }),
            )?;
            return Ok(skipped(
                &symbol,
                "order marked filled but no filled quantity",
            ));
        }
    };

    let fill_started = std::time::Instant::now();
    let (profit_limit, stop_px, stop_limit_px) = exit_prices(fill_price, rules);
    let mut bracket_result = None;
    let mut oco_order_id = None;

    if bracket && rules.playbook.exit.use_oco_at_entry {
        match place_oco_bracket_with_retry(
            runtime,
            api,
            account_hash,
            &symbol,
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
                state.last_fill_to_bracket_seconds =
                    Some(fill_started.elapsed().as_secs());
                oco_order_id = bracket
                    .order
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                bracket_result = Some(bracket.order);
            }
            Err(err) => {
                state.unbracketed_positions.insert(
                    symbol.clone(),
                    UnbracketedPosition {
                        symbol: symbol.clone(),
                        account_hash: account_hash.to_string(),
                        quantity: filled_qty,
                        entry_price: fill_price,
                        fill_order_id: order_id.clone(),
                        detected_at: Utc::now(),
                        bracket_attempts: 1,
                    },
                );
                state.trading_halted_reason =
                    Some(format!("unbracketed position: {symbol}"));
                journal::append_event(
                    rules_path,
                    "bracket_failed",
                    json!({
                        "symbol": symbol,
                        "error": err.to_string(),
                        "quantity": filled_qty,
                    }),
                )?;
            }
        }
    }

    let pos_id = position_id(&symbol, &rules.schedule.timezone);
    state.open_positions.insert(
        pos_id.clone(),
        SwingPosition {
            position_id: pos_id,
            symbol: symbol.clone(),
            account_hash: account_hash.to_string(),
            quantity: filled_qty,
            entry_price: fill_price,
            opened_at: Utc::now(),
            stop_price: stop_px,
            profit_limit,
            stop_risk_usd: filled_qty * (fill_price - stop_px).max(0.0),
            market_value_usd: filled_qty * fill_price,
            oco_order_id,
            exit_plan_version: 1,
        },
    );
    state.trades_today += 1;
    save_state(rules_path, state)?;

    journal::append_event(
        rules_path,
        "entry_filled",
        json!({
            "source": source,
            "symbol": symbol,
            "quantity": filled_qty,
            "fill_price": fill_price,
            "capital_check": capital_check_to_json(&capital),
            "bracket": bracket_result,
        }),
    )?;

    Ok(EntryAttempt {
        status: EntryStatus::Filled,
        symbol,
        reason: None,
        quantity: Some(filled_qty),
        limit_price: Some(fill_price),
        order: Some(order),
        capital_check: Some(capital_check_to_json(&capital)),
        bracket: bracket_result,
        fill: Some(serde_json::to_value(&wait)?),
        order_id: Some(order_id),
    })
}

fn skipped(symbol: &str, reason: impl Into<String>) -> EntryAttempt {
    EntryAttempt {
        status: EntryStatus::Skipped,
        symbol: symbol.to_string(),
        reason: Some(reason.into()),
        quantity: None,
        limit_price: None,
        order: None,
        capital_check: None,
        bracket: None,
        fill: None,
        order_id: None,
    }
}

fn resolve_entry_limit_price(snap: &TechnicalSnapshot, rules: &TraderRules) -> f64 {
    match rules.execution.entry_limit_basis.as_str() {
        "bid" => snap.bid.unwrap_or(snap.last),
        "mid" => match (snap.bid, snap.ask) {
            (Some(b), Some(a)) => (b + a) / 2.0,
            _ => snap.last,
        },
        "last" => snap.last,
        _ => snap.ask.unwrap_or(snap.last),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantity_respects_risk_and_budget() {
        let rules = TraderRules::default();
        let entry = 100.0;
        let stop = 96.0;
        let qty = compute_entry_quantity(&rules, entry, stop, 5000.0);
        // risk => 5 shares, max_position_pct 8% of $3k sleeve => 2 shares (binding)
        assert!((qty - 2.0).abs() < 0.01);
    }

    #[test]
    fn sizing_reports_max_position_pct_binding() {
        let mut rules = TraderRules::default();
        rules.capital.fixed_sleeve_cap_usd = 4000.0;
        rules.playbook.entry.position_size.max_position_pct = 15.0;
        let sizing = compute_position_sizing(&rules, 100.0, 96.0, 5000.0);
        assert_eq!(sizing.binding_constraint, "max_position_pct");
        assert!((sizing.position_size_usd - 600.0).abs() < 0.01);
        assert!((sizing.risk_pct_size_usd - 700.0).abs() < 0.01);
    }

    #[test]
    fn limit_price_mid_from_quote() {
        let rules = TraderRules::default();
        let snap = TechnicalSnapshot {
            symbol: "AAPL".into(),
            last: 100.0,
            bid: Some(99.0),
            ask: Some(101.0),
            spread_pct: Some(2.0),
            sma_9: None,
            sma_20: None,
            sma_50: None,
            rsi_14: Some(50.0),
            atr_14: None,
            volume_sma_20: Some(1_000_000.0),
            relative_volume: None,
            above_sma_9: None,
            above_sma_20: None,
            above_sma_50: None,
            intraday: false,
        };
        let mut rules_mid = rules;
        rules_mid.execution.entry_limit_basis = "mid".into();
        assert!((resolve_entry_limit_price(&snap, &rules_mid) - 100.0).abs() < 0.01);
    }
}
