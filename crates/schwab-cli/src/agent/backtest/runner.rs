//! Day-loop options backtest over synthetic BS marks.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use serde_json::{json, Value};

use crate::agent::exits::{evaluate_all_exits, SpreadMark};
use crate::agent::journal;
use crate::agent::paths::{backtest_cache_path, backtest_state_path};
use crate::agent::regime::{classify_options_regime, OptionsRegimeClass};
use crate::agent::roll::{
    roll_biased_dte_min, roll_biased_max_width, roll_biased_short_delta_max, roll_eligible,
    roll_money_ok, RollEligibility,
};
use crate::agent::sim::{compute_stats, ensure_ledger, ClosedSimTrade};
use crate::agent::spread_analytics::{compute_vertical_analytics, VerticalAnalyticsInput};
use crate::agent::state::{save_state, AgentState, TrackedPosition};
use crate::options::StrategyKind;
use crate::rules::{OptionsRegimeConfig, RulesConfig};

use super::bs::vertical_debit_to_close;
use super::cache::BacktestCache;
use super::synth::{
    pick_iron_condor, pick_vertical, vertical_passes_entry_gates, SynthCondor, SynthVertical,
};

pub struct BacktestRunOptions {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub fresh: bool,
}

#[derive(Debug, Clone)]
enum OpenKind {
    Vertical(SynthVertical),
    Condor(SynthCondor),
}

#[derive(Debug, Clone)]
struct OpenBt {
    id: String,
    kind: OpenKind,
    opened_on: NaiveDate,
    entry_credit: f64,
    max_loss_usd: f64,
    contracts: u32,
    peak_profit_pct: Option<f64>,
    rolls_used: u32,
    account_hash: String,
    opened_at: chrono::DateTime<Utc>,
}

