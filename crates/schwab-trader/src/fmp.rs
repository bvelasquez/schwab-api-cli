//! Financial Modeling Prep client — candidate discovery (not trade truth).
//!
//! Free tier: `most-actives`, `biggest-gainers`, `biggest-losers`, single `quote`.
//! `company-screener` is paid on current FMP plans (HTTP 402 on free keys).

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const FMP_BASE: &str = "https://financialmodelingprep.com";
const ENV_KEY: &str = "FMP_API_KEY";

#[derive(Debug, Clone)]
pub struct FmpClient {
    http: Client,
    api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FmpMover {
    pub symbol: String,
    pub name: Option<String>,
    pub price: Option<f64>,
    pub change: Option<f64>,
    #[serde(rename = "changesPercentage")]
    pub changes_percentage: Option<f64>,
    pub exchange: Option<String>,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub min_price: f64,
    pub include_actives: bool,
    pub include_gainers: bool,
    pub include_losers: bool,
    pub limit: usize,
    /// When true, try paid `company-screener` first; fall back to movers on 402.
    pub prefer_screener: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            min_price: 5.0,
            include_actives: true,
            include_gainers: true,
            include_losers: true,
            limit: 40,
            prefer_screener: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoverResult {
    pub mode: String,
    pub candidates: Vec<FmpMover>,
    pub symbols: Vec<String>,
    pub warnings: Vec<String>,
}

impl FmpClient {
    pub fn from_env() -> Result<Self> {
        let api_key = env::var(ENV_KEY).with_context(|| {
            format!("{ENV_KEY} required (add to project .env — Financial Modeling Prep)")
        })?;
        if api_key.trim().is_empty() {
            bail!("{ENV_KEY} is empty");
        }
        Ok(Self {
            http: Client::new(),
            api_key: api_key.trim().to_string(),
        })
    }

    async fn get_json(&self, path_and_query: &str) -> Result<(u16, Value)> {
        let sep = if path_and_query.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!("{FMP_BASE}/{path_and_query}{sep}apikey={}", self.api_key);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("FMP GET {path_and_query}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let value = serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text));
        Ok((status, value))
    }

    pub async fn most_actives(&self) -> Result<Vec<FmpMover>> {
        self.fetch_movers("stable/most-actives", "most-actives")
            .await
    }

    pub async fn biggest_gainers(&self) -> Result<Vec<FmpMover>> {
        self.fetch_movers("stable/biggest-gainers", "biggest-gainers")
            .await
    }

    pub async fn biggest_losers(&self) -> Result<Vec<FmpMover>> {
        self.fetch_movers("stable/biggest-losers", "biggest-losers")
            .await
    }

    async fn fetch_movers(&self, path: &str, source: &str) -> Result<Vec<FmpMover>> {
        let (status, value) = self.get_json(path).await?;
        if status == 402 || status == 403 {
            bail!("FMP {path} restricted (HTTP {status}): {value}");
        }
        if !(200..300).contains(&status) {
            bail!("FMP {path} failed HTTP {status}: {value}");
        }
        let mut rows: Vec<FmpMover> = serde_json::from_value(value)
            .with_context(|| format!("parse FMP movers from {path}"))?;
        for row in &mut rows {
            row.source = source.to_string();
            row.symbol = row.symbol.trim().to_uppercase();
        }
        Ok(rows)
    }

    /// Paid on current free plans — returns Ok(None) when HTTP 402/403.
    pub async fn company_screener(
        &self,
        min_price: f64,
        min_volume: f64,
        limit: usize,
    ) -> Result<Option<Vec<FmpMover>>> {
        let q = format!(
            "stable/company-screener?priceMoreThan={min_price}&volumeMoreThan={min_volume}\
             &isActivelyTrading=true&country=US&exchange=NASDAQ,NYSE,AMEX&limit={limit}"
        );
        let (status, value) = self.get_json(&q).await?;
        if status == 402 || status == 403 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            bail!("FMP company-screener failed HTTP {status}: {value}");
        }
        let arr = value
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("company-screener expected array, got {value}"))?;
        let mut out = Vec::new();
        for item in arr {
            let symbol = item
                .get("symbol")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_uppercase();
            if symbol.is_empty() {
                continue;
            }
            out.push(FmpMover {
                symbol,
                name: item
                    .get("companyName")
                    .or_else(|| item.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                price: item.get("price").and_then(|v| v.as_f64()),
                change: item.get("change").and_then(|v| v.as_f64()),
                changes_percentage: item
                    .get("changesPercentage")
                    .or_else(|| item.get("changePercentage"))
                    .and_then(|v| v.as_f64()),
                exchange: item
                    .get("exchangeShortName")
                    .or_else(|| item.get("exchange"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                source: "company-screener".into(),
            });
        }
        Ok(Some(out))
    }

    pub async fn discover(&self, opts: &DiscoverOptions) -> Result<DiscoverResult> {
        let mut warnings = Vec::new();

        if opts.prefer_screener {
            match self
                .company_screener(opts.min_price, 500_000.0, opts.limit.max(1))
                .await
            {
                Ok(Some(rows)) => {
                    let filtered = filter_movers(rows, opts);
                    return Ok(DiscoverResult {
                        mode: "company-screener".into(),
                        symbols: filtered.iter().map(|r| r.symbol.clone()).collect(),
                        candidates: filtered,
                        warnings,
                    });
                }
                Ok(None) => warnings.push(
                    "company-screener not on this FMP plan (HTTP 402) — using free movers lists"
                        .into(),
                ),
                Err(err) => warnings.push(format!("company-screener error: {err:#}")),
            }
        }

        let mut rows = Vec::new();
        if opts.include_actives {
            rows.extend(self.most_actives().await?);
        }
        if opts.include_gainers {
            rows.extend(self.biggest_gainers().await?);
        }
        if opts.include_losers {
            rows.extend(self.biggest_losers().await?);
        }
        let filtered = filter_movers(rows, opts);
        Ok(DiscoverResult {
            mode: "movers".into(),
            symbols: filtered.iter().map(|r| r.symbol.clone()).collect(),
            candidates: filtered,
            warnings,
        })
    }
}

fn us_listed(exchange: Option<&str>) -> bool {
    match exchange.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("NASDAQ" | "NYSE" | "AMEX" | "NYSEARCA" | "BATS" | "ARCA") => true,
        // Free movers sometimes omit exchange — keep symbol for Schwab to validate.
        None | Some("") => true,
        _ => false,
    }
}

fn looks_like_simple_us_symbol(sym: &str) -> bool {
    let s = sym.trim().to_uppercase();
    if s.is_empty() || s.len() > 5 {
        return false;
    }
    // Drop overseas suffixes / warrants-ish.
    if s.contains('.') || s.contains('-') || s.contains('/') {
        return false;
    }
    s.chars().all(|c| c.is_ascii_alphabetic())
}

fn filter_movers(rows: Vec<FmpMover>, opts: &DiscoverOptions) -> Vec<FmpMover> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in rows {
        if !looks_like_simple_us_symbol(&row.symbol) {
            continue;
        }
        if !us_listed(row.exchange.as_deref()) {
            continue;
        }
        if let Some(px) = row.price {
            if px < opts.min_price {
                continue;
            }
        }
        if !seen.insert(row.symbol.clone()) {
            continue;
        }
        out.push(row);
        if out.len() >= opts.limit {
            break;
        }
    }
    out
}

pub fn write_discovered_pool(path: &Path, symbols: &[String], label: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let mut body = String::new();
    body.push_str("version: 1\n");
    body.push_str(&format!("label: {label}\n"));
    body.push_str("# Auto-generated by `schwab-trader watchlist discover` (FMP).\n");
    body.push_str("# Run `watchlist build --write` to persist thematic YAML (agent also promotes qualifiers in-memory).\n");
    body.push_str("symbols:\n");
    for sym in symbols {
        body.push_str(&format!("  - {sym}\n"));
    }
    fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Why an FMP refresh is being considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmpDiscoverTrigger {
    Premarket,
    AtOpen,
    Periodic,
}

impl FmpDiscoverTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Premarket => "premarket",
            Self::AtOpen => "at_open",
            Self::Periodic => "periodic",
        }
    }
}

