use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::adaptation::{apply_llm_profile_selection, apply_regime_profile, effective_rules};
use crate::agent::llm::OpenRouterClient;
use crate::agent::paths::{backtest_cache_path, backtest_state_path};
use crate::agent::state::{
    load_backtest_state, save_backtest_state, TraderState,
};
use crate::backtest::cache::BacktestCache;
use crate::backtest::exits::process_backtest_exits;
use crate::backtest::prefetch::symbols_for_prefetch;
use crate::capital::compute_capital_check;
use crate::commands::scan_cmd::run_scan_inner;
use crate::config::TraderRuntime;
use crate::entry::{attempt_entry, EntryStatus};
use crate::journal;
use crate::learn::{
    adaptation_allowed, apply_rule_patches, build_learn_context, should_run_learn,
};
use crate::market_ctx::MarketCtx;
use crate::regime::detect_regime;
use crate::risk::update_drawdown;
use crate::rules::TraderRules;
use crate::sim::{compute_stats, ensure_ledger, snapshot_equity};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryFillMode {
    /// Signal and fill at same-day close (default swing backtest).
    Close,
    /// Signal at close, fill at next session open.
    NextOpen,
}

impl EntryFillMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "close" => Ok(Self::Close),
            "next_open" | "open" => Ok(Self::NextOpen),
            other => anyhow::bail!("invalid fill_at {other:?} (use close or next_open)"),
        }
    }
}

pub struct BacktestRunOptions {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub fresh: bool,
    /// Run LLM learn loop and apply rule patches in-memory (does not write rules YAML).
    pub learn: bool,
    pub fill_at: EntryFillMode,
}