pub fn run_backtest(
    rules_path: &Path,
    rules: &RulesConfig,
    options: BacktestRunOptions,
) -> Result<Value> {
    let cache_path = backtest_cache_path(rules_path);
    let cache = BacktestCache::load(&cache_path).with_context(|| {
        format!(
            "missing cache {} — run: schwab agent backtest prefetch --rules-file {}",
            cache_path.display(),
            rules_path.display()
        )
    })?;

    let state_path = backtest_state_path(rules_path);
    let mut state = if options.fresh {
        journal::clear_backtest_journal(rules_path)?;
        let mut s = AgentState {
            agent_id: rules.agent_id.clone(),
            ..Default::default()
        };
        ensure_ledger(&mut s, rules);
        s
    } else {
        let mut s = crate::agent::state::load_state(&state_path).unwrap_or_default();
        s.agent_id = rules.agent_id.clone();
        ensure_ledger(&mut s, rules);
        s
    };

    let bench = rules.regime.benchmark_symbol.trim().to_uppercase();
    let days = cache.trading_days(&bench, options.from, options.to)?;
    let account = rules
        .enabled_accounts()
        .next()
        .map(|a| a.hash.clone())
        .unwrap_or_else(|| "BACKTEST".into());

    let mut open: Vec<OpenBt> = Vec::new();
    if options.fresh {
        state.open_positions.clear();
        state.trades_today = 0;
        state.rolls_today = 0;
        if let Some(ledger) = state.sim.as_mut() {
            ledger.realized_pnl_usd = 0.0;
            ledger.closed_trades.clear();
        }
    }

    let mut skip_counts: HashMap<String, u32> = HashMap::new();
    let mut day_summaries = Vec::new();

    for day in &days {
        state.reset_daily_if_needed(*day);
        let as_of = day
            .and_hms_opt(21, 0, 0)
            .map(|t| t.and_utc())
            .unwrap_or_else(Utc::now);

        let mut still_open = Vec::new();
        let closes_bench = cache.closes_through(&bench, *day);
        let spot_bench = cache.close_on_date(&bench, *day).unwrap_or(0.0);
        let (above50, above200) = sma_flags(&closes_bench, spot_bench);
        let vix = cache.vix_on_date(&rules.regime.vix_symbol, *day);
        let class = if rules.regime.enabled {
            classify_options_regime(&rules.regime, vix, above50, above200)
        } else {
            OptionsRegimeClass::Neutral
        };
        let preferred = preferred_for(&rules.regime, class);
        let pause = class == OptionsRegimeClass::Hostile
            || vix.is_some_and(|v| v >= rules.regime.pause_entries_vix_above)
            || rules
                .regime
                .pause_entries_vix_below
                .is_some_and(|floor| vix.is_some_and(|v| v <= floor));
        let blocked = !rules.risk.active_blocked_date_labels(*day).is_empty();

        for pos in open.drain(..) {
            match process_open(
                rules_path,
                rules,
                &cache,
                &mut state,
                pos,
                *day,
                as_of,
                &mut skip_counts,
                Some(preferred.as_str()),
            )? {
                Some(p) => still_open.push(p),
                None => {}
            }
        }
        open = still_open;
        sync_open_to_state(&mut state, &open);

        let mut entry_action = Value::Null;
        if !pause && !blocked && preferred != "pause" && state.trading_halted_reason.is_none() {
            if let Some(detail) = try_entry(
                rules_path,
                rules,
                &cache,
                &mut state,
                &account,
                *day,
                as_of,
                &preferred,
                vix.unwrap_or(18.0),
                &mut open,
                &mut skip_counts,
            )? {
                entry_action = detail;
            }
        } else if pause {
            *skip_counts.entry("vix_or_regime_pause".into()).or_insert(0) += 1;
        } else if blocked {
            *skip_counts.entry("blocked_dates".into()).or_insert(0) += 1;
        }

        sync_open_to_state(&mut state, &open);
        let summary = json!({
            "day": day.to_string(),
            "regime": class.as_str(),
            "preferred": preferred,
            "vix": vix,
            "open_positions": open.len(),
            "entry": entry_action,
        });
        journal::append_backtest_event_at(
            rules_path,
            as_of,
            "backtest_day_summary",
            summary.clone(),
        )?;
        day_summaries.push(summary);
    }

    save_state(&state_path, &state)?;
    let stats = compute_stats(&state, rules);
    Ok(json!({
        "agent_id": rules.agent_id,
        "from": options.from.to_string(),
        "to": options.to.to_string(),
        "trading_days": days.len(),
        "pricing_model": "black_scholes_vix_iv",
        "caveat": "Synthetic BS marks using VIX as IV proxy — not OPRA historical fills",
        "final_stats": stats,
        "skip_reason_counts": skip_counts,
        "open_positions": open.len(),
        "day_summaries_tail": day_summaries.into_iter().rev().take(5).collect::<Vec<_>>(),
    }))
}

fn preferred_for(cfg: &OptionsRegimeConfig, class: OptionsRegimeClass) -> String {
    cfg.strategy_map
        .get(class.as_str())
        .cloned()
        .unwrap_or_else(|| match class {
            OptionsRegimeClass::BearishTrend => "call_credit".into(),
            OptionsRegimeClass::HighVolChop => "iron_condor".into(),
            OptionsRegimeClass::Hostile => "pause".into(),
            _ => "put_credit".into(),
        })
}

fn sma_flags(closes: &[f64], last: f64) -> (bool, bool) {
    let sma = |n: usize| -> Option<f64> {
        if closes.len() < n {
            return None;
        }
        let slice = &closes[closes.len() - n..];
        Some(slice.iter().sum::<f64>() / n as f64)
    };
    (
        sma(50).map(|s| last >= s).unwrap_or(true),
        sma(200).map(|s| last >= s).unwrap_or(true),
    )
}

fn sync_open_to_state(state: &mut AgentState, open: &[OpenBt]) {
    let ids: std::collections::HashSet<_> = open.iter().map(|o| o.id.clone()).collect();
    state.open_positions.retain(|k, _| ids.contains(k));
    for o in open {
        if let Some(p) = state.open_positions.get_mut(&o.id) {
            p.peak_profit_pct = o.peak_profit_pct;
            p.rolls_used = o.rolls_used;
        }
    }
}

