use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::paths::rules_runtime_stem;
use crate::agent::state::AgentState;
use crate::config::RuntimeConfig;
use crate::market_hours::{resolve_eqo_market_open, MarketStatusSource, ResolvedMarketStatus};
use schwab_market_data::MarketDataApi;

const DISK_CACHE_MAX_AGE: Duration = Duration::from_secs(86_400);

/// Live Schwab hours payload shared between watch refresh task and TUI.
#[derive(Debug, Clone, Default)]
pub struct MarketSnapshot {
    pub hours: Option<Value>,
}

impl MarketSnapshot {
    pub fn hours_source(&self) -> Option<MarketStatusSource> {
        self.hours.as_ref().map(|_| MarketStatusSource::SchwabApi)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskHoursCache {
    fetched_at: DateTime<Utc>,
    hours: Value,
}

pub fn market_hours_cache_path(rules_path: &Path) -> std::path::PathBuf {
    let dir = rules_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = rules_runtime_stem(rules_path);
    dir.join(format!(".eqo-market-hours-{stem}.json"))
}

pub fn save_market_hours_cache(rules_path: &Path, hours: &Value) -> std::io::Result<()> {
    let path = market_hours_cache_path(rules_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = DiskHoursCache {
        fetched_at: Utc::now(),
        hours: hours.clone(),
    };
    let content = serde_json::to_string_pretty(&payload)?;
    fs::write(path, content)
}

pub fn load_market_hours_cache(rules_path: &Path) -> Option<Value> {
    let path = market_hours_cache_path(rules_path);
    let content = fs::read_to_string(path).ok()?;
    let cache: DiskHoursCache = serde_json::from_str(&content).ok()?;
    if Utc::now()
        .signed_duration_since(cache.fetched_at)
        .to_std()
        .ok()?
        > DISK_CACHE_MAX_AGE
    {
        return None;
    }
    Some(cache.hours)
}

pub fn resolve_market_status(
    rules_path: &Path,
    state: &AgentState,
    live: Option<&MarketSnapshot>,
) -> ResolvedMarketStatus {
    let now = Utc::now();
    if let Some(snapshot) = live {
        if let Some(hours) = snapshot.hours.as_ref() {
            return resolve_eqo_market_open(Some(hours), state, now, snapshot.hours_source());
        }
    }
    if let Some(hours) = load_market_hours_cache(rules_path) {
        return resolve_eqo_market_open(
            Some(&hours),
            state,
            now,
            Some(MarketStatusSource::HoursCache),
        );
    }
    resolve_eqo_market_open(None, state, now, None)
}

pub async fn refresh_market_snapshot(
    runtime: &RuntimeConfig,
    snapshot: &Arc<Mutex<MarketSnapshot>>,
) {
    let Ok(market) = runtime.build_market_api() else {
        return;
    };
    if let Ok(hours) = fetch_option_hours(&market).await {
        if let Ok(mut guard) = snapshot.lock() {
            *guard = MarketSnapshot { hours: Some(hours) };
        }
    }
}

pub async fn fetch_option_hours(market: &MarketDataApi) -> anyhow::Result<Value> {
    market
        .markets()
        .hours("option", None)
        .await
        .map_err(Into::into)
}

pub fn market_label(status: ResolvedMarketStatus, session: Option<&str>) -> (String, bool) {
    (status.label(session), status.open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn label_never_unknown() {
        let status = ResolvedMarketStatus {
            open: false,
            source: MarketStatusSource::Schedule,
        };
        let (label, open) = market_label(status, Some("overnight"));
        assert!(!label.contains("unknown"));
        assert!(!open);
    }

    #[test]
    fn disk_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("options-test.yaml");
        let hours = json!({ "option": { "EQO": { "isOpen": false } } });
        save_market_hours_cache(&rules, &hours).unwrap();
        let loaded = load_market_hours_cache(&rules).unwrap();
        assert_eq!(loaded, hours);
    }
}
