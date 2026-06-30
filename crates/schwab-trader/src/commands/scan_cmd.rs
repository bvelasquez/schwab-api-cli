use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::agent::state::TraderState;
use crate::config::TraderRuntime;
use crate::market_ctx::MarketCtx;
use crate::rules::TraderRules;
use crate::technical::{
    fetch_technical_snapshot_with_benchmark, passes_entry_filters, technical_to_json,
    TechnicalSnapshot,
};

pub async fn run(runtime: &TraderRuntime, rules_path: &Path) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let market = runtime.build_market_api()?;
    let market = MarketCtx::for_rules(market, rules_path, &rules);
    let state = crate::agent::state::load_state(rules_path, &rules.trader_id)?;
    let data = run_scan_inner(&market, &rules, &state).await?;
    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok("trader scan", data)
            .with_inputs(json!({ "rules_file": rules_path })),
    );
    Ok(())
}

pub async fn run_scan_inner(
    market: &MarketCtx,
    rules: &TraderRules,
    state: &TraderState,
) -> Result<Value> {
    let mut symbols = rules.all_watchlist_symbols();
    for s in &state.dynamic_watchlist {
        let u = s.trim().to_uppercase();
        if !u.is_empty() && !symbols.contains(&u) {
            symbols.push(u);
        }
    }

    let mut candidates = Vec::new();
    let mut rejected = Vec::new();

    let bench_sym = rules.adaptation.regime.benchmark_symbol.trim().to_uppercase();
    let benchmark_candles = if bench_sym.is_empty() {
        Vec::new()
    } else {
        market
            .daily_candles_with_config(&bench_sym, "year", 1, "daily")
            .await
            .unwrap_or_default()
    };
    let bench_ref = if benchmark_candles.is_empty() {
        None
    } else {
        Some(benchmark_candles.as_slice())
    };

    for symbol in symbols {
        if rules.is_core_holding(&symbol) {
            rejected.push(json!({ "symbol": symbol, "reason": "core_holding" }));
            continue;
        }
        if rules.is_blocked_symbol(&symbol) {
            rejected.push(json!({ "symbol": symbol, "reason": "blocked_symbol" }));
            continue;
        }
        if state.has_open_symbol(&symbol) {
            rejected.push(json!({ "symbol": symbol, "reason": "already_open" }));
            continue;
        }
        let snap = match fetch_technical_snapshot_with_benchmark(
            market,
            rules,
            &symbol,
            bench_ref,
        )
        .await
        {
            Ok(s) => s,
            Err(err) => {
                rejected.push(json!({ "symbol": symbol, "reason": err.to_string() }));
                continue;
            }
        };
        if let Some(reason) =
            passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, rules)
        {
            rejected.push(json!({
                "symbol": symbol,
                "reason": reason,
                "technical_context": technical_to_json(&snap),
            }));
            continue;
        }
        let score = candidate_score(&snap);
        candidates.push(json!({
            "symbol": symbol,
            "score": score,
            "technical_context": technical_to_json(&snap),
        }));
    }

    candidates.sort_by(|a, b| {
        let sa = a.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let sb = b.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(json!({
        "candidates": &candidates,
        "rejected": rejected,
        "candidate_count": candidates.len(),
        "active_profile": state.active_profile,
        "effective_playbook": crate::learn::adaptable_playbook_snapshot(rules),
        "market_cache": market.cache_status(),
    }))
}

/// Higher is better: RSI near midpoint, strong relative volume, tight spread.
pub fn candidate_score(snap: &TechnicalSnapshot) -> f64 {
    let rsi_score = snap
        .rsi_14
        .map(|r| 1.0 - (r - 50.0).abs() / 50.0)
        .unwrap_or(0.0)
        .max(0.0);
    let vol_score = snap
        .relative_volume
        .unwrap_or(1.0)
        .min(3.0)
        / 3.0;
    let spread_score = snap
        .spread_pct
        .map(|s| (1.0 - s / 2.0).max(0.0))
        .unwrap_or(0.0);
    let rs_score = snap
        .history_features
        .as_ref()
        .and_then(|h| h.rs_vs_benchmark_30d_pct)
        .map(|rs| ((rs / 15.0) + 0.5).clamp(0.0, 1.0))
        .unwrap_or(0.5);
    rsi_score * 0.35 + vol_score * 0.3 + spread_score * 0.2 + rs_score * 0.15
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_score_prefers_balanced_rsi() {
        let snap = TechnicalSnapshot {
            symbol: "AAPL".into(),
            last: 100.0,
            bid: Some(99.5),
            ask: Some(100.5),
            spread_pct: Some(1.0),
            sma_9: None,
            sma_20: None,
            sma_50: None,
            rsi_14: Some(50.0),
            atr_14: None,
            volume_sma_20: Some(1_000_000.0),
            relative_volume: Some(2.0),
            above_sma_9: None,
            above_sma_20: None,
            above_sma_50: None,
            intraday: false,
            history_features: None,
        };
        let balanced = candidate_score(&snap);
        let extreme = candidate_score(&TechnicalSnapshot {
            rsi_14: Some(20.0),
            ..snap
        });
        assert!(balanced > extreme);
    }
}