fn process_open(
    rules_path: &Path,
    rules: &RulesConfig,
    cache: &BacktestCache,
    state: &mut AgentState,
    mut pos: OpenBt,
    day: NaiveDate,
    as_of: chrono::DateTime<Utc>,
    skips: &mut HashMap<String, u32>,
    preferred_strategy: Option<&str>,
) -> Result<Option<OpenBt>> {
    match &pos.kind {
        OpenKind::Vertical(v) => {
            let Some(spot) = cache.close_on_date(&v.underlying, day) else {
                return Ok(Some(pos));
            };
            let iv = cache
                .vix_on_date(&rules.regime.vix_symbol, day)
                .unwrap_or(v.iv_pct);
            let dte = (v.expiry - day).num_days().max(0);
            let debit = vertical_debit_to_close(
                v.is_put,
                spot,
                v.short_strike,
                v.long_strike,
                dte,
                iv,
            );
            let profit_pct = if pos.entry_credit > f64::EPSILON {
                ((pos.entry_credit - debit) / pos.entry_credit) * 100.0
            } else {
                0.0
            };
            let peak = pos.peak_profit_pct.unwrap_or(profit_pct).max(profit_pct);
            pos.peak_profit_pct = Some(peak);

            let analytics = compute_vertical_analytics(VerticalAnalyticsInput {
                is_put_spread: v.is_put,
                underlying_price: spot,
                short_strike: v.short_strike,
                long_strike: v.long_strike,
                credit: pos.entry_credit,
                dte,
                chain_iv_pct: Some(iv),
                realized_vol_pct: Some(v.realized_vol_pct).filter(|x| *x > 0.0),
                short_delta: Some(
                    crate::agent::backtest::bs::bs_delta(
                        v.is_put,
                        spot,
                        v.short_strike,
                        crate::agent::backtest::bs::years_from_dte(dte),
                        0.0,
                        (iv / 100.0).max(0.01),
                    ),
                ),
                long_delta: None,
                short_theta: None,
                long_theta: None,
                contracts: pos.contracts,
                underlying_change_pct: None,
            });
            let mark = SpreadMark {
                entry_credit: pos.entry_credit,
                debit_to_close: debit,
                profit_pct,
                dte,
                source: "bs".into(),
            };
            let open_strategy = if v.is_put { "put_credit" } else { "call_credit" };
            let exit = evaluate_all_exits(
                rules,
                Some(pos.entry_credit),
                &mark,
                Some(&analytics),
                pos.peak_profit_pct,
                Some(pos.opened_at),
                preferred_strategy,
                open_strategy,
            );

            if let Some(eval) = exit {
                if eval.reason == "stop_loss" && rules.exit_rules.roll.enabled {
                    if let Some(rolled) = try_roll(
                        rules_path,
                        rules,
                        cache,
                        state,
                        &pos,
                        v,
                        &eval,
                        analytics.short_otm_pct,
                        day,
                        as_of,
                        spot,
                        iv,
                        skips,
                    )? {
                        return Ok(Some(rolled));
                    }
                }
                close_position(rules_path, state, &pos, &eval.reason, debit, as_of, day)?;
                return Ok(None);
            }
            Ok(Some(pos))
        }
        OpenKind::Condor(c) => {
            let Some(spot) = cache.close_on_date(&c.underlying, day) else {
                return Ok(Some(pos));
            };
            let iv = cache
                .vix_on_date(&rules.regime.vix_symbol, day)
                .unwrap_or(c.iv_pct);
            let dte = (c.expiry - day).num_days().max(0);
            let put_debit = vertical_debit_to_close(true, spot, c.put_short, c.put_long, dte, iv);
            let call_debit =
                vertical_debit_to_close(false, spot, c.call_short, c.call_long, dte, iv);
            let debit = put_debit + call_debit;
            let profit_pct = if pos.entry_credit > f64::EPSILON {
                ((pos.entry_credit - debit) / pos.entry_credit) * 100.0
            } else {
                0.0
            };
            let peak = pos.peak_profit_pct.unwrap_or(profit_pct).max(profit_pct);
            pos.peak_profit_pct = Some(peak);
            let mark = SpreadMark {
                entry_credit: pos.entry_credit,
                debit_to_close: debit,
                profit_pct,
                dte,
                source: "bs".into(),
            };
            // Condors: mechanical + regime-mismatch (no greek thesis analytics).
            let exit = evaluate_all_exits(
                rules,
                Some(pos.entry_credit),
                &mark,
                None,
                None,
                None,
                preferred_strategy,
                "iron_condor",
            );
            if let Some(eval) = exit {
                close_position(rules_path, state, &pos, &eval.reason, debit, as_of, day)?;
                return Ok(None);
            }
            Ok(Some(pos))
        }
    }
}

