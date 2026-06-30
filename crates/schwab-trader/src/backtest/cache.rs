use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::market_ctx::parse_schwab_candles;
use crate::technical::Candle;

pub const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestCache {
    pub version: u32,
    pub fetched_at: DateTime<Utc>,
    pub from: NaiveDate,
    pub to: NaiveDate,
    #[serde(default)]
    pub symbols: HashMap<String, Vec<StoredCandle>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCandle {
    pub datetime_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl StoredCandle {
    pub fn from_schwab_json(c: &serde_json::Value) -> Option<Self> {
        Some(Self {
            datetime_ms: c.get("datetime")?.as_i64()?,
            open: c.get("open")?.as_f64()?,
            high: c.get("high")?.as_f64()?,
            low: c.get("low")?.as_f64()?,
            close: c.get("close")?.as_f64()?,
            volume: c.get("volume")?.as_f64()?,
        })
    }

    pub fn to_candle(&self) -> Candle {
        Candle {
            close: self.close,
            high: self.high,
            low: self.low,
            volume: self.volume,
        }
    }

    pub fn trading_date_et(&self) -> NaiveDate {
        let secs = self.datetime_ms / 1000;
        let dt = Utc
            .timestamp_opt(secs, 0)
            .single()
            .unwrap_or_else(Utc::now);
        crate::market_session::trading_day_at(dt, crate::market_session::US_EQUITY_TIMEZONE)
    }
}

impl BacktestCache {
    pub fn new(from: NaiveDate, to: NaiveDate) -> Self {
        Self {
            version: CACHE_VERSION,
            fetched_at: Utc::now(),
            from,
            to,
            symbols: HashMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read backtest cache {}", path.display()))?;
        let cache: Self = serde_json::from_str(&raw).context("parse backtest cache JSON")?;
        anyhow::ensure!(
            cache.version == CACHE_VERSION,
            "unsupported backtest cache version {} (expected {CACHE_VERSION})",
            cache.version
        );
        Ok(cache)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(path, raw).with_context(|| format!("write backtest cache {}", path.display()))
    }

    pub fn upsert_symbol_history(&mut self, symbol: &str, history: &serde_json::Value) {
        let sym = symbol.trim().to_uppercase();
        let mut candles: Vec<StoredCandle> = history
            .get("candles")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(StoredCandle::from_schwab_json).collect())
            .unwrap_or_default();
        candles.sort_by_key(|c| c.datetime_ms);
        candles.dedup_by_key(|c| c.datetime_ms);
        if let Some(existing) = self.symbols.get(&sym) {
            let mut merged = existing.clone();
            merged.extend(candles);
            merged.sort_by_key(|c| c.datetime_ms);
            merged.dedup_by_key(|c| c.datetime_ms);
            candles = merged;
        }
        self.symbols.insert(sym, candles);
    }

    pub fn merge_history(&mut self, symbol: &str, candles: Vec<StoredCandle>) {
        let sym = symbol.trim().to_uppercase();
        let mut merged = self.symbols.get(&sym).cloned().unwrap_or_default();
        merged.extend(candles);
        merged.sort_by_key(|c| c.datetime_ms);
        merged.dedup_by_key(|c| c.datetime_ms);
        self.symbols.insert(sym, merged);
    }

    pub fn candles_through(&self, symbol: &str, as_of: DateTime<Utc>) -> Vec<StoredCandle> {
        let sym = symbol.trim().to_uppercase();
        let as_of_ms = as_of.timestamp_millis();
        self.symbols
            .get(&sym)
            .map(|bars| {
                bars.iter()
                    .filter(|b| b.datetime_ms <= as_of_ms)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn bar_on_or_before(&self, symbol: &str, as_of: DateTime<Utc>) -> Result<StoredCandle> {
        self.candles_through(symbol, as_of)
            .into_iter()
            .last()
            .with_context(|| format!("no bar for {symbol} on or before {as_of}"))
    }

    pub fn bar_on_date(&self, symbol: &str, day: NaiveDate) -> Option<StoredCandle> {
        let sym = symbol.trim().to_uppercase();
        self.symbols.get(&sym).and_then(|bars| {
            bars.iter()
                .filter(|b| b.trading_date_et() == day)
                .last()
                .cloned()
        })
    }

    pub fn trading_days(
        &self,
        benchmark: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<NaiveDate>> {
        let sym = benchmark.trim().to_uppercase();
        let bars = self
            .symbols
            .get(&sym)
            .with_context(|| format!("benchmark {sym} missing from cache — run backtest prefetch"))?;
        let days: Vec<NaiveDate> = bars
            .iter()
            .map(|b| b.trading_date_et())
            .filter(|d| *d >= from && *d <= to)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        anyhow::ensure!(!days.is_empty(), "no trading days for {sym} between {from} and {to}");
        Ok(days)
    }

    pub fn ingest_schwab_history(&mut self, symbol: &str, history: &serde_json::Value) {
        let candles = parse_schwab_candles(history);
        let stored: Vec<StoredCandle> = history
            .get("candles")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(StoredCandle::from_schwab_json).collect())
            .unwrap_or_default();
        if stored.is_empty() && !candles.is_empty() {
            tracing::warn!("ingest {symbol}: parsed candles but missing datetime fields");
        }
        self.merge_history(symbol, stored);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trading_days_are_sorted_unique() {
        let mut cache = BacktestCache::new(
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2024, 1, 31).unwrap(),
        );
        cache.merge_history(
            "SPY",
            vec![
                StoredCandle {
                    datetime_ms: 1_704_153_600_000,
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 1.0,
                    volume: 1.0,
                },
                StoredCandle {
                    datetime_ms: 1_704_326_400_000,
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 1.0,
                    volume: 1.0,
                },
            ],
        );
        let days = cache
            .trading_days(
                "SPY",
                NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(),
            )
            .unwrap();
        assert!(days.len() >= 2);
        assert!(days[0] < days[1]);
    }
}
