//! One shadow-arm evaluation of a regular-session tick.
//!
//! Arms only ever see a `MarketCtx::Tape` (no API client) and a paper
//! `TraderState`; nothing here takes a `TraderApi`, `TraderRuntime`, or
//! notifier, so no order, Telegram, or audio path is reachable. The LLM is
//! never called from this path. An arm may read a journaled decision
//! (`llm_signal`) that a separate batch wrote earlier the same day.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use crate::adaptation::{apply_regime_profile, effective_rules};
use crate::agent::runner::{maybe_clear_stale_redeploy, prioritise_scan_for_redeploy};
use crate::agent::state::{position_id_for_date, TraderState};
use crate::capital::compute_sim_capital_check;
use crate::commands::scan_cmd::run_scan_inner;
use crate::entry::{
    entry_gate_reason, plan_entry, quantity_below_min_reason, resolve_entry_limit_price,
    SimEntryFill,
};
use crate::journal::append_event_to_path;
use crate::market_ctx::{MarketCtx, TickTape};
use crate::market_session::trading_day;
use crate::regime::{detect_regime, neutral_snapshot};
use crate::risk::{compute_sleeve_equity, update_drawdown};
use crate::rules::TraderRules;
use crate::shuffle::entry_shuffle_block_from_scan;
use crate::sim::{ensure_ledger, process_sim_exits, record_sim_entry_at, LedgerSink};
use crate::technical::{fetch_technical_snapshot, reason_code};

use super::arm::{shadow_warn, ShadowArm, ShadowArmState, ShadowArms, ShadowDay};

/// Journals only fills (renamed `shadow_*`); plan-tighten / trailing events
/// are dropped to keep arm journals small. State is saved by the arm loop.
struct ShadowSink<'a> {
    journal: &'a Path,
}

impl LedgerSink for ShadowSink<'_> {
    fn event(&self, event_type: &str, payload: Value) -> Result<()> {
        if event_type == "sim_exit_filled" {
            append_event_to_path(self.journal, Utc::now(), "shadow_exit_filled", payload)?;
        }
        Ok(())
    }

    fn persist(&self, _state: &TraderState) -> Result<()> {
        Ok(())
    }
}

impl ShadowArms {
    /// Evaluate every arm against the data production fetched this tick.
    /// Per-arm failures are logged and never propagate.
    pub async fn run_tick(
        &mut self,
        rules_path: &Path,
        production: &TraderState,
        tape: Arc<TickTape>,
    ) {
        let market = MarketCtx::from_tape(tape);
        let prod_equity = compute_sleeve_equity(production);
        for arm in &mut self.arms {
            if let Err(err) = run_arm_tick(arm, rules_path, &market, production, prod_equity).await
            {
                shadow_warn(rules_path, &arm.id, &format!("tick failed: {err:#}"));
            }
            if let Err(err) = arm.state.save(&arm.state_path) {
                shadow_warn(rules_path, &arm.id, &format!("save state failed: {err:#}"));
            }
        }
    }
}