fn try_roll(
    rules_path: &Path,
    rules: &RulesConfig,
    cache: &BacktestCache,
    state: &mut AgentState,
    pos: &OpenBt,
    v: &SynthVertical,
    eval: &crate::agent::exits::ExitEvaluation,
    short_otm_pct: Option<f64>,
    day: NaiveDate,
    as_of: chrono::DateTime<Utc>,
    spot: f64,
    iv: f64,
    skips: &mut HashMap<String, u32>,
) -> Result<Option<OpenBt>> {
    let tracked = state.open_positions.get(&pos.id).cloned().unwrap_or(TrackedPosition {
        position_id: pos.id.clone(),
        account_hash: pos.account_hash.clone(),
        underlying: v.underlying.clone(),
        expiry: v.expiry.to_string(),
        strategy: "vertical".into(),
        opened_at: pos.opened_at,
        entry_credit: Some(pos.entry_credit),
        max_loss_usd: pos.max_loss_usd,
        contracts: pos.contracts,
        entry_params: Some(json!({
            "spread_type": if v.is_put { "put_credit" } else { "call_credit" },
            "short_strike": v.short_strike,
            "long_strike": v.long_strike,
        })),
        entry_short_delta: Some(v.short_delta.abs()),
        rolls_used: pos.rolls_used,
        ..Default::default()
    });

    if let Err(reason) = roll_eligible(
        &rules.exit_rules.roll,
        &RollEligibility {
            tracked: &tracked,
            mark_dte: eval.mark.dte,
            short_otm_pct,
            rolls_today: state.rolls_today,
            reserved_risk_usd: state.reserved_risk_usd(),
            max_portfolio_risk_usd: rules.risk.max_portfolio_risk_usd,
        },
    ) {
        *skips.entry(reason.as_str().into()).or_insert(0) += 1;
        return Ok(None);
    }

    let roll = &rules.exit_rules.roll;
    let width = (v.short_strike - v.long_strike).abs();
    let mut entry = rules.entry_rules.vertical.clone();
    entry.dte_min = roll_biased_dte_min(entry.dte_min, eval.mark.dte, roll.min_dte_extension);
    if entry.dte_min > entry.dte_max {
        entry.dte_max = entry.dte_min;
    }
    entry.short_delta_max = roll_biased_short_delta_max(
        entry.short_delta_max,
        roll.target_short_delta_max,
        Some(v.short_delta.abs()),
    );
    entry.short_delta_min = entry.short_delta_min.min(entry.short_delta_max * 0.5);
    entry.max_width = roll_biased_max_width(entry.max_width, Some(width));
    entry.max_contracts_per_trade = pos.contracts.max(1);

    let closes = cache.closes_through(&v.underlying, day);
    let Some(candidate) = pick_vertical(
        &v.underlying,
        day,
        spot,
        iv,
        &closes,
        &entry,
        v.is_put,
        pos.contracts,
    ) else {
        *skips.entry("no_roll_candidate".into()).or_insert(0) += 1;
        return Ok(None);
    };
    if !roll_money_ok(
        pos.entry_credit,
        eval.mark.debit_to_close,
        candidate.credit,
        roll.max_debit_pct_of_entry_credit,
    ) {
        *skips.entry("roll_debit_too_large".into()).or_insert(0) += 1;
        return Ok(None);
    }
    if vertical_passes_entry_gates(rules, &entry, &candidate, spot).is_err() {
        *skips.entry("roll_gates_fail".into()).or_insert(0) += 1;
        return Ok(None);
    }

    // Close old, open new — successful roll skips stop cooldown.
    close_position(
        rules_path,
        state,
        pos,
        "defensive_roll",
        eval.mark.debit_to_close,
        as_of,
        day,
    )?;
    state.rolls_today = state.rolls_today.saturating_add(1);
    let next_rolls = pos.rolls_used.saturating_add(1);
    let margin = vertical_margin(&candidate);
    let side = if candidate.is_put { "P" } else { "C" };
    let new_id = format!(
        "{}|{}|{}|vertical|{}|{}",
        pos.account_hash, candidate.underlying, candidate.expiry, side, candidate.short_strike
    );
    let opened = insert_vertical(
        state,
        &pos.account_hash,
        day,
        as_of,
        candidate.clone(),
        margin,
        &new_id,
        next_rolls,
        true,
    );
    journal::append_backtest_event_at(
        rules_path,
        as_of,
        "defensive_roll",
        json!({
            "closed_id": pos.id,
            "new_id": new_id,
            "close_debit": eval.mark.debit_to_close,
            "new_credit": candidate.credit,
            "net": candidate.credit - eval.mark.debit_to_close,
        }),
    )?;
    Ok(Some(opened))
}

