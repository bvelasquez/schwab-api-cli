//! Live Schwab market data or replay from a backtest candle cache.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use schwab_market_data::MarketDataApi;
use serde_json::Value;

use crate::backtest::cache::{BacktestCache, StoredCandle};
use crate::market_cache::{cache_status_json, LiveCacheHandle};
use crate::rules::TraderRules;
use crate::technical::Candle;

#[derive(Clone)]
pub enum MarketCtx {
    Live {
        market: Arc<MarketDataApi>,
        live_cache: Option<Arc<LiveCacheHandle>>,
    },
    Replay {
        cache: Arc<BacktestCache>,
        as_of: DateTime<Utc>,
    },
}

impl MarketCtx {
    pub fn live(market: Arc<MarketDataApi>) -> Self {
        Self::Live {
            market,
            live_cache: None,
        }
    }

    /// Live quotes + optional on-disk daily bar cache (from backtest prefetch).
    pub fn for_rules(market: Arc<MarketDataApi>, rules_path: &Path, rules: &TraderRules) -> Self {
        let config = rules.technical.market_cache.clone();
        let live_cache = LiveCacheHandle::try_load(rules_path, &config).map(Arc::new);
        if live_cache.is_some() {
            tracing::debug!(
                "market cache overlay active: {}",
                cache_status_json(live_cache.as_deref())
            );
        }
        Self::Live {
            market,
            live_cache,
        }
    }

    pub fn replay(cache: Arc<BacktestCache>, as_of: DateTime<Utc>) -> Self {
        Self::Replay { cache, as_of }
    }

    pub fn as_of(&self) -> DateTime<Utc> {
        match self {
            Self::Live { .. } => Utc::now(),
            Self::Replay { as_of, .. } => *as_of,
        }
    }

    pub fn cache_status(&self) -> Value {
        match self {
            Self::Live { live_cache, .. } => cache_status_json(live_cache.as_deref()),
            Self::Replay { cache, .. } => serde_json::json!({
                "mode": "replay",
                "symbols": cache.symbols.len(),
                "from": cache.from.to_string(),
                "to": cache.to.to_string(),
            }),
        }
    }

    pub async fn quote_last_bid_ask(&self, symbol: &str) -> Result<(f64, Option<f64>, Option<f64>)> {
        let symbol = symbol.trim().to_uppercase();
        match self {
            Self::Live { market, .. } => {
                let raw = market
                    .quotes()
                    .get_quote(&symbol, Some("quote"), None)
                    .await?;
                let quote = extract_quote(&raw, &symbol);
                let last = quote_f64(&quote, "lastPrice").unwrap_or(0.0);
                let bid = quote_f64(&quote, "bidPrice");
                let ask = quote_f64(&quote, "askPrice");
                Ok((last, bid, ask))
            }
            Self::Replay { cache, as_of } => {
                let bar = cache
                    .bar_on_or_before(&symbol, *as_of)
                    .with_context(|| format!("no replay bar for {symbol} as of {as_of}"))?;
                let last = bar.close;
                let spread = (last * 0.0005).max(0.01);
                Ok((last, Some(last - spread), Some(last + spread)))
            }
        }
    }

    pub async fn daily_candles(&self, symbol: &str, min_bars: usize) -> Result<Vec<Candle>> {
        self.daily_candles_with_config(symbol, "year", 1, "daily")
            .await
            .and_then(|c| {
                if c.len() < min_bars && !c.is_empty() {
                    tracing::warn!(
                        "{symbol}: have {} daily bars, wanted {min_bars}",
                        c.len()
                    );
                }
                Ok(c)
            })
    }