/// Intraday FMP refresh gates (premarket / at open / every N minutes).
pub fn fmp_discover_due(
    state: &crate::agent::state::TraderState,
    rules: &crate::rules::TraderRules,
    trigger: FmpDiscoverTrigger,
) -> bool {
    let cfg = &rules.sources.fmp;
    if !cfg.enabled {
        return false;
    }
    let today = crate::market_session::trading_day(&rules.schedule.timezone);
    match trigger {
        FmpDiscoverTrigger::Premarket => {
            cfg.run_premarket && state.last_fmp_premarket_day != Some(today)
        }
        FmpDiscoverTrigger::AtOpen => {
            cfg.run_at_open && state.last_fmp_open_day != Some(today)
        }
        FmpDiscoverTrigger::Periodic => {
            let every = cfg.discover_every_minutes.max(15) as i64;
            match state.last_fmp_discover_at {
                None => true,
                Some(at) => Utc::now().signed_duration_since(at).num_minutes() >= every,
            }
        }
    }
}

/// Fetch FMP candidates and rotate them into `dynamic_watchlist` (keeps open positions).
pub async fn apply_fmp_discover_if_due(
    state: &mut crate::agent::state::TraderState,
    rules: &crate::rules::TraderRules,
    rules_path: &Path,
    market: Option<&crate::market_ctx::MarketCtx>,
    trigger: FmpDiscoverTrigger,
) -> Result<Option<Value>> {
    if !fmp_discover_due(state, rules, trigger) {
        return Ok(None);
    }
    if !rules.watchlists.dynamic {
        return Ok(Some(json!({
            "skipped": true,
            "reason": "watchlists.dynamic is false",
            "trigger": trigger.as_str(),
        })));
    }

    let cfg = &rules.sources.fmp;
    let client = match FmpClient::from_env() {
        Ok(c) => c,
        Err(err) => {
            return Ok(Some(json!({
                "skipped": true,
                "reason": format!("FMP unavailable: {err:#}"),
                "trigger": trigger.as_str(),
            })));
        }
    };

    let opts = DiscoverOptions {
        min_price: rules.playbook.entry.min_price_usd,
        limit: (cfg.max_add_per_refresh.max(1) * 3) as usize,
        prefer_screener: !cfg.movers_only,
        ..DiscoverOptions::default()
    };
    let discovered = client.discover(&opts).await?;

    let max_add = cfg.max_add_per_refresh.max(1) as usize;
    let (candidates, filter_skipped) =
        select_fmp_candidates(rules, market, cfg.require_playbook_pass, &discovered.candidates, max_add)
            .await;

    let previous_fmp = state.fmp_dynamic_symbols.clone();
    let added = crate::watchlist::dynamic_merge::merge_dynamic_symbols(
        state,
        rules,
        &previous_fmp,
        &candidates,
        max_add,
    );

    let today = crate::market_session::trading_day(&rules.schedule.timezone);
    state.fmp_dynamic_symbols = added.clone();
    state.last_fmp_discover_at = Some(Utc::now());
    match trigger {
        FmpDiscoverTrigger::Premarket => state.last_fmp_premarket_day = Some(today),
        FmpDiscoverTrigger::AtOpen => state.last_fmp_open_day = Some(today),
        FmpDiscoverTrigger::Periodic => {}
    }

    let mut pool_path: Option<String> = None;
    if cfg.write_pool {
        let out = rules_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("universe")
            .join("fmp-discovered.yaml");
        write_discovered_pool(&out, &discovered.symbols, "fmp-discovered")?;
        pool_path = Some(out.display().to_string());
    }

    let summary = json!({
        "mode": discovered.mode,
        "trigger": trigger.as_str(),
        "discover_every_minutes": cfg.discover_every_minutes,
        "added": added,
        "playbook_filter_skipped": filter_skipped,
        "require_playbook_pass": cfg.require_playbook_pass,
        "dynamic_watchlist": state.dynamic_watchlist,
        "warnings": discovered.warnings,
        "pool_path": pool_path,
        "candidate_count": discovered.symbols.len(),
    });
    state.last_fmp_discover = Some(summary.clone());
    Ok(Some(summary))
}