fn close_position(
    rules_path: &Path,
    state: &mut AgentState,
    pos: &OpenBt,
    reason: &str,
    debit: f64,
    as_of: chrono::DateTime<Utc>,
    day: NaiveDate,
) -> Result<()> {
    let entry_credit = pos.entry_credit;
    let contracts = pos.contracts.max(1) as f64;
    let pnl_usd = (entry_credit - debit) * 100.0 * contracts;
    let pnl_pct = if entry_credit > f64::EPSILON {
        ((entry_credit - debit) / entry_credit) * 100.0
    } else {
        0.0
    };
    let hold_days = (day - pos.opened_on).num_days().max(0) as u32;
    let underlying = match &pos.kind {
        OpenKind::Vertical(v) => v.underlying.clone(),
        OpenKind::Condor(c) => c.underlying.clone(),
    };
    let expiry = match &pos.kind {
        OpenKind::Vertical(v) => v.expiry.to_string(),
        OpenKind::Condor(c) => c.expiry.to_string(),
    };
    let strategy = match &pos.kind {
        OpenKind::Vertical(_) => "vertical",
        OpenKind::Condor(_) => "iron_condor",
    };

    if let Some(ledger) = state.sim.as_mut() {
        ledger.realized_pnl_usd += pnl_usd;
        ledger.closed_trades.push(ClosedSimTrade {
            trade_id: format!("bt-{}-{}", pos.id, as_of.timestamp()),
            position_id: pos.id.clone(),
            underlying: underlying.clone(),
            expiry: expiry.clone(),
            strategy: strategy.into(),
            contracts: pos.contracts,
            entry_credit,
            exit_debit: debit,
            opened_at: pos.opened_at,
            closed_at: as_of,
            pnl_usd,
            pnl_pct,
            exit_reason: reason.to_string(),
            hold_days,
        });
    }
    state.open_positions.remove(&pos.id);
    journal::append_backtest_event_at(
        rules_path,
        as_of,
        "sim_exit_filled",
        json!({
            "position_id": pos.id,
            "underlying": underlying,
            "exit_reason": reason,
            "pnl_usd": pnl_usd,
            "pnl_pct": pnl_pct,
            "exit_debit": debit,
            "entry_credit": entry_credit,
        }),
    )?;
    Ok(())
}