    pub async fn daily_candles_with_config(
        &self,
        symbol: &str,
        period_type: &str,
        period: u32,
        frequency_type: &str,
    ) -> Result<Vec<Candle>> {
        let symbol = symbol.trim().to_uppercase();
        match self {
            Self::Live { market, live_cache } => {
                self.live_daily_candles(
                    market,
                    live_cache.as_deref(),
                    &symbol,
                    period_type,
                    period,
                    frequency_type,
                )
                .await
            }
            Self::Replay { cache, as_of } => {
                let min = min_bars_for_config(period_type, period);
                let bars = cache.candles_through(&symbol, *as_of);
                if bars.len() < min && !bars.is_empty() {
                    tracing::warn!(
                        "{symbol}: replay has {have} bars, wanted ~{min} for {period_type}/{period}",
                        have = bars.len(),
                        min = min,
                        period_type = period_type,
                        period = period,
                    );
                }
                Ok(bars.iter().map(StoredCandle::to_candle).collect())
            }
        }
    }

    async fn live_daily_candles(
        &self,
        market: &MarketDataApi,
        live_cache: Option<&LiveCacheHandle>,
        symbol: &str,
        period_type: &str,
        period: u32,
        frequency_type: &str,
    ) -> Result<Vec<Candle>> {
        let min = min_bars_for_config(period_type, period);

        if let Some(handle) = live_cache {
            if handle.needs_refresh(symbol) {
                if let Err(err) = handle.refresh_symbol(market, symbol).await {
                    tracing::warn!("market cache refresh failed for {symbol}: {err}");
                }
            }
            let bars = handle
                .cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .candles_through(symbol, Utc::now());
            if bars.len() >= min.min(50) {
                tracing::trace!("market cache hit {symbol}: {} bars", bars.len());
                return Ok(bars.iter().map(StoredCandle::to_candle).collect());
            }
        }

        let history = market
            .price_history()
            .get(
                symbol,
                Some(period_type),
                Some(period),
                Some(frequency_type),
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .with_context(|| format!("fetch daily history for {symbol}"))?;
        let candles = parse_schwab_candles(&history);

        if let Some(handle) = live_cache {
            if let Err(err) = merge_api_into_cache(handle, market, symbol, &history).await {
                tracing::warn!("market cache merge failed for {symbol}: {err}");
            }
        }

        Ok(candles)
    }

    pub fn replay_bar(&self, symbol: &str) -> Option<StoredCandle> {
        match self {
            Self::Replay { cache, as_of } => cache.bar_on_or_before(symbol, *as_of).ok(),
            Self::Live { .. } => None,
        }
    }
}

async fn merge_api_into_cache(
    handle: &LiveCacheHandle,
    market: &MarketDataApi,
    symbol: &str,
    history: &Value,
) -> Result<()> {
    let sym = symbol.trim().to_uppercase();
    {
        let mut cache = handle
            .cache
            .write()
            .unwrap_or_else(|e| e.into_inner());
        cache.ingest_schwab_history(&sym, history);
        cache.fetched_at = Utc::now();
        cache.save(&handle.path)?;
    }
    handle.mark_refreshed(&sym);
    let _ = market;
    Ok(())
}

fn min_bars_for_config(period_type: &str, period: u32) -> usize {
    match period_type {
        "day" => (period as usize).saturating_mul(390),
        "month" => (period as usize).saturating_mul(21),
        "year" => (period as usize).saturating_mul(252),
        "ytd" => 252,
        _ => period as usize,
    }
}

pub fn parse_schwab_candles(history: &Value) -> Vec<Candle> {
    history
        .get("candles")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    Some(Candle {
                        close: c.get("close")?.as_f64()?,
                        high: c.get("high")?.as_f64()?,
                        low: c.get("low")?.as_f64()?,
                        volume: c.get("volume")?.as_f64()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn extract_quote(raw: &Value, symbol: &str) -> Value {
    if let Some(entry) = raw.get(symbol) {
        return entry.get("quote").cloned().unwrap_or(Value::Null);
    }
    raw.get("quote")
        .cloned()
        .unwrap_or_else(|| raw.clone())
}

fn quote_f64(quote: &Value, field: &str) -> Option<f64> {
    quote.get(field).and_then(|v| v.as_f64())
}