pub async fn run_backtest(
    runtime: &TraderRuntime,
    rules_path: &Path,
    options: BacktestRunOptions,
) -> Result<()> {
    anyhow::ensure!(
        !runtime.dry_run,
        "backtest run requires paper mode — do not pass --dry-run"
    );

    let mut rules = TraderRules::load(rules_path)?;
    rules.log_validation_hints();
    if rules.is_intraday() {
        tracing::warn!("backtest v1 is optimized for swing (daily bars); intraday rules may be inaccurate");
    }

    let cache_path = backtest_cache_path(rules_path);
    let cache = BacktestCache::load(&cache_path).with_context(|| {
        format!(
            "missing cache {} — run: schwab-trader backtest prefetch --rules-file {}",
            cache_path.display(),
            rules_path.display()
        )
    })?;

    let benchmark = rules.adaptation.regime.benchmark_symbol.clone();
    let trading_days = cache.trading_days(&benchmark, options.from, options.to)?;

    let account = rules.primary_account()?.hash.clone();
    let api = runtime.build_api()?;

    if options.fresh {
        journal::clear_backtest_journal(rules_path)?;
    }

    let mut state = if options.fresh {
        fresh_backtest_state(&rules)
    } else {
        load_backtest_state(rules_path, &rules.trader_id)?
    };

    let llm_client = if options.learn && rules.backtest_learn_enabled() {
        match OpenRouterClient::from_env() {
            Ok(c) => Some(c),
            Err(err) => {
                tracing::warn!("backtest learn disabled: {err}");
                None
            }
        }
    } else {
        None
    };

    let mut runtime = runtime.clone();
    runtime.simulate = true;

    let mut day_summaries = Vec::new();
    let mut learn_runs = 0u32;

    for (day_idx, day) in trading_days.iter().enumerate() {
        let as_of = market_close_utc(*day, &rules.schedule.timezone)?;
        state.tick_count += 1;
        state.regular_tick_count += 1;
        state.last_tick = Some(as_of);
        reset_trades_day_at(&mut state, &rules.schedule.timezone, *day);

        let ctx = MarketCtx::replay(Arc::new(cache.clone()), as_of);
        let profile_before = state.active_profile.clone();

        let regime = detect_regime(&ctx, &rules).await.unwrap_or_else(|err| {
            tracing::warn!("regime detection failed on {day}: {err}");
            crate::regime::RegimeSnapshot {
                class: "neutral".into(),
                benchmark_symbol: rules.adaptation.regime.benchmark_symbol.clone(),
                vix_symbol: rules.adaptation.regime.vix_symbol.clone(),
                benchmark_last: 0.0,
                vix: None,
                above_sma_50: false,
                above_sma_200: false,
                realized_vol_annualized_pct: 0.0,
                realized_vol_percentile: 50.0,
                recommended_profile: rules.adaptation.default_profile.clone(),
                signals: json!({}),
            }
        });
        state.last_regime = Some(regime.to_json());
        apply_regime_profile(&mut state, &rules, &regime);
        let tick_rules = effective_rules(&rules, &state);

        let drawdown = update_drawdown(&mut state, &tick_rules);
        let closure_exits =
            process_backtest_exits(rules_path, &tick_rules, &mut state, &ctx, &cache, *day, as_of)
                .await?;

        let scan = run_scan_inner(&ctx, &tick_rules, &state).await?;
        let capital = compute_capital_check(
            &api,
            &tick_rules,
            &state,
            &account,
            None,
            None,
            true,
            Some(rules_path),
        )
        .await?;

        let mut entry_attempts = Vec::new();
        if capital.passed && backtest_entry_block_reason(&state, &tick_rules).is_none() {
            if let Some(candidates) = scan.get("candidates").and_then(|v| v.as_array()) {
                for candidate in candidates {
                    let symbol = candidate
                        .get("symbol")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if symbol.is_empty() {
                        continue;
                    }

                    let (fill_price, fill_at) = resolve_backtest_fill(
                        &cache,
                        &trading_days,
                        day_idx,
                        &symbol,
                        options.fill_at,
                        as_of,
                    )?;
                    let Some(fill_at) = fill_at else {
                        entry_attempts.push(json!({
                            "status": "skipped",
                            "symbol": symbol,
                            "reason": "next_open fill unavailable (last trading day)",
                            "effective_playbook": crate::learn::adaptable_playbook_snapshot(&tick_rules),
                            "active_profile": state.active_profile,
                        }));
                        continue;
                    };

                    let attempt = attempt_entry(
                        &runtime,
                        rules_path,
                        &tick_rules,
                        &mut state,
                        &api,
                        &ctx,
                        &account,
                        symbol,
                        fill_price,
                        None,
                        true,
                        "backtest",
                        Some(fill_at),
                    )
                    .await?;
                    let status = match attempt.status {
                        EntryStatus::Simulated => "simulated",
                        EntryStatus::Skipped => "skipped",
                        _ => "other",
                    };
                    let done = matches!(attempt.status, EntryStatus::Simulated);
                    entry_attempts.push(json!({
                        "status": status,
                        "symbol": symbol,
                        "fill_at": options.fill_at,
                        "attempt": attempt,
                    }));
                    if done {
                        break;
                    }
                }
            }
        }

        snapshot_equity(&mut state, &tick_rules);

        let mut learn_result = None;
        if options.learn && should_run_learn(&rules, &state, true) {
            if let Some(client) = &llm_client {
                learn_result = Some(
                    run_backtest_learn(
                        &mut rules,
                        &mut state,
                        rules_path,
                        client,
                        as_of,
                    )
                    .await?,
                );
                learn_runs += 1;
            }
        }

        let summary = json!({
            "day": day.to_string(),
            "as_of": as_of.to_rfc3339(),
            "tick": state.tick_count,
            "regime": regime.to_json(),
            "active_profile": state.active_profile,
            "profile_changed": profile_before != state.active_profile,
            "effective_playbook": crate::learn::adaptable_playbook_snapshot(&tick_rules),
            "drawdown": drawdown,
            "closure_exits": closure_exits,
            "scan_candidates": scan.get("candidate_count"),
            "capital_passed": capital.passed,
            "entry_attempts": entry_attempts,
            "open_positions": state.open_positions.len(),
            "sim_stats": compute_stats(&state),
            "learn": learn_result,
            "fill_at": options.fill_at,
        });
        journal::append_backtest_event(
            rules_path,
            as_of,
            "backtest_day_summary",
            summary.clone(),
        )?;
        day_summaries.push(summary);
        save_backtest_state(rules_path, &state)?;
    }

    let benchmark_comparison = crate::backtest::compute_benchmark_roi(
        &cache,
        &benchmark,
        options.from,
        options.to,
    )
    .ok();

    let report = json!({
        "mode": "backtest",
        "from": options.from.to_string(),
        "to": options.to.to_string(),
        "trading_days": trading_days.len(),
        "symbols_cached": symbols_for_prefetch(&rules, Some(rules_path)).len(),
        "cache_path": backtest_cache_path(rules_path),
        "state_path": backtest_state_path(rules_path),
        "learn_enabled": options.learn,
        "learn_runs": learn_runs,
        "fill_at": options.fill_at,
        "fractional_shares": rules.fractional_shares_allowed(),
        "final_stats": compute_stats(&state),
        "final_summary": state.summary(),
        "benchmark_comparison": benchmark_comparison,
        "day_summaries": day_summaries,
    });

    runtime.emit(schwab_cli::output::ResponseEnvelope::ok(
        "backtest run complete",
        report,
    )
    .with_inputs(json!({
        "rules_file": rules_path,
        "from": options.from.to_string(),
        "to": options.to.to_string(),
        "fresh": options.fresh,
        "learn": options.learn,
        "fill_at": options.fill_at,
    })));

    Ok(())
}

