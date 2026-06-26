//! Agent-oriented market snapshot: quote + fundamentals + price context + research hints.

use std::collections::HashMap;

use anyhow::{Context, Result};
use schwab_market_data::MarketDataApi;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct InfoOptions {
    pub include_history: bool,
    pub history_period_type: String,
    pub history_period: u32,
    pub history_frequency_type: String,
}

impl Default for InfoOptions {
    fn default() -> Self {
        Self {
            include_history: true,
            history_period_type: "month".to_string(),
            history_period: 1,
            history_frequency_type: "daily".to_string(),
        }
    }
}

pub async fn build_symbol_info(
    api: &MarketDataApi,
    symbol: &str,
    options: &InfoOptions,
) -> Result<Value> {
    let symbol = symbol.trim().to_uppercase();

    let quote_raw = api
        .quotes()
        .get_quote(&symbol, Some("all"), None)
        .await
        .with_context(|| format!("Failed to fetch quote for {symbol}"))?;

    let instrument_raw = api
        .instruments()
        .search(&symbol, "fundamental")
        .await
        .with_context(|| format!("Failed to fetch instrument fundamentals for {symbol}"))?;

    let quote_entry = extract_quote_entry(&quote_raw, &symbol);
    let instrument_entry = extract_instrument_entry(&instrument_raw, &symbol);

    let asset_main = quote_entry
        .get("assetMainType")
        .or_else(|| instrument_entry.get("assetType"))
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN")
        .to_string();

    let asset_sub = quote_entry
        .get("assetSubType")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let reference = quote_entry.get("reference").cloned().unwrap_or(json!({}));
    let quote = quote_entry.get("quote").cloned().unwrap_or(json!({}));
    let regular = quote_entry.get("regular").cloned().unwrap_or(json!({}));
    let quote_fundamental = quote_entry.get("fundamental").cloned().unwrap_or(json!({}));
    let instrument_fundamental = instrument_entry
        .get("fundamental")
        .cloned()
        .unwrap_or(json!({}));

    let identity = json!({
        "symbol": symbol,
        "description": reference.get("description")
            .or(instrument_entry.get("description")),
        "cusip": reference.get("cusip").or(instrument_entry.get("cusip")),
        "exchange": reference.get("exchangeName")
            .or(reference.get("exchange"))
            .or(instrument_entry.get("exchange")),
        "assetMainType": asset_main,
        "assetSubType": asset_sub,
        "optionable": reference.get("optionable"),
        "shortable": reference.get("isShortable"),
    });

    let fundamentals = merge_objects(quote_fundamental, instrument_fundamental);

    let mut price_context = json!(null);
    if options.include_history {
        let history = api
            .price_history()
            .get(
                &symbol,
                Some(options.history_period_type.as_str()),
                Some(options.history_period),
                Some(options.history_frequency_type.as_str()),
                None,
                None,
                None,
                None,
                Some(true),
            )
            .await
            .with_context(|| format!("Failed to fetch price history for {symbol}"))?;
        price_context = summarize_history(&history, options);
    }

    let research_hints = build_research_hints(&symbol, &identity, &fundamentals, &price_context);

    Ok(json!({
        "symbol": symbol,
        "identity": identity,
        "quote": quote,
        "regularSession": regular,
        "fundamentals": fundamentals,
        "priceContext": price_context,
        "researchHints": research_hints,
        "sources": {
            "quote": "GET /{symbol}/quotes?fields=all",
            "fundamentals": "GET /instruments?projection=fundamental",
            "priceHistory": if options.include_history {
                Value::String(format!(
                    "GET /pricehistory?periodType={}&period={}&frequencyType={}",
                    options.history_period_type, options.history_period, options.history_frequency_type
                ))
            } else {
                Value::Null
            }
        }
    }))
}

pub async fn build_info_dossier(
    api: &MarketDataApi,
    symbols: &[String],
    options: InfoOptions,
) -> Result<Value> {
    let mut entries = Vec::new();
    for symbol in symbols {
        entries.push(build_symbol_info(api, symbol, &options).await?);
    }
    Ok(json!({
        "count": entries.len(),
        "symbols": entries,
        "agentNote": "Use Schwab data for live prices, dividends, and ratios. Use researchHints.recommendedWebQueries for qualitative context (holdings, sector, news, alternatives) before trade plans."
    }))
}