fn try_entry(
    rules_path: &Path,
    rules: &RulesConfig,
    cache: &BacktestCache,
    state: &mut AgentState,
    account: &str,
    day: NaiveDate,
    as_of: chrono::DateTime<Utc>,
    preferred: &str,
    iv_pct: f64,
    open: &mut Vec<OpenBt>,
    skips: &mut HashMap<String, u32>,
) -> Result<Option<Value>> {
    if rules.risk.max_trades_per_day > 0 && state.trades_today >= rules.risk.max_trades_per_day {
        *skips.entry("max_trades_per_day".into()).or_insert(0) += 1;
        return Ok(None);
    }

    for item in rules.watchlist_items() {
        let sym = item.symbol.trim().to_uppercase();
        if !rules
            .risk
            .allowed_underlyings
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&sym))
        {
            continue;
        }
        if let Some(max) = rules.risk.max_open_for_underlying(&sym) {
            if state.count_open_for_underlying(account, &sym) >= max {
                *skips.entry("max_open_per_underlying".into()).or_insert(0) += 1;
                continue;
            }
        }
        if let Some(group) = rules.risk.correlation_group_for(&sym) {
            let count = group
                .symbols
                .iter()
                .map(|s| state.count_open_for_underlying(account, s))
                .sum::<u32>();
            if count >= group.max_open {
                *skips.entry("correlation_group".into()).or_insert(0) += 1;
                continue;
            }
        }

        let Some(spot) = cache.close_on_date(&sym, day) else {
            continue;
        };
        let closes = cache.closes_through(&sym, day);

        if preferred == "iron_condor" && rules.strategies.iron_condor.enabled {
            let open_c = state.count_open_for_strategy(account, StrategyKind::IronCondor);
            if open_c >= rules.entry_rules.iron_condor.max_open_positions {
                *skips.entry("max_open_condor".into()).or_insert(0) += 1;
                continue;
            }
            if let Some(c) = pick_iron_condor(&sym, day, spot, iv_pct, &closes, rules) {
                let margin = condor_margin(&c);
                if margin > rules.risk.max_risk_per_trade_usd
                    || state.reserved_risk_usd() + margin > rules.risk.max_portfolio_risk_usd
                {
                    *skips.entry("risk_cap".into()).or_insert(0) += 1;
                    continue;
                }
                let id = format!("{account}|{sym}|{}|condor|{}", c.expiry, c.put_short);
                if state.open_positions.contains_key(&id) {
                    continue;
                }
                let opened = insert_condor(state, account, day, as_of, c, margin, &id);
                journal::append_backtest_event_at(
                    rules_path,
                    as_of,
                    "sim_entry_filled",
                    json!({ "position_id": id, "strategy": "iron_condor" }),
                )?;
                open.push(opened);
                return Ok(Some(json!({"action":"entry","strategy":"iron_condor","id": id})));
            }
            *skips.entry("no_condor_candidate".into()).or_insert(0) += 1;
            continue;
        }

        if !rules.strategies.vertical.enabled {
            continue;
        }
        let open_v = state.count_open_for_strategy(account, StrategyKind::Vertical);
        if open_v >= rules.entry_rules.vertical.max_open_positions {
            *skips.entry("max_open_vertical".into()).or_insert(0) += 1;
            continue;
        }
        let is_put = preferred != "call_credit";
        let entry = &rules.entry_rules.vertical;
        let Some(v) = pick_vertical(
            &sym,
            day,
            spot,
            iv_pct,
            &closes,
            entry,
            is_put,
            entry.max_contracts_per_trade,
        ) else {
            *skips.entry("no_vertical_candidate".into()).or_insert(0) += 1;
            continue;
        };
        if let Err(reason) = vertical_passes_entry_gates(rules, entry, &v, spot) {
            *skips.entry(reason).or_insert(0) += 1;
            continue;
        }
        let margin = vertical_margin(&v);
        if margin > rules.risk.max_risk_per_trade_usd
            || state.reserved_risk_usd() + margin > rules.risk.max_portfolio_risk_usd
        {
            *skips.entry("risk_cap".into()).or_insert(0) += 1;
            continue;
        }
        let side = if is_put { "P" } else { "C" };
        let id = format!(
            "{account}|{sym}|{}|vertical|{side}|{}",
            v.expiry, v.short_strike
        );
        if state.open_positions.contains_key(&id) {
            continue;
        }
        let opened = insert_vertical(state, account, day, as_of, v, margin, &id, 0, false);
        journal::append_backtest_event_at(
            rules_path,
            as_of,
            "sim_entry_filled",
            json!({ "position_id": id, "strategy": "vertical" }),
        )?;
        open.push(opened);
        return Ok(Some(json!({"action":"entry","strategy":"vertical","id": id})));
    }
    Ok(None)
}