async fn run_backtest_learn(
    rules: &mut TraderRules,
    state: &mut TraderState,
    rules_path: &Path,
    client: &OpenRouterClient,
    as_of: DateTime<Utc>,
) -> Result<serde_json::Value> {
    let learn_ctx = build_learn_context(rules, state, rules_path, true)?;
    match client
        .review(
            &rules.llm,
            "learn",
            &rules.llm.learn_model,
            &learn_ctx,
            false,
        )
        .await
    {
        Ok(review) => {
            let mut applied = Vec::new();
            if apply_llm_profile_selection(state, rules, &review) {
                tracing::info!(
                    "backtest learn selected profile {:?}",
                    state.active_profile
                );
            }
            if !review.rule_patches.is_empty()
                && adaptation_allowed(false, true, rules, true)
            {
                match apply_rule_patches(rules, &review.rule_patches) {
                    Ok(p) => {
                        applied = p;
                        state.closed_trades_since_learn = 0;
                    }
                    Err(err) => {
                        journal::append_backtest_event(
                            rules_path,
                            as_of,
                            "rule_patch_proposed",
                            json!({
                                "error": err.to_string(),
                                "patches": review.rule_patches,
                            }),
                        )?;
                        return Ok(json!({
                            "error": err.to_string(),
                            "patches_rejected": review.rule_patches,
                        }));
                    }
                }
            }
            let event_type = if applied.is_empty() {
                "rule_patch_proposed"
            } else {
                "backtest_rule_applied"
            };
            journal::append_backtest_event(
                rules_path,
                as_of,
                event_type,
                json!({
                    "patches": review.rule_patches,
                    "applied": applied,
                    "profile_selection": review.profile_name,
                }),
            )?;
            state.last_learn_tick = Some(state.tick_count);
            Ok(json!({
                "review": review,
                "applied": applied,
            }))
        }
        Err(err) => Ok(json!({ "error": err.to_string() })),
    }
}

fn resolve_backtest_fill(
    cache: &BacktestCache,
    trading_days: &[NaiveDate],
    day_idx: usize,
    symbol: &str,
    mode: EntryFillMode,
    signal_at: DateTime<Utc>,
) -> Result<(Option<f64>, Option<DateTime<Utc>>)> {
    match mode {
        EntryFillMode::Close => Ok((None, Some(signal_at))),
        EntryFillMode::NextOpen => {
            let next_day = trading_days.get(day_idx + 1).copied();
            let Some(next_day) = next_day else {
                return Ok((None, None));
            };
            let bar = cache
                .bar_on_date(symbol, next_day)
                .with_context(|| format!("no open bar for {symbol} on {next_day}"))?;
            let open_px = bar.open;
            anyhow::ensure!(open_px > 0.0, "invalid open price for {symbol} on {next_day}");
            let fill_at = market_open_utc(next_day, crate::market_session::US_EQUITY_TIMEZONE)?;
            Ok((Some(open_px), Some(fill_at)))
        }
    }
}

fn fresh_backtest_state(rules: &TraderRules) -> TraderState {
    let mut state = TraderState::default();
    state.trader_id = rules.trader_id.clone();
    ensure_ledger(&mut state, rules);
    state
}

fn backtest_entry_block_reason(
    state: &TraderState,
    rules: &TraderRules,
) -> Option<String> {
    state.entry_block_reason_replay(rules)
}

fn reset_trades_day_at(state: &mut TraderState, _tz_name: &str, day: NaiveDate) {
    if state.trades_day != Some(day) {
        state.trades_day = Some(day);
        state.trades_today = 0;
    }
}

pub fn market_close_utc(day: NaiveDate, tz_name: &str) -> Result<DateTime<Utc>> {
    let tz = crate::market_session::trading_tz(tz_name);
    let dt = tz
        .with_ymd_and_hms(day.year(), day.month(), day.day(), 15, 59, 0)
        .single()
        .context("invalid backtest day")?;
    Ok(dt.with_timezone(&Utc))
}

fn market_open_utc(day: NaiveDate, tz_name: &str) -> Result<DateTime<Utc>> {
    let tz = crate::market_session::trading_tz(tz_name);
    let dt = tz
        .with_ymd_and_hms(day.year(), day.month(), day.day(), 9, 30, 0)
        .single()
        .context("invalid backtest open day")?;
    Ok(dt.with_timezone(&Utc))
}