async fn run_arm_tick(
    arm: &mut ShadowArm,
    rules_path: &Path,
    market: &MarketCtx,
    production: &TraderState,
    prod_equity: f64,
) -> Result<()> {
    let rules = &arm.rules;
    let journal = arm.journal_path.as_path();
    let now = Utc::now();
    ensure_ledger(&mut arm.state.trader, rules);
    roll_day(&mut arm.state, rules, prod_equity, journal)?;

    let st = &mut arm.state.trader;
    st.tick_count += 1;
    st.regular_tick_count += 1;
    st.last_tick = Some(now);
    st.reset_trades_day(&rules.schedule.timezone);
    maybe_clear_stale_redeploy(st);
    st.dynamic_watchlist = production.dynamic_watchlist.clone();
    update_drawdown(st, rules);

    let regime = detect_regime(market, rules).await.unwrap_or_else(|_| {
        neutral_snapshot(&rules.adaptation.regime, &rules.adaptation.default_profile)
    });
    apply_regime_profile(st, rules, &regime);
    let tick_rules = effective_rules(rules, st);

    let mut rejections: Vec<String> = Vec::new();
    let exits = process_sim_exits(&ShadowSink { journal }, &tick_rules, st, market).await?;
    let (exit_count, realized) = exits
        .iter()
        .filter(|e| e.get("exit_reason").is_some())
        .fold((0u32, 0.0f64), |(n, pnl), e| {
            (
                n + 1,
                pnl + e.get("pnl_usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            )
        });

    let mut scan = run_scan_inner(market, &tick_rules, st, None).await?;
    prioritise_scan_for_redeploy(&mut scan, st.redeploy_signal.as_ref());
    if let Some(rejected) = scan.get("rejected").and_then(|v| v.as_array()) {
        rejections.extend(
            rejected
                .iter()
                .filter_map(|r| r.get("reason_code").and_then(|v| v.as_str()))
                .map(str::to_string),
        );
    }

    let mut entered = 0u32;
    let mut capital = compute_sim_capital_check(&tick_rules, st, None, None, Some(rules_path));
    crate::capital::relax_unconstrained_budget(&tick_rules, &mut capital);
    let blocked = if tick_rules.capital.unconstrained {
        st.entry_block_reason_unconstrained(&tick_rules)
    } else {
        st.entry_block_reason(&tick_rules)
    };
    if capital.passed && blocked.is_none() {
        let symbols: Vec<String> = scan
            .get("candidates")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get("symbol").and_then(|v| v.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let account_hash = rules.primary_account()?.hash.clone();
        for symbol in symbols {
            if let Some(reason) =
                entry_shuffle_block_from_scan(&tick_rules, st, &symbol, &scan, None)
            {
                rejections.push(reason_code(&reason).to_string());
                continue;
            }
            match shadow_entry(
                &tick_rules,
                st,
                market,
                &account_hash,
                &symbol,
                rules_path,
                journal,
            )
            .await?
            {
                Ok(()) => {
                    entered += 1;
                    break;
                }
                Err(reason) => rejections.push(reason_code(&reason).to_string()),
            }
        }
    }

    let arm_equity = compute_sleeve_equity(st);
    if let Some(day) = arm.state.day.as_mut() {
        day.entries += entered;
        day.exits += exit_count;
        day.realized_pnl_usd += realized;
        day.arm_last_equity_usd = arm_equity;
        day.prod_last_equity_usd = prod_equity;
        for code in rejections {
            *day.rejections.entry(code).or_insert(0) += 1;
        }
    }
    Ok(())
}

/// Flush the previous day's `shadow_day_summary` when the trading day changes.
/// A new day starts from the previous day's closing equities so daily changes
/// (including overnight gaps) telescope to the total.
fn roll_day(
    state: &mut ShadowArmState,
    rules: &TraderRules,
    prod_equity: f64,
    journal: &Path,
) -> Result<()> {
    let today = trading_day(&rules.schedule.timezone);
    let (arm_start, prod_start) = match &state.day {
        Some(d) if d.date == today => return Ok(()),
        Some(d) => {
            append_event_to_path(
                journal,
                Utc::now(),
                "shadow_day_summary",
                d.summary_payload(
                    state.trader.open_positions.len(),
                    state.trader.active_profile.as_deref(),
                ),
            )?;
            (d.arm_last_equity_usd, d.prod_last_equity_usd)
        }
        None => (compute_sleeve_equity(&state.trader), prod_equity),
    };
    state.started_at.get_or_insert_with(Utc::now);
    state.day = Some(ShadowDay::new(today, arm_start, prod_start));
    Ok(())
}

/// Paper entry through the same gates, sizing, capital check, and fill price
/// as `attempt_entry`'s simulate path. `Ok(Err(reason))` is a skip.
async fn shadow_entry(
    rules: &TraderRules,
    state: &mut TraderState,
    market: &MarketCtx,
    account_hash: &str,
    symbol: &str,
    rules_path: &Path,
    journal: &Path,
) -> Result<std::result::Result<(), String>> {
    let symbol = symbol.trim().to_uppercase();
    if let Some(reason) = entry_gate_reason(rules, state, &symbol, None, false) {
        return Ok(Err(reason));
    }
    let snap = match fetch_technical_snapshot(market, rules, &symbol).await {
        Ok(s) => s,
        Err(err) => return Ok(Err(err.to_string())),
    };
    let limit_price = resolve_entry_limit_price(&snap, rules);
    if limit_price <= 0.0 {
        return Ok(Err("could not resolve limit price".into()));
    }

    let mut preview = compute_sim_capital_check(rules, state, None, None, Some(rules_path));
    crate::capital::relax_unconstrained_budget(rules, &mut preview);
    let plan = plan_entry(rules, &snap, limit_price, preview.tradable_budget_usd);
    let (adjust, adjust_code) = crate::agent::llm_signal::adjust_entry(rules, &symbol);
    if matches!(adjust, crate::agent::llm_signal::EntryAdjust::Skip) {
        return Ok(Err(adjust_code.unwrap_or_else(|| "llm_signal_skip".into())));
    }
    let quantity = crate::agent::llm_signal::size_from_adjust(plan.quantity, adjust);
    if let Some(reason) =
        quantity_below_min_reason(rules, quantity, preview.tradable_budget_usd, limit_price)
    {
        return Ok(Err(reason));
    }
    let estimated_cost = schwab_cli::portfolio::estimate_equity_buy_cost(
        quantity,
        "LIMIT",
        Some(limit_price),
        None,
    )?;
    let stop_risk = quantity * (limit_price - plan.stop_price).max(0.0);
    let mut capital = compute_sim_capital_check(
        rules,
        state,
        Some(estimated_cost),
        Some(stop_risk),
        Some(rules_path),
    );
    crate::capital::relax_unconstrained_budget(rules, &mut capital);
    if !capital.passed {
        return Ok(Err(capital
            .reject_reason
            .clone()
            .unwrap_or_else(|| "capital_check failed".into())));
    }

    let fill_at = Utc::now();
    let pos_id = position_id_for_date(
        &symbol,
        &rules.schedule.timezone,
        trading_day(&rules.schedule.timezone),
    );
    record_sim_entry_at(
        state,
        rules,
        account_hash,
        &symbol,
        quantity,
        limit_price,
        &pos_id,
        fill_at,
        snap.atr_14,
        plan.range,
    )?;
    state.trades_today += 1;
    let payload = SimEntryFill {
        source: "shadow",
        trade_id: &pos_id,
        symbol: &symbol,
        quantity,
        fill_price: limit_price,
        stop_price: plan.stop_price,
        profit_limit: plan.profit_limit,
        capital: &capital,
        sizing: &plan.sizing,
    }
    .payload(state);
    append_event_to_path(journal, fill_at, "shadow_entry_filled", payload)?;
    Ok(Ok(()))
}
