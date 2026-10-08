//! Tier C: a hypothetical trade for every symbol-day, admitted or rejected.
//! No sleeve and no slot cap. Exits use the same bar walker as the backtest.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Result;
use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::{json, Value};

use crate::agent::state::SwingPosition;
use crate::backtest::cache::BacktestCache;
use crate::backtest::exits::exit_reason_for_bar;
use crate::capital::{exit_geometry, ExitRangeContext};
use crate::history_features::compute_history_features;
use crate::rules::TraderRules;
use crate::technical::{build_technical_snapshot, passes_entry_filters, reason_code, Candle};

pub fn label_cache(
    rules: &TraderRules,
    cache: &BacktestCache,
    max_symbols: Option<usize>,
) -> Vec<Value> {
    let bench_sym = rules
        .adaptation
        .regime
        .benchmark_symbol
        .trim()
        .to_uppercase();
    let bench = cache.symbols.get(&bench_sym).cloned().unwrap_or_default();
    let regime_by_day = regime_by_day(rules, cache, &bench);
    let mut symbols: Vec<String> = cache
        .symbols
        .keys()
        .filter(|s| s.as_str() != bench_sym.as_str())
        .cloned()
        .collect();
    symbols.sort();
    if let Some(n) = max_symbols {
        symbols.truncate(n);
    }
    let mut rows = Vec::new();
    for symbol in symbols {
        let Some(bars) = cache.symbols.get(&symbol) else {
            continue;
        };
        if bars.len() < 60 {
            continue;
        }
        for i in 59..bars.len() {
            let day = bars[i].trading_date_et();
            let hist: Vec<Candle> = bars[..=i].iter().map(|b| b.to_candle()).collect();
            let last = bars[i].close;
            let mut snap = match build_technical_snapshot(
                &symbol,
                last,
                Some(last * 0.999),
                Some(last * 1.001),
                Some(0.2),
                &hist,
                false,
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let bench_hist: Vec<Candle> = bench
                .iter()
                .filter(|b| b.trading_date_et() <= day)
                .map(|b| b.to_candle())
                .collect();
            let bench_ref = if bench_hist.is_empty() {
                None
            } else {
                Some(bench_hist.as_slice())
            };
            snap.history_features = Some(compute_history_features(&hist, last, bench_ref));
            let reason =
                passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, rules);
            let code = reason.as_deref().map(reason_code);
            let trade = simulate_forward(rules, &bars[i..], last, &snap);
            let fwd = |n: usize| forward_excess(&bars[i..], bench_on(cache, &bench_sym, day), n);
            rows.push(json!({
                "symbol": symbol,
                "day": day.format("%Y-%m-%d").to_string(),
                "admitted": reason.is_none(),
                "reason_code": code,
                "rsi_14": snap.rsi_14,
                "above_sma_20": snap.above_sma_20,
                "above_sma_50": snap.above_sma_50,
                "above_sma_200": snap.history_features.as_ref().and_then(|h| h.above_sma_200),
                "rs_vs_benchmark_30d": snap.history_features.as_ref().and_then(|h| h.rs_vs_benchmark_30d_pct),
                "hypothetical": trade,
                "fwd_1d": fwd(1),
                "fwd_5d": fwd(5),
                "fwd_10d": fwd(10),
                "fwd_20d": fwd(20),
                "regime": regime_by_day.get(&day.format("%Y-%m-%d").to_string()).cloned().unwrap_or_else(|| "unknown".into()),
            }));
        }
    }
    rows
}

fn regime_by_day(
    rules: &TraderRules,
    cache: &BacktestCache,
    bench: &[crate::backtest::cache::StoredCandle],
) -> HashMap<String, String> {
    let cfg = &rules.adaptation.regime;
    let vix_sym = cfg.vix_symbol.trim().to_uppercase();
    let vix_bars = cache.symbols.get(&vix_sym);
    let closes: Vec<f64> = bench.iter().map(|b| b.close).collect();
    let mut out = HashMap::new();
    for (i, bar) in bench.iter().enumerate() {
        let window = &closes[..=i];
        let last = bar.close;
        let above_50 = crate::regime::sma(window, 50).is_some_and(|s| last >= s);
        let above_200 = crate::regime::sma(window, 200).is_some_and(|s| last >= s);
        let pct = crate::regime::realized_vol_percentile(
            window,
            cfg.realized_vol_lookback.max(2),
            cfg.realized_vol_history.max(2),
        );
        let day = bar.trading_date_et();
        let vix = vix_bars.and_then(|bars| {
            bars.iter()
                .rev()
                .find(|b| b.trading_date_et() <= day)
                .map(|b| b.close)
        });
        let class = crate::regime::classify_regime(cfg, vix, above_50, above_200, pct);
        out.insert(
            day.format("%Y-%m-%d").to_string(),
            class.as_str().to_string(),
        );
    }
    out
}

