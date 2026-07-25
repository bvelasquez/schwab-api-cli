use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

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

    pub fn trading_date_et(&self) -> NaiveDate {
        let secs = self.datetime_ms / 1000;
        let dt = Utc
            .timestamp_opt(secs, 0)
            .single()
            .unwrap_or_else(Utc::now);
        dt.with_timezone(&New_York).date_naive()
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

    pub fn merge_history(&mut self, symbol: &str, candles: Vec<StoredCandle>) {
        let sym = symbol.trim().to_uppercase();
        let mut merged = self.symbols.get(&sym).cloned().unwrap_or_default();
        merged.extend(candles);
        merged.sort_by_key(|c| c.datetime_ms);
        merged.dedup_by_key(|c| c.datetime_ms);
        self.symbols.insert(sym, merged);
    }

    pub fn ingest_schwab_history(&mut self, symbol: &str, history: &serde_json::Value) {
        let stored: Vec<StoredCandle> = history
            .get("candles")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(StoredCandle::from_schwab_json).collect())
            .unwrap_or_default();
        self.merge_history(symbol, stored);
    }

    pub fn closes_through(&self, symbol: &str, day: NaiveDate) -> Vec<f64> {
        let sym = symbol.trim().to_uppercase();
        self.symbols
            .get(&sym)
            .map(|bars| {
                bars.iter()
                    .filter(|b| b.trading_date_et() <= day)
                    .map(|b| b.close)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn bar_on_date(&self, symbol: &str, day: NaiveDate) -> Option<&StoredCandle> {
        let sym = symbol.trim().to_uppercase();
        self.symbols.get(&sym).and_then(|bars| {
            bars.iter()
                .filter(|b| b.trading_date_et() == day)
                .last()
        })
    }

    pub fn close_on_date(&self, symbol: &str, day: NaiveDate) -> Option<f64> {
        self.bar_on_date(symbol, day).map(|b| b.close)
    }

    /// Prefer `$VIX` then `VIX` for the index level on `day`.
    pub fn vix_on_date(&self, configured: &str, day: NaiveDate) -> Option<f64> {
        let cfg = configured.trim().to_uppercase();
        let candidates = if cfg.is_empty() {
            vec!["$VIX".to_string(), "VIX".to_string()]
        } else {
            let mut v = vec![cfg.clone()];
            if cfg != "$VIX" {
                v.push("$VIX".into());
            }
            if cfg != "VIX" {
                v.push("VIX".into());
            }
            v
        };
        for sym in candidates {
            if let Some(c) = self.close_on_date(&sym, day) {
                return Some(c);
            }
        }
        None
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
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        anyhow::ensure!(
            !days.is_empty(),
            "no trading days for {sym} between {from} and {to}"
        );
        Ok(days)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trading_days_unique_sorted() {
        let mut cache = BacktestCache::new(
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2024, 1, 31).unwrap(),
        );
        // 2024-01-02 and 2024-01-03 ET approx via UTC noon
        cache.merge_history(
            "SPY",
            vec![
                StoredCandle {
                    datetime_ms: 1_704_196_800_000,
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 470.0,
                    volume: 1.0,
                },
                StoredCandle {
                    datetime_ms: 1_704_283_200_000,
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 471.0,
                    volume: 1.0,
                },
            ],
        );
        let days = cache
            .trading_days(
                "SPY",
                NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
            )
            .unwrap();
        assert!(days.len() >= 2);
        assert!(days[0] < days[1]);
    }
}
