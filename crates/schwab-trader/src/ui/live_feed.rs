//! Background Schwab quote refresh for watch TUI.

use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use schwab_market_data::MarketDataApi;
use serde_json::Value;

use crate::agent::state::load_state;
use crate::market_ctx::MarketCtx;
use crate::rules::TraderRules;
use crate::ui::live::{collect_quote_symbols, QuoteTick, WatchLiveSnapshot};

const QUOTE_REFRESH_SECS: u64 = 10;

pub fn new_live_snapshot() -> Arc<RwLock<WatchLiveSnapshot>> {
    Arc::new(RwLock::new(WatchLiveSnapshot::default()))
}

pub fn spawn_live_quote_feed(
    rules_path: std::path::PathBuf,
    live: Arc<RwLock<WatchLiveSnapshot>>,
    market_api: Arc<MarketDataApi>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let rules = match TraderRules::load(&rules_path) {
            Ok(r) => r,
            Err(err) => {
                if let Ok(mut g) = live.write() {
                    g.last_error = Some(format!("rules load: {err:#}"));
                }
                return;
            }
        };
        let market = MarketCtx::for_rules(market_api, &rules_path, &rules);
        let mut interval = tokio::time::interval(Duration::from_secs(QUOTE_REFRESH_SECS));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(err) = refresh_once(&rules_path, &rules, &market, &live).await {
                if let Ok(mut g) = live.write() {
                    g.last_error = Some(err.to_string());
                }
            }
        }
    })
}

async fn refresh_once(
    rules_path: &Path,
    rules: &TraderRules,
    market: &MarketCtx,
    live: &Arc<RwLock<WatchLiveSnapshot>>,
) -> Result<()> {
    let state = load_state(rules_path, &rules.trader_id)?;
    let symbols = collect_quote_symbols(&state);
    if symbols.is_empty() {
        if let Ok(mut g) = live.write() {
            g.last_fetch = Some(Utc::now());
            g.last_error = None;
        }
        return Ok(());
    }

    let mut quotes = fetch_quotes_batch(market, &symbols).await?;
    let now = Utc::now();
    for q in quotes.values_mut() {
        q.fetched_at = now;
    }

    if let Ok(mut g) = live.write() {
        g.quotes = quotes;
        g.last_fetch = Some(now);
        g.last_error = None;
    }
    Ok(())
}

async fn fetch_quotes_batch(
    market: &MarketCtx,
    symbols: &[String],
) -> Result<std::collections::HashMap<String, QuoteTick>> {
    let mut out = std::collections::HashMap::new();
    for symbol in symbols {
        match market.quote_last_bid_ask(symbol).await {
            Ok((last, bid, ask)) if last > 0.0 => {
                out.insert(
                    symbol.to_uppercase(),
                    QuoteTick {
                        symbol: symbol.clone(),
                        last,
                        bid,
                        ask,
                        fetched_at: Utc::now(),
                    },
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!("quote {symbol}: {err:#}");
            }
        }
    }
    if out.is_empty() && !symbols.is_empty() {
        anyhow::bail!("no quotes returned for {}", symbols.join(","));
    }
    Ok(out)
}

#[allow(dead_code)]
fn parse_batch_quotes(raw: &Value, symbols: &[String]) -> std::collections::HashMap<String, QuoteTick> {
    let mut out = std::collections::HashMap::new();
    let now = Utc::now();
    for sym in symbols {
        let quote = extract_quote_node(raw, sym);
        let last = quote_f64(&quote, "lastPrice").unwrap_or(0.0);
        if last <= 0.0 {
            continue;
        }
        out.insert(
            sym.to_uppercase(),
            QuoteTick {
                symbol: sym.clone(),
                last,
                bid: quote_f64(&quote, "bidPrice"),
                ask: quote_f64(&quote, "askPrice"),
                fetched_at: now,
            },
        );
    }
    out
}

fn extract_quote_node(raw: &Value, symbol: &str) -> Value {
    let sym = symbol.trim().to_uppercase();
    if let Some(entry) = raw.get(&sym) {
        return entry.get("quote").cloned().unwrap_or(Value::Null);
    }
    raw.get("quote")
        .cloned()
        .unwrap_or_else(|| raw.clone())
}

fn quote_f64(quote: &Value, field: &str) -> Option<f64> {
    quote.get(field).and_then(|v| v.as_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_batch_quote_shape() {
        let raw = json!({
            "AMD": { "quote": { "lastPrice": 160.5, "bidPrice": 160.4, "askPrice": 160.6 } }
        });
        let q = parse_batch_quotes(&raw, &["AMD".into()]);
        assert!((q["AMD"].last - 160.5).abs() < 0.01);
    }
}
