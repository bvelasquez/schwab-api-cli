//! On-disk daily bar cache for live ticks (shared with backtest prefetch).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use schwab_market_data::MarketDataApi;
use serde::{Deserialize, Serialize};

use crate::agent::paths::backtest_cache_path;
use crate::backtest::cache::BacktestCache;
use crate::market_session::{trading_day_at, US_EQUITY_TIMEZONE};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketCacheConfig {
    /// Use rules/.backtest-cache-*.json for daily bars when available.
    pub enabled: bool,
    /// Fetch from Schwab and merge when newest cached bar is older than this many calendar days.
    pub refresh_if_older_than_days: u32,
    /// Minimum seconds between per-symbol API refreshes (rate-limit guard).
    pub refresh_cooldown_seconds: u64,
}

impl Default for MarketCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            refresh_if_older_than_days: 1,
            refresh_cooldown_seconds: 3600,
        }
    }
}

pub struct LiveCacheHandle {
    pub cache: Arc<std::sync::RwLock<BacktestCache>>,
    pub path: PathBuf,
    pub config: MarketCacheConfig,
    refresh_cooldown: Arc<Mutex<HashMap<String, Instant>>>,
}

impl LiveCacheHandle {
    pub fn try_load(rules_path: &Path, config: &MarketCacheConfig) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        let path = backtest_cache_path(rules_path);
        let cache = BacktestCache::load(&path).ok()?;
        Some(Self {
            cache: Arc::new(std::sync::RwLock::new(cache)),
            path,
            config: config.clone(),
            refresh_cooldown: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn bar_count(&self, symbol: &str) -> usize {
        let sym = symbol.trim().to_uppercase();
        self.cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .symbols
            .get(&sym)
            .map(|b| b.len())
            .unwrap_or(0)
    }

    pub fn needs_refresh(&self, symbol: &str) -> bool {
        let sym = symbol.trim().to_uppercase();
        let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
        let Some(bars) = cache.symbols.get(&sym) else {
            return true;
        };
        let Some(last) = bars.last() else {
            return true;
        };
        let last_day = last.trading_date_et();
        let today = trading_day_at(Utc::now(), US_EQUITY_TIMEZONE);
        let cutoff = today - chrono::Duration::days(self.config.refresh_if_older_than_days as i64);
        last_day < cutoff
    }

    fn cooldown_ok(&self, symbol: &str) -> bool {
        let sym = symbol.trim().to_uppercase();
        let guard = self.refresh_cooldown.lock().unwrap();
        match guard.get(&sym) {
            Some(t) => t.elapsed() >= Duration::from_secs(self.config.refresh_cooldown_seconds),
            None => true,
        }
    }

    pub(crate) fn mark_refreshed(&self, symbol: &str) {
        let sym = symbol.trim().to_uppercase();
        self.refresh_cooldown
            .lock()
            .unwrap()
            .insert(sym, Instant::now());
    }

    pub async fn refresh_symbol(
        &self,
        market: &MarketDataApi,
        symbol: &str,
    ) -> Result<()> {
        let sym = symbol.trim().to_uppercase();
        if !self.cooldown_ok(&sym) {
            tracing::debug!("market cache refresh skipped (cooldown) for {sym}");
            return Ok(());
        }
        let history = market
            .price_history()
            .get(
                &sym,
                Some("year"),
                Some(1),
                Some("daily"),
                Some(1),
                None,
                None,
                None,
                None,
            )
            .await
            .with_context(|| format!("refresh market cache for {sym}"))?;
        {
            let mut cache = self
                .cache
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cache.ingest_schwab_history(&sym, &history);
            cache.fetched_at = Utc::now();
            cache.save(&self.path)?;
        }
        self.mark_refreshed(&sym);
        tracing::info!(
            "market cache refreshed {sym}: {} bars",
            self.cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .symbols
                .get(&sym)
                .map(|v| v.len())
                .unwrap_or(0)
        );
        Ok(())
    }
}

pub fn cache_status_json(handle: Option<&LiveCacheHandle>) -> serde_json::Value {
    match handle {
        None => serde_json::json!({ "enabled": false }),
        Some(h) => {
            let cache = h.cache.read().unwrap_or_else(|e| e.into_inner());
            serde_json::json!({
                "enabled": true,
                "path": h.path,
                "from": cache.from.to_string(),
                "to": cache.to.to_string(),
                "symbols": cache.symbols.len(),
                "fetched_at": cache.fetched_at.to_rfc3339(),
            })
        }
    }
}

pub fn newest_bar_day(cache: &BacktestCache, symbol: &str) -> Option<chrono::NaiveDate> {
    let sym = symbol.trim().to_uppercase();
    cache.symbols.get(&sym)?.last().map(|b| b.trading_date_et())
}

#[cfg(test)]
mod cache_config_tests {
    use super::MarketCacheConfig;

    #[test]
    fn default_cache_config_enabled() {
        let cfg = MarketCacheConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.refresh_if_older_than_days, 1);
    }
}
