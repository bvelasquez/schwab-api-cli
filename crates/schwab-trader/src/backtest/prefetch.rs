use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{NaiveDate, TimeZone, Utc};
use chrono::Datelike;
use schwab_market_data::MarketDataApi;
use tokio::time::sleep;

use crate::agent::paths::backtest_cache_path;
use crate::backtest::cache::BacktestCache;
use crate::rules::TraderRules;

pub fn symbols_for_prefetch(rules: &TraderRules, rules_path: Option<&Path>) -> Vec<String> {
    let mut out = rules.all_watchlist_symbols();
    if let Some(path) = rules_path {
        if let Ok(pool) = rules.candidate_pool_symbols(path) {
            for sym in pool {
                if !out.contains(&sym) {
                    out.push(sym);
                }
            }
        }
    }
    let bench = rules.adaptation.regime.benchmark_symbol.trim().to_uppercase();
    if !bench.is_empty() && !out.contains(&bench) {
        out.push(bench);
    }
    let vix = rules.adaptation.regime.vix_symbol.trim().to_uppercase();
    if !vix.is_empty() && !out.contains(&vix) {
        out.push(vix);
    }
    for h in &rules.capital.core_holdings {
        let s = h.trim().to_uppercase();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    out.sort();
    out.dedup();
    out
}

pub async fn prefetch_daily_bars(
    market: &Arc<MarketDataApi>,
    rules: &TraderRules,
    rules_path: &Path,
    from: NaiveDate,
    to: NaiveDate,
    force: bool,
) -> Result<BacktestCache> {
    let cache_path = backtest_cache_path(rules_path);
    if cache_path.is_file() && !force {
        let existing = BacktestCache::load(&cache_path)?;
        if existing.from <= from && existing.to >= to {
            return Ok(existing);
        }
    }

    let symbols = symbols_for_prefetch(rules, Some(rules_path));
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
            let history = market
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
                .with_context(|| {
                    format!("fetch daily history for {symbol} ({chunk_from}..{chunk_to})")
                })?;
            cache.ingest_schwab_history(symbol, &history);
            tracing::info!(
                "prefetch {symbol} {chunk_from}..{chunk_to}: {} bars total",
                cache.symbols.get(symbol).map(|v| v.len()).unwrap_or(0)
            );
            sleep(Duration::from_millis(350)).await;
        }
    }

    cache.save(&cache_path)?;
    Ok(cache)
}

/// Split a date range into calendar-year chunks for Schwab `periodType=year` requests.
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
        start = end + chrono::Duration::days(1);
    }
    chunks
}

fn naive_date_start_ms(day: NaiveDate) -> Result<i64> {
    let tz = crate::market_session::trading_tz(crate::market_session::US_EQUITY_TIMEZONE);
    let dt = tz
        .with_ymd_and_hms(day.year(), day.month(), day.day(), 0, 0, 0)
        .single()
        .context("invalid start date")?;
    Ok(dt.with_timezone(&Utc).timestamp_millis())
}

fn naive_date_end_ms(day: NaiveDate) -> Result<i64> {
    let tz = crate::market_session::trading_tz(crate::market_session::US_EQUITY_TIMEZONE);
    let dt = tz
        .with_ymd_and_hms(day.year(), day.month(), day.day(), 23, 59, 59)
        .single()
        .context("invalid end date")?;
    Ok(dt.with_timezone(&Utc).timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_chunks_split_by_year() {
        let from = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
        let chunks = date_chunks(from, to);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].0, from);
        assert_eq!(chunks[0].1, NaiveDate::from_ymd_opt(2024, 12, 31).unwrap());
        assert_eq!(chunks[2].1, to);
    }
}
