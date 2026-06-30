use std::sync::Arc;

use anyhow::Result;
use schwab_market_data::MarketDataApi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::commands::scan_cmd::candidate_score;
use crate::market_ctx::MarketCtx;
use crate::rules::{TraderRules, WatchlistThematic};
use crate::technical::{
    fetch_technical_snapshot_with_benchmark, passes_entry_filters, technical_to_json,
};

const QUOTE_CHUNK: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteTarget {
    Thematic,
    Core,
    Both,
}

impl WriteTarget {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "thematic" => Ok(Self::Thematic),
            "core" => Ok(Self::Core),
            "both" => Ok(Self::Both),
            other => anyhow::bail!("unknown write target `{other}` (use thematic|core|both)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub top_n: Option<u32>,
    pub min_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualifiedSymbol {
    pub symbol: String,
    pub score: f64,
    pub technical_context: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildResult {
    pub pool_size: usize,
    pub screened_count: usize,
    pub cheap_rejected_count: usize,
    pub qualified: Vec<QualifiedSymbol>,
    pub rejected: Vec<Value>,
    pub proposed_thematic: Vec<WatchlistThematic>,
    pub proposed_core_append: Vec<String>,
}

pub async fn build_watchlist(
    market: &MarketCtx,
    rules: &TraderRules,
    rules_path: &std::path::Path,
    options: &BuildOptions,
) -> Result<BuildResult> {
    let pool = rules.symbols_for_screening(rules_path)?;
    let top_n = options
        .top_n
        .unwrap_or(rules.watchlists.screened.top_n)
        .max(1) as usize;
    let min_score = options
        .min_score
        .unwrap_or(rules.watchlists.screened.min_score);

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

    let (cheap_pass, cheap_rejected) = cheap_quote_filter(market, rules, &pool).await?;
    let mut qualified = Vec::new();
    let mut rejected = cheap_rejected;

    for symbol in cheap_pass {
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
        if score < min_score {
            rejected.push(json!({
                "symbol": symbol,
                "reason": format!("score {score:.3} below min {min_score:.3}"),
                "score": score,
                "technical_context": technical_to_json(&snap),
            }));
            continue;
        }
        qualified.push(QualifiedSymbol {
            symbol: symbol.clone(),
            score,
            technical_context: technical_to_json(&snap),
        });
    }

    qualified.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    qualified.truncate(top_n);

    let proposed_thematic: Vec<WatchlistThematic> = qualified
        .iter()
        .map(|q| WatchlistThematic {
            symbol: q.symbol.clone(),
            tags: vec!["screened".into()],
        })
        .collect();

    let core_set: std::collections::HashSet<String> = rules
        .watchlists
        .core
        .iter()
        .map(|s| s.trim().to_uppercase())
        .collect();
    let proposed_core_append: Vec<String> = qualified
        .iter()
        .map(|q| q.symbol.clone())
        .filter(|s| !core_set.contains(s))
        .collect();

    Ok(BuildResult {
        pool_size: pool.len(),
        screened_count: pool.len(),
        cheap_rejected_count: rejected.len(),
        qualified,
        rejected,
        proposed_thematic,
        proposed_core_append,
    })
}

async fn cheap_quote_filter(
    market: &MarketCtx,
    rules: &TraderRules,
    symbols: &[String],
) -> Result<(Vec<String>, Vec<Value>)> {
    let MarketCtx::Live { market: api, .. } = market else {
        return Ok((symbols.to_vec(), Vec::new()));
    };

    let entry = &rules.playbook.entry;
    let mut pass = Vec::new();
    let mut rejected = Vec::new();

    for chunk in symbols.chunks(QUOTE_CHUNK) {
        let joined = chunk.join(",");
        let raw = api
            .quotes()
            .get_quotes(&joined, Some("quote"), None)
            .await?;
        for symbol in chunk {
            match cheap_quote_check(&raw, symbol, entry.min_price_usd, entry.max_spread_pct) {
                Ok(()) => pass.push(symbol.clone()),
                Err(reason) => {
                    rejected.push(json!({ "symbol": symbol, "reason": reason, "phase": "quote" }));
                }
            }
        }
    }
    Ok((pass, rejected))
}

fn cheap_quote_check(
    raw: &Value,
    symbol: &str,
    min_price: f64,
    max_spread_pct: f64,
) -> Result<(), String> {
    let quote = extract_batch_quote(raw, symbol);
    let last = quote
        .get("lastPrice")
        .and_then(|v| v.as_f64())
        .filter(|p| *p > 0.0)
        .ok_or_else(|| "missing lastPrice".to_string())?;
    if last < min_price {
        return Err(format!("price {last:.2} below min {min_price:.2}"));
    }
    let bid = quote.get("bidPrice").and_then(|v| v.as_f64());
    let ask = quote.get("askPrice").and_then(|v| v.as_f64());
    if let (Some(b), Some(a)) = (bid, ask) {
        if last > 0.0 {
            let spread = ((a - b) / last) * 100.0;
            if spread > max_spread_pct {
                return Err(format!("spread {spread:.2}% too wide"));
            }
        }
    }
    Ok(())
}

fn extract_batch_quote(raw: &Value, symbol: &str) -> Value {
    if let Some(entry) = raw.get(symbol) {
        return entry.get("quote").cloned().unwrap_or(Value::Null);
    }
    if let Some(obj) = raw.as_object() {
        for (_key, entry) in obj {
            if let Some(sym) = entry
                .pointer("/reference/symbol")
                .or_else(|| entry.get("symbol"))
                .and_then(|v| v.as_str())
            {
                if sym.eq_ignore_ascii_case(symbol) {
                    return entry.get("quote").cloned().unwrap_or(Value::Null);
                }
            }
        }
    }
    raw.get("quote")
        .cloned()
        .unwrap_or_else(|| raw.clone())
}

pub async fn validate_pool_quotes(
    api: &Arc<MarketDataApi>,
    symbols: &[String],
) -> Result<Value> {
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for chunk in symbols.chunks(QUOTE_CHUNK) {
        let joined = chunk.join(",");
        let raw = api
            .quotes()
            .get_quotes(&joined, Some("quote"), None)
            .await?;
        for symbol in chunk {
            if cheap_quote_check(&raw, symbol, 0.0, f64::MAX).is_ok() {
                ok.push(symbol.clone());
            } else {
                failed.push(json!({ "symbol": symbol, "reason": "no quote" }));
            }
        }
    }
    Ok(json!({
        "total": symbols.len(),
        "ok_count": ok.len(),
        "failed_count": failed.len(),
        "ok": ok,
        "failed": failed,
    }))
}
