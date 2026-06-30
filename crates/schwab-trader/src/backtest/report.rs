//! Aggregated backtest analysis from journal + ledger state.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use serde_json::{json, Value};

use crate::agent::paths::backtest_journal_path;
use crate::agent::state::load_backtest_state;
use crate::agent::state::TraderState;
use crate::backtest::cache::BacktestCache;
use crate::journal;
use crate::rules::TraderRules;
use crate::sim::{compute_stats, SimStats};

pub fn build_backtest_analysis_report(
    rules_path: &Path,
    rules: &TraderRules,
    cache: Option<&BacktestCache>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<Value> {
    let state = load_backtest_state(rules_path, &rules.trader_id)?;
    let events = journal::read_all_backtest(rules_path)?;
    let stats = compute_stats(&state);

    let mut event_counts: HashMap<String, u32> = HashMap::new();
    let mut per_symbol: HashMap<String, SymbolAgg> = HashMap::new();
    let mut exit_reasons: HashMap<String, u32> = HashMap::new();
    let mut skip_reasons: HashMap<String, u32> = HashMap::new();
    let mut regime_counts: HashMap<String, u32> = HashMap::new();
    let mut profile_counts: HashMap<String, u32> = HashMap::new();
    let mut adaptations = Vec::new();
    let mut monthly_pnl: HashMap<String, f64> = HashMap::new();
    let mut trading_days_observed = 0u32;
    let mut days_with_positions = 0u32;
    let mut open_position_days_sum = 0u64;
    let mut max_concurrent_positions = 0u32;

    for e in &events {
        let Some(event_type) = e.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        *event_counts.entry(event_type.to_string()).or_insert(0) += 1;
        let payload = e.get("payload").cloned().unwrap_or(json!({}));
        let ts = e.get("ts").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "sim_exit_filled" => {
                let symbol = payload_str(&payload, "symbol");
                let pnl = payload_f64(&payload, "pnl_usd");
                let reason = payload_str(&payload, "exit_reason");
                *exit_reasons.entry(reason.clone()).or_insert(0) += 1;
                let agg = per_symbol.entry(symbol).or_default();
                agg.trades += 1;
                agg.pnl_usd += pnl;
                if pnl >= 0.0 {
                    agg.wins += 1;
                }
                if let Some(month) = ts.get(0..7) {
                    *monthly_pnl.entry(month.to_string()).or_insert(0.0) += pnl;
                }
            }
            "backtest_day_summary" => {
                trading_days_observed += 1;
                let open = payload
                    .get("open_positions")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                open_position_days_sum += open as u64;
                max_concurrent_positions = max_concurrent_positions.max(open);
                if open > 0 {
                    days_with_positions += 1;
                }
                if let Some(regime) = payload.get("regime").and_then(|r| r.get("class")).and_then(|v| v.as_str())
                {
                    *regime_counts.entry(regime.to_string()).or_insert(0) += 1;
                }
                if let Some(profile) = payload.get("active_profile").and_then(|v| v.as_str()) {
                    *profile_counts.entry(profile.to_string()).or_insert(0) += 1;
                }
                if let Some(attempts) = payload.get("entry_attempts").and_then(|v| v.as_array()) {
                    for att in attempts {
                        if att.get("status").and_then(|v| v.as_str()) == Some("skipped") {
                            let reason = att
                                .pointer("/attempt/reason")
                                .and_then(|v| v.as_str())
                                .or_else(|| att.get("reason").and_then(|v| v.as_str()))
                                .unwrap_or("unknown");
                            let key = reason.split('(').next().unwrap_or(reason).trim().to_string();
                            *skip_reasons.entry(key).or_insert(0) += 1;
                        }
                    }
                }
            }
            "rule_auto_applied" | "rule_patch_proposed" | "backtest_rule_applied" => {
                adaptations.push(json!({ "ts": ts, "type": event_type, "payload": payload }));
            }
            _ => {}
        }
    }

    let closed_trades = state
        .sim
        .as_ref()
        .map(|l| serde_json::to_value(&l.closed_trades).unwrap_or(json!([])))
        .unwrap_or(json!([]));

    let equity_curve = state
        .sim
        .as_ref()
        .map(|l| serde_json::to_value(&l.equity_snapshots).unwrap_or(json!([])))
        .unwrap_or(json!([]));

    let per_symbol_json: Vec<Value> = per_symbol
        .into_iter()
        .map(|(symbol, agg)| {
            json!({
                "symbol": symbol,
                "trades": agg.trades,
                "wins": agg.wins,
                "win_rate_pct": if agg.trades > 0 { agg.wins as f64 / agg.trades as f64 * 100.0 } else { 0.0 },
                "pnl_usd": (agg.pnl_usd * 100.0).round() / 100.0,
            })
        })
        .collect();

    let benchmark = rules.adaptation.regime.benchmark_symbol.clone();
    let benchmark_comparison = cache
        .and_then(|c| from.zip(to).and_then(|(f, t)| compute_benchmark_roi(c, &benchmark, f, t).ok()));

    let time_in_market_pct = if trading_days_observed > 0 {
        (days_with_positions as f64 / trading_days_observed as f64) * 100.0
    } else {
        0.0
    };
    let avg_open_positions = if trading_days_observed > 0 {
        open_position_days_sum as f64 / trading_days_observed as f64
    } else {
        0.0
    };
    let unique_symbols_traded = per_symbol_json.len();

    let exposure_adjusted = build_exposure_adjusted_benchmark(
        &state,
        &rules,
        stats.as_ref(),
        benchmark_comparison.as_ref(),
        time_in_market_pct,
        avg_open_positions,
        cache,
        &benchmark,
        from,
        to,
    );

    Ok(json!({
        "generated_at": Utc::now().to_rfc3339(),
        "mode": "backtest",
        "rules_file": rules_path,
        "trader_id": rules.trader_id,
        "journal_path": backtest_journal_path(rules_path),
        "period": {
            "from": from.map(|d| d.to_string()),
            "to": to.map(|d| d.to_string()),
            "first_event": events.first().and_then(|e| e.get("ts")),
            "last_event": events.last().and_then(|e| e.get("ts")),
        },
        "ledger_stats": stats,
        "event_counts": event_counts,
        "closed_trades_ledger": closed_trades,
        "equity_curve": equity_curve,
        "per_symbol": per_symbol_json,
        "exit_reason_counts": exit_reasons,
        "entry_skip_reasons": skip_reasons,
        "regime_day_counts": regime_counts,
        "profile_day_counts": profile_counts,
        "monthly_closed_pnl_usd": monthly_pnl,
        "adaptations": adaptations,
        "benchmark_comparison": benchmark_comparison,
        "exposure_adjusted_benchmark": exposure_adjusted,
        "portfolio_exposure": {
            "watchlist_symbols": rules.all_watchlist_symbols().len(),
            "unique_symbols_traded": unique_symbols_traded,
            "max_positions_rule": rules.playbook.entry.max_positions,
            "max_new_entries_per_day": rules.playbook.entry.max_new_entries_per_day,
            "trading_days_observed": trading_days_observed,
            "days_with_open_positions": days_with_positions,
            "time_in_market_pct": (time_in_market_pct * 100.0).round() / 100.0,
            "avg_open_positions": (avg_open_positions * 100.0).round() / 100.0,
            "max_concurrent_positions": max_concurrent_positions,
            "note": "Multi-symbol portfolio; benchmark is 100% SPY buy-and-hold — compare risk-adjusted or time-in-market adjusted returns",
        },
        "active_profile": state.active_profile,
        "last_regime": state.last_regime,
        "tick_count": state.tick_count,
        "closed_trades_since_learn": state.closed_trades_since_learn,
    }))
}