async fn select_fmp_candidates(
    rules: &crate::rules::TraderRules,
    market: Option<&crate::market_ctx::MarketCtx>,
    require_playbook_pass: bool,
    rows: &[FmpMover],
    max_add: usize,
) -> (Vec<String>, u32) {
    let mut out = Vec::new();
    let mut filter_skipped = 0u32;

    let bench_candles = if let Some(market) = market {
        let bench_sym = rules.adaptation.regime.benchmark_symbol.trim().to_uppercase();
        if bench_sym.is_empty() {
            Vec::new()
        } else {
            market
                .daily_candles_with_config(&bench_sym, "year", 1, "daily")
                .await
                .unwrap_or_default()
        }
    } else {
        Vec::new()
    };
    let bench_ref = if bench_candles.is_empty() {
        None
    } else {
        Some(bench_candles.as_slice())
    };

    for row in rows {
        if out.len() >= max_add {
            break;
        }
        let sym = row.symbol.trim().to_uppercase();
        if sym.is_empty()
            || rules.is_core_holding(&sym)
            || rules.is_blocked_symbol(&sym)
        {
            continue;
        }

        if require_playbook_pass {
            let Some(market) = market else {
                filter_skipped += 1;
                continue;
            };
            let snap = match crate::technical::fetch_technical_snapshot_with_benchmark(
                market, rules, &sym, bench_ref,
            )
            .await
            {
                Ok(s) => s,
                Err(_) => {
                    filter_skipped += 1;
                    continue;
                }
            };
            if crate::technical::passes_entry_filters(
                &snap,
                &rules.playbook.entry,
                &rules.technical,
                rules,
            )
            .is_some()
            {
                filter_skipped += 1;
                continue;
            }
        }

        if out.iter().any(|s: &String| s.eq_ignore_ascii_case(&sym)) {
            continue;
        }
        out.push(sym);
    }

    (out, filter_skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn filters_penny_and_foreign() {
        let opts = DiscoverOptions {
            min_price: 5.0,
            limit: 10,
            ..DiscoverOptions::default()
        };
        let rows = vec![
            FmpMover {
                symbol: "SOFI".into(),
                name: None,
                price: Some(15.0),
                change: None,
                changes_percentage: None,
                exchange: Some("NASDAQ".into()),
                source: "t".into(),
            },
            FmpMover {
                symbol: "NCRA".into(),
                name: None,
                price: Some(3.0),
                change: None,
                changes_percentage: None,
                exchange: Some("NASDAQ".into()),
                source: "t".into(),
            },
            FmpMover {
                symbol: "AAPL.DE".into(),
                name: None,
                price: Some(200.0),
                change: None,
                changes_percentage: None,
                exchange: Some("XETRA".into()),
                source: "t".into(),
            },
        ];
        let out = filter_movers(rows, &opts);
        assert_eq!(out.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["SOFI"]);
    }

    #[test]
    fn discover_due_premarket_open_and_periodic() {
        use crate::agent::state::TraderState;
        use crate::rules::{FmpSourcesConfig, TraderRules};

        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.sources.fmp = FmpSourcesConfig {
            enabled: true,
            discover_every_minutes: 60,
            run_premarket: true,
            run_at_open: true,
            ..FmpSourcesConfig::default()
        };
        let mut state = TraderState::default();
        assert!(fmp_discover_due(
            &state,
            &rules,
            FmpDiscoverTrigger::Premarket
        ));
        assert!(fmp_discover_due(&state, &rules, FmpDiscoverTrigger::AtOpen));
        assert!(fmp_discover_due(
            &state,
            &rules,
            FmpDiscoverTrigger::Periodic
        ));

        let today = crate::market_session::trading_day(&rules.schedule.timezone);
        state.last_fmp_premarket_day = Some(today);
        assert!(!fmp_discover_due(
            &state,
            &rules,
            FmpDiscoverTrigger::Premarket
        ));

        state.last_fmp_discover_at = Some(Utc::now());
        assert!(!fmp_discover_due(
            &state,
            &rules,
            FmpDiscoverTrigger::Periodic
        ));
        state.last_fmp_discover_at = Some(Utc::now() - chrono::Duration::minutes(90));
        assert!(fmp_discover_due(
            &state,
            &rules,
            FmpDiscoverTrigger::Periodic
        ));
    }
}