fn bench_on<'a>(
    cache: &'a BacktestCache,
    symbol: &str,
    day: NaiveDate,
) -> Vec<crate::backtest::cache::StoredCandle> {
    cache
        .symbols
        .get(symbol)
        .map(|b| {
            b.iter()
                .filter(|c| c.trading_date_et() >= day)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn forward_excess(
    bars: &[crate::backtest::cache::StoredCandle],
    bench: Vec<crate::backtest::cache::StoredCandle>,
    n: usize,
) -> Option<f64> {
    if bars.len() <= n || bench.len() <= n {
        return None;
    }
    let sym = (bars[n].close / bars[0].close - 1.0) * 100.0;
    let mkt = (bench[n].close / bench[0].close - 1.0) * 100.0;
    Some(sym - mkt)
}

fn simulate_forward(
    rules: &TraderRules,
    bars: &[crate::backtest::cache::StoredCandle],
    entry: f64,
    snap: &crate::technical::TechnicalSnapshot,
) -> Value {
    let range = ExitRangeContext::from_history(rules, snap.history_features.as_ref());
    let g = exit_geometry(entry, rules, snap.atr_14, range);
    let opened = Utc
        .timestamp_opt(bars[0].datetime_ms / 1000, 0)
        .single()
        .unwrap_or_else(Utc::now);
    let mut pos = SwingPosition {
        position_id: format!("{}|{}", snap.symbol, bars[0].trading_date_et()),
        symbol: snap.symbol.clone(),
        account_hash: "research".into(),
        quantity: 1.0,
        entry_price: entry,
        opened_at: opened,
        stop_price: g.stop_price,
        profit_limit: g.target_price,
        stop_risk_usd: (entry - g.stop_price).max(0.0),
        market_value_usd: entry,
        oco_order_id: None,
        exit_plan_version: 1,
        peak_profit_pct: Some(0.0),
        trough_profit_pct: Some(0.0),
        entry_rs_vs_benchmark_30d: None,
    };
    let max_hold = rules.playbook.exit.time_stop_days.max(1) as usize;
    let last_i = (bars.len() - 1).min(max_hold);
    for i in 1..=last_i {
        let now = Utc
            .timestamp_opt(bars[i].datetime_ms / 1000, 0)
            .single()
            .unwrap_or(opened);
        pos.opened_at = opened;
        if let Some(reason) = exit_reason_for_bar(rules, &pos, &bars[i], now) {
            let exit_px = crate::sim::sim_fill_price_for_rules(rules, reason, &pos, bars[i].close);
            let pnl_pct = (exit_px / entry - 1.0) * 100.0;
            let gap = if reason == "stop_loss" {
                Some((exit_px - pos.stop_price) / entry * 100.0)
            } else {
                None
            };
            return json!({
                "exit_reason": reason,
                "hold_days": i,
                "pnl_pct": pnl_pct,
                "stop_gap_slippage_pct": gap,
                "exit_day": bars[i].trading_date_et().format("%Y-%m-%d").to_string(),
            });
        }
    }
    let exit_px = bars[last_i].close;
    json!({
        "exit_reason": "time_stop",
        "hold_days": last_i,
        "pnl_pct": (exit_px / entry - 1.0) * 100.0,
        "stop_gap_slippage_pct": null,
        "exit_day": bars[last_i].trading_date_et().format("%Y-%m-%d").to_string(),
    })
}

pub fn write_jsonl(path: &Path, rows: &[Value]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut w = BufWriter::new(File::create(path)?);
    for row in rows {
        writeln!(w, "{row}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::cache::{BacktestCache, StoredCandle};
    use chrono::TimeZone;

    fn bar(day: i64, close: f64) -> StoredCandle {
        let dt = Utc.with_ymd_and_hms(2024, 1, 1, 14, 30, 0).unwrap() + chrono::Duration::days(day);
        StoredCandle {
            datetime_ms: dt.timestamp_millis(),
            open: close,
            high: close * 1.01,
            low: close * 0.99,
            close,
            volume: 2_000_000.0,
        }
    }

    #[test]
    fn labels_admitted_and_rejected_without_a_sleeve() {
        let mut rules = TraderRules::default();
        rules.version = 1;
        rules.trader_id = "t".into();
        rules.accounts = vec![crate::rules::TraderAccount {
            hash: "abc".into(),
            label: None,
            r#type: crate::rules::AccountType::Margin,
            enabled: true,
        }];
        rules.playbook.entry.require_above_sma = vec![20];
        rules.playbook.entry.rsi_14_range = [1.0, 99.0];
        rules.playbook.exit.profit_target_pct = 50.0;
        rules.playbook.exit.stop_loss_pct = 50.0;
        rules.playbook.exit.time_stop_days = 5;
        rules.adaptation.regime.benchmark_symbol = "SPY".into();
        let mut cache = BacktestCache::new(
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2024, 6, 1).unwrap(),
        );
        let up: Vec<_> = (0..80).map(|i| bar(i, 100.0 + i as f64)).collect();
        let spy: Vec<_> = (0..80).map(|i| bar(i, 400.0 + i as f64 * 0.2)).collect();
        cache.symbols.insert("AAA".into(), up);
        cache.symbols.insert("SPY".into(), spy);
        let rows = label_cache(&rules, &cache, None);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.get("hypothetical").is_some()));
        assert!(rows
            .iter()
            .any(|r| r.get("fwd_5d").and_then(|v| v.as_f64()).is_some()));
    }
}