#[derive(Default)]
struct SymbolAgg {
    trades: u32,
    wins: u32,
    pnl_usd: f64,
}

fn payload_str(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn payload_f64(payload: &Value, key: &str) -> f64 {
    payload.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

pub fn compute_benchmark_roi(
    cache: &BacktestCache,
    symbol: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Value> {
    let days = cache.trading_days(symbol, from, to)?;
    let sym = symbol.trim().to_uppercase();
    let bars = cache
        .symbols
        .get(&sym)
        .with_context(|| format!("benchmark {sym} missing from cache"))?;
    let start_day = days.first().context("no start trading day")?;
    let end_day = days.last().context("no end trading day")?;
    let start_bar = cache
        .bar_on_date(symbol, *start_day)
        .with_context(|| format!("no {symbol} bar on {start_day}"))?;
    let end_bar = cache
        .bar_on_date(symbol, *end_day)
        .with_context(|| format!("no {symbol} bar on {end_day}"))?;
    let start_px = start_bar.close;
    let end_px = end_bar.close;
    let roi_pct = if start_px > 0.0 {
        ((end_px / start_px) - 1.0) * 100.0
    } else {
        0.0
    };
    Ok(json!({
        "symbol": symbol,
        "from": start_day.to_string(),
        "to": end_day.to_string(),
        "start_close": start_px,
        "end_close": end_px,
        "buy_hold_roi_pct": (roi_pct * 100.0).round() / 100.0,
        "max_drawdown_pct": compute_benchmark_max_drawdown(bars, *start_day, *end_day),
        "trading_days": days.len(),
    }))
}

fn compute_benchmark_max_drawdown(
    bars: &[crate::backtest::cache::StoredCandle],
    from: NaiveDate,
    to: NaiveDate,
) -> f64 {
    let closes: Vec<f64> = bars
        .iter()
        .filter(|b| {
            let d = b.trading_date_et();
            d >= from && d <= to
        })
        .map(|b| b.close)
        .collect();
    if closes.is_empty() {
        return 0.0;
    }
    let mut peak = closes[0];
    let mut max_dd: f64 = 0.0;
    for c in closes {
        peak = peak.max(c);
        if peak > 0.0 {
            max_dd = max_dd.max((peak - c) / peak * 100.0);
        }
    }
    (max_dd * 100.0).round() / 100.0
}

fn sleeve_deploy_stats(state: &TraderState, rules: &TraderRules) -> (f64, f64) {
    let cap = rules.capital.fixed_sleeve_cap_usd.max(1.0);
    let snaps = state
        .sim
        .as_ref()
        .map(|s| s.equity_snapshots.as_slice())
        .unwrap_or(&[]);
    if snaps.is_empty() {
        return (0.0, 0.0);
    }
    let pcts: Vec<f64> = snaps
        .iter()
        .map(|s| (s.positions_value_usd / cap) * 100.0)
        .collect();
    let avg = pcts.iter().sum::<f64>() / pcts.len() as f64;
    let max = pcts.iter().copied().fold(0.0, f64::max);
    (avg, max)
}

fn build_exposure_adjusted_benchmark(
    state: &TraderState,
    rules: &TraderRules,
    stats: Option<&SimStats>,
    bench: Option<&Value>,
    time_in_market_pct: f64,
    avg_open_positions: f64,
    cache: Option<&BacktestCache>,
    bench_symbol: &str,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Value {
    let strategy_roi = stats.map(|s| s.roi_pct).unwrap_or(0.0);
    let strategy_max_dd = stats.map(|s| s.max_drawdown_pct).unwrap_or(0.0);
    let spy_roi = bench
        .and_then(|b| b.get("buy_hold_roi_pct"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let spy_max_dd = bench
        .and_then(|b| b.get("max_drawdown_pct"))
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| {
            cache.and_then(|c| {
                from.zip(to).and_then(|(f, t)| {
                    c.symbols.get(&bench_symbol.to_uppercase()).map(|bars| {
                        compute_benchmark_max_drawdown(bars, f, t)
                    })
                })
            })
            .unwrap_or(0.0)
        });

    let (avg_sleeve_deploy_pct, max_sleeve_deploy_pct) = sleeve_deploy_stats(state, rules);
    let max_pos = rules.playbook.entry.max_positions.max(1) as f64;
    let position_slot_util = (avg_open_positions / max_pos).clamp(0.0, 1.0);

    let spy_at_time_in_market = spy_roi * (time_in_market_pct / 100.0);
    let spy_at_position_slots = spy_roi * position_slot_util;
    let spy_at_sleeve_deploy = spy_roi * (avg_sleeve_deploy_pct / 100.0);

    let alpha_vs_100pct_spy = strategy_roi - spy_roi;
    let alpha_vs_time_in_market = strategy_roi - spy_at_time_in_market;
    let alpha_vs_position_slots = strategy_roi - spy_at_position_slots;
    let alpha_vs_sleeve_deploy = strategy_roi - spy_at_sleeve_deploy;

    let strategy_risk_ratio = if strategy_max_dd > 0.0 {
        strategy_roi / strategy_max_dd
    } else {
        0.0
    };
    let spy_risk_ratio = if spy_max_dd > 0.0 {
        spy_roi / spy_max_dd
    } else {
        0.0
    };

    json!({
        "strategy_roi_pct": round2(strategy_roi),
        "strategy_max_drawdown_pct": round2(strategy_max_dd),
        "benchmark_buy_hold_roi_pct": round2(spy_roi),
        "benchmark_max_drawdown_pct": round2(spy_max_dd),
        "avg_sleeve_deploy_pct": round2(avg_sleeve_deploy_pct),
        "max_sleeve_deploy_pct": round2(max_sleeve_deploy_pct),
        "time_in_market_pct": round2(time_in_market_pct),
        "avg_open_positions": round2(avg_open_positions),
        "position_slot_utilization_pct": round2(position_slot_util * 100.0),
        "benchmark_at_time_in_market_roi_pct": round2(spy_at_time_in_market),
        "benchmark_at_position_slots_roi_pct": round2(spy_at_position_slots),
        "benchmark_at_sleeve_deploy_roi_pct": round2(spy_at_sleeve_deploy),
        "alpha_vs_100pct_benchmark_pct": round2(alpha_vs_100pct_spy),
        "alpha_vs_time_in_market_pct": round2(alpha_vs_time_in_market),
        "alpha_vs_position_slots_pct": round2(alpha_vs_position_slots),
        "alpha_vs_sleeve_deploy_pct": round2(alpha_vs_sleeve_deploy),
        "return_over_max_dd_strategy": round2(strategy_risk_ratio),
        "return_over_max_dd_benchmark": round2(spy_risk_ratio),
        "interpretation": "alpha_vs_sleeve_deploy_pct is the fairest headline: compares strategy ROI to benchmark at the same average capital deployed",
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
