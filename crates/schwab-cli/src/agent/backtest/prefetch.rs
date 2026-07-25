use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate, Utc};
use schwab_market_data::MarketDataApi;
use tokio::time::sleep;

use crate::agent::paths::backtest_cache_path;
use crate::rules::RulesConfig;

use super::cache::BacktestCache;

pub fn symbols_for_prefetch(rules: &RulesConfig) -> Vec<String> {
    let mut out: Vec<String> = rules
        .watchlist_items()
        .into_iter()
        .map(|w| w.symbol.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    let bench = rules.regime.benchmark_symbol.trim().to_uppercase();
    if !bench.is_empty() && !out.contains(&bench) {
        out.push(bench);
    }
    let vix = rules.regime.vix_symbol.trim().to_uppercase();
    if !vix.is_empty() && !out.contains(&vix) {
        out.push(vix);
    }
    // Schwab sometimes needs bare VIX without $.
    if !out.iter().any(|s| s == "VIX") {
        out.push("VIX".into());
    }
    if !out.iter().any(|s| s == "$VIX") {
        out.push("$VIX".into());
    }
    out.sort();
    out.dedup();
    out
}

pub async fn prefetch_daily_bars(
    market: &Arc<MarketDataApi>,
    rules: &RulesConfig,
    rules_path: &Path,
    from: NaiveDate,
    to: NaiveDate,
    force: bool,
) -> Result<BacktestCache> {
    let cache_path = backtest_cache_path(rules_path);
    if cache_path.is_file() && !force {
        let existing = BacktestCache::load(&cache_path)?;
        if existing.from <= from && existing.to >= to {
            let needed = symbols_for_prefetch(rules);
            let has_all = needed.iter().all(|s| existing.symbols.contains_key(s));
            if has_all {
                return Ok(existing);
            }
        }
    }

    let symbols = symbols_for_prefetch(rules);
    let chunks = date_chunks(from, to);

    let mut cache = if cache_path.is_file() {
        BacktestCache::load(&cache_path).unwrap_or_else(|_| BacktestCache::new(from, to))
    } else {
        BacktestCache::new(from, to)
    };
    cache.from = cache.from.min(from);
    cache.to = cache.to.max(to);
    cache.fetched_at = Utc::now();

    for (i, symbol) in symbols.iter().enumerate() {
        if i > 0 {
            sleep(Duration::from_millis(550)).await;
        }
        for (chunk_from, chunk_to) in &chunks {
            let start_ms = naive_date_start_ms(*chunk_from)?;
            let end_ms = naive_date_end_ms(*chunk_to)?;
            match market
                .price_history()
                .get(
                    symbol,
                    Some("year"),
                    Some(1),
                    Some("daily"),
                    Some(1),
                    Some(start_ms),
                    Some(end_ms),
                    Some(false),
                    Some(false),
                )
                .await
            {
                Ok(history) => {
                    cache.ingest_schwab_history(symbol, &history);
                    tracing::info!(
                        "prefetch {symbol} {chunk_from}..{chunk_to}: {} bars",
                        cache.symbols.get(symbol).map(|v| v.len()).unwrap_or(0)
                    );
                }
                Err(e) => {
                    tracing::warn!("prefetch {symbol} failed ({chunk_from}..{chunk_to}): {e:#}");
                }
            }
            sleep(Duration::from_millis(350)).await;
        }
    }

    cache.save(&cache_path)?;
    Ok(cache)
}

fn date_chunks(from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut chunks = Vec::new();
    let mut start = from;
    while start <= to {
        let year_end = NaiveDate::from_ymd_opt(start.year(), 12, 31).unwrap_or(to);
        let end = year_end.min(to);
        chunks.push((start, end));
        if end >= to {
            break;
        }
        start = end
            .succ_opt()
            .unwrap_or(to);
    }
    chunks
}

fn naive_date_start_ms(d: NaiveDate) -> Result<i64> {
    let dt = d
        .and_hms_opt(0, 0, 0)
        .context("invalid date")?
        .and_utc();
    Ok(dt.timestamp_millis())
}

fn naive_date_end_ms(d: NaiveDate) -> Result<i64> {
    let dt = d
        .and_hms_opt(23, 59, 59)
        .context("invalid date")?
        .and_utc();
    Ok(dt.timestamp_millis())
}

pub fn default_from_to(from: Option<NaiveDate>, to: Option<NaiveDate>) -> Result<(NaiveDate, NaiveDate)> {
    let to = to.unwrap_or_else(|| {
        let today = Utc::now().date_naive();
        today.pred_opt().unwrap_or(today)
    });
    let from = from.unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(to.year() - 1, to.month(), to.day().min(28))
            .unwrap_or(to - chrono::Duration::days(365))
    });
    anyhow::ensure!(from <= to, "from ({from}) must be <= to ({to})");
    Ok((from, to))
}

pub fn parse_ymd(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .with_context(|| format!("invalid date {s:?}, expected YYYY-MM-DD"))
}