fn extract_quote_entry(raw: &Value, symbol: &str) -> Value {
    if let Some(entry) = raw.get(symbol) {
        return entry.clone();
    }
    if let Some(obj) = raw.as_object() {
        if obj.len() == 1 {
            return obj.values().next().cloned().unwrap_or(json!({}));
        }
    }
    json!({})
}

fn extract_instrument_entry(raw: &Value, symbol: &str) -> Value {
    let Some(list) = raw.get("instruments").and_then(|v| v.as_array()) else {
        return json!({});
    };
    list.iter()
        .find(|i| {
            i.get("symbol")
                .and_then(|s| s.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
        })
        .cloned()
        .unwrap_or_else(|| list.first().cloned().unwrap_or(json!({})))
}

fn merge_objects(a: Value, b: Value) -> Value {
    let mut out = HashMap::<String, Value>::new();
    for obj in [a, b] {
        if let Some(map) = obj.as_object() {
            for (k, v) in map {
                if !v.is_null() {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
    }
    serde_json::to_value(out).unwrap_or(json!({}))
}

fn summarize_history(history: &Value, options: &InfoOptions) -> Value {
    let candles = history
        .get("candles")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    if candles.is_empty() {
        return json!({
            "empty": true,
            "periodType": options.history_period_type,
            "period": options.history_period,
            "frequencyType": options.history_frequency_type,
        });
    }

    let first = &candles[0];
    let last = candles.last().unwrap();
    let first_close = first.get("close").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let last_close = last.get("close").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let change = last_close - first_close;
    let change_pct = if first_close.abs() > f64::EPSILON {
        (change / first_close) * 100.0
    } else {
        0.0
    };

    let (high, low) = candles.iter().fold((f64::MIN, f64::MAX), |(hi, lo), c| {
        let h = c.get("high").and_then(|v| v.as_f64()).unwrap_or(hi);
        let l = c.get("low").and_then(|v| v.as_f64()).unwrap_or(lo);
        (hi.max(h), lo.min(l))
    });

    let avg_volume = candles
        .iter()
        .filter_map(|c| c.get("volume").and_then(|v| v.as_f64()))
        .sum::<f64>()
        / candles.len() as f64;

    let previous_close = history.get("previousClose");
    let recent = candles
        .iter()
        .rev()
        .take(5)
        .map(|c| {
            json!({
                "datetime": c.get("datetime"),
                "close": c.get("close"),
                "volume": c.get("volume"),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "empty": false,
        "periodType": options.history_period_type,
        "period": options.history_period,
        "frequencyType": options.history_frequency_type,
        "candleCount": candles.len(),
        "firstClose": first_close,
        "lastClose": last_close,
        "change": change,
        "changePercent": change_pct,
        "rangeHigh": high,
        "rangeLow": low,
        "averageVolume": avg_volume,
        "previousClose": previous_close,
        "recentCandles": recent,
    })
}

fn build_research_hints(
    symbol: &str,
    identity: &Value,
    fundamentals: &Value,
    price_context: &Value,
) -> Value {
    let description = identity
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or(symbol);
    let asset_main = identity
        .get("assetMainType")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    let asset_sub = identity.get("assetSubType").and_then(|v| v.as_str());

    let asset_class = classify_asset(asset_main, asset_sub);
    let div_yield = fundamentals
        .get("dividendYield")
        .or_else(|| fundamentals.get("divYield"))
        .and_then(|v| v.as_f64());

    let mut recommended_queries = vec![
        format!("{description} ({symbol}) latest news and outlook"),
        format!("{symbol} key risks and recent performance"),
    ];

    let data_gaps = vec![
        "Schwab API does not provide business narrative, management, or SEC filing summaries"
            .to_string(),
        "Schwab API does not provide ETF/mutual fund full holdings or expense ratio".to_string(),
        "Schwab API does not provide analyst ratings or price targets".to_string(),
    ];

    let mut focus_areas = Vec::new();

    match asset_class.as_str() {
        "etf" | "mutual_fund" => {
            recommended_queries.push(format!(
                "{symbol} ETF holdings top positions expense ratio issuer fact sheet"
            ));
            recommended_queries.push(format!(
                "{symbol} alternatives comparison dividend yield duration credit risk"
            ));
            focus_areas.extend([
                "Holdings composition and sector/country weights".to_string(),
                "Expense ratio and tracking difference vs benchmark".to_string(),
                "Distribution policy and tax treatment".to_string(),
                "Comparable ETFs/funds for substitution in rebalance plans".to_string(),
            ]);
            if div_yield.is_some() {
                focus_areas
                    .push("Verify distribution sustainability vs underlying yield".to_string());
            }
        }
        "equity" => {
            recommended_queries.push(format!(
                "{symbol} earnings revenue growth competitors moat SEC 10-K summary"
            ));
            recommended_queries.push(format!(
                "{symbol} analyst consensus price target institutional ownership"
            ));
            focus_areas.extend([
                "Business model, competitive position, and recent earnings".to_string(),
                "Valuation vs sector (P/E, growth, margins from Schwab fundamentals)".to_string(),
                "Catalysts and risks for the planned holding period".to_string(),
            ]);
        }
        _ => {
            recommended_queries.push(format!(
                "{symbol} {asset_main} instrument structure and risks"
            ));
            focus_areas.push("Confirm instrument type and settlement before trading".to_string());
        }
    }

    if !price_context.is_null()
        && price_context.get("empty").and_then(|v| v.as_bool()) == Some(false)
    {
        let change_pct = price_context
            .get("changePercent")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if change_pct.abs() > 5.0 {
            focus_areas.push(format!(
                "Recent {change_pct:.1}% move over lookback — verify news-driven volatility"
            ));
        }
    }

    json!({
        "assetClass": asset_class,
        "schwabCovers": [
            "Live quote, bid/ask, volume, session prices",
            "Dividend schedule and yield (when available)",
            "Key ratios for equities (P/E, margins, market cap, beta)",
            "Recent OHLCV price context"
        ],
        "schwabDoesNotCover": data_gaps,
        "recommendedWebQueries": recommended_queries,
        "planBuildFocus": focus_areas,
        "suggestedWorkflow": [
            format!("schwab market info {symbol} --json"),
            "Web research using recommendedWebQueries",
            "schwab portfolio summary --json",
            "schwab plan prompt --json → draft YAML plan → schwab plan validate → schwab plan run --dry-run"
        ]
    })
}

fn classify_asset(asset_main: &str, asset_sub: Option<&str>) -> String {
    match asset_sub {
        Some("ETF") => "etf".to_string(),
        Some(sub) if sub.contains("FUND") || sub.eq_ignore_ascii_case("MUTUAL_FUND") => {
            "mutual_fund".to_string()
        }
        _ => match asset_main {
            "EQUITY" => "equity".to_string(),
            "MUTUAL_FUND" => "mutual_fund".to_string(),
            "INDEX" => "index".to_string(),
            "OPTION" => "option".to_string(),
            "FUTURE" => "future".to_string(),
            "FOREX" => "forex".to_string(),
            other => other.to_lowercase(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_candle_range() {
        let history = json!({
            "candles": [
                {"close": 100.0, "high": 101.0, "low": 99.0, "volume": 1000, "datetime": 1},
                {"close": 102.0, "high": 103.0, "low": 100.5, "volume": 2000, "datetime": 2}
            ],
            "previousClose": 99.5
        });
        let summary = summarize_history(&history, &InfoOptions::default());
        assert_eq!(summary["candleCount"], 2);
        assert_eq!(summary["firstClose"], 100.0);
        assert_eq!(summary["lastClose"], 102.0);
        assert_eq!(summary["changePercent"], 2.0);
        assert_eq!(summary["rangeHigh"], 103.0);
        assert_eq!(summary["rangeLow"], 99.0);
    }

    #[test]
    fn classifies_etf() {
        assert_eq!(classify_asset("EQUITY", Some("ETF")), "etf");
        assert_eq!(classify_asset("EQUITY", None), "equity");
    }

    #[test]
    fn merges_fundamentals_prefers_non_null() {
        let a = json!({"divYield": 3.5, "peRatio": null});
        let b = json!({"peRatio": 20.0, "marketCap": 100.0});
        let merged = merge_objects(a, b);
        assert_eq!(merged["divYield"], 3.5);
        assert_eq!(merged["peRatio"], 20.0);
        assert_eq!(merged["marketCap"], 100.0);
    }
}