fn vertical_margin(v: &SynthVertical) -> f64 {
    let width = (v.short_strike - v.long_strike).abs();
    ((width - v.credit).max(0.0) * 100.0) * v.contracts.max(1) as f64
}

fn condor_margin(c: &SynthCondor) -> f64 {
    let put_w = (c.put_short - c.put_long).abs();
    let call_w = (c.call_long - c.call_short).abs();
    let width = put_w.max(call_w);
    ((width - c.credit).max(0.0) * 100.0) * c.contracts.max(1) as f64
}

fn insert_vertical(
    state: &mut AgentState,
    account: &str,
    day: NaiveDate,
    as_of: chrono::DateTime<Utc>,
    v: SynthVertical,
    margin: f64,
    id: &str,
    rolls_used: u32,
    roll_replacement: bool,
) -> OpenBt {
    if !roll_replacement {
        state.trades_today = state.trades_today.saturating_add(1);
    }
    state.open_positions.insert(
        id.to_string(),
        TrackedPosition {
            position_id: id.to_string(),
            account_hash: account.to_string(),
            underlying: v.underlying.clone(),
            expiry: v.expiry.to_string(),
            strategy: "vertical".into(),
            opened_at: as_of,
            entry_credit: Some(v.credit),
            max_loss_usd: margin,
            contracts: v.contracts,
            entry_params: Some(json!({
                "underlying": v.underlying,
                "expiry": v.expiry.to_string(),
                "spread_type": if v.is_put { "put_credit" } else { "call_credit" },
                "short_strike": v.short_strike,
                "long_strike": v.long_strike,
                "contracts": v.contracts,
                "limit_credit": v.credit,
            })),
            entry_short_delta: Some(v.short_delta.abs()),
            rolls_used,
            last_roll_at: if rolls_used > 0 { Some(as_of) } else { None },
            ..Default::default()
        },
    );
    OpenBt {
        id: id.to_string(),
        kind: OpenKind::Vertical(v.clone()),
        opened_on: day,
        entry_credit: v.credit,
        max_loss_usd: margin,
        contracts: v.contracts,
        peak_profit_pct: None,
        rolls_used,
        account_hash: account.to_string(),
        opened_at: as_of,
    }
}

fn insert_condor(
    state: &mut AgentState,
    account: &str,
    day: NaiveDate,
    as_of: chrono::DateTime<Utc>,
    c: SynthCondor,
    margin: f64,
    id: &str,
) -> OpenBt {
    state.trades_today = state.trades_today.saturating_add(1);
    state.open_positions.insert(
        id.to_string(),
        TrackedPosition {
            position_id: id.to_string(),
            account_hash: account.to_string(),
            underlying: c.underlying.clone(),
            expiry: c.expiry.to_string(),
            strategy: "iron_condor".into(),
            opened_at: as_of,
            entry_credit: Some(c.credit),
            max_loss_usd: margin,
            contracts: c.contracts,
            entry_params: Some(json!({
                "underlying": c.underlying,
                "expiry": c.expiry.to_string(),
                "put_short": c.put_short,
                "put_long": c.put_long,
                "call_short": c.call_short,
                "call_long": c.call_long,
                "contracts": c.contracts,
                "limit_credit": c.credit,
            })),
            ..Default::default()
        },
    );
    OpenBt {
        id: id.to_string(),
        kind: OpenKind::Condor(c.clone()),
        opened_on: day,
        entry_credit: c.credit,
        max_loss_usd: margin,
        contracts: c.contracts,
        peak_profit_pct: None,
        rolls_used: 0,
        account_hash: account.to_string(),
        opened_at: as_of,
    }
}
