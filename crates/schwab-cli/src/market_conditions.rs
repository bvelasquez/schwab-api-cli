//! Live benchmark quotes for TUI market-conditions panels and CLI summaries.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use schwab_market_data::MarketDataApi;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Symbol and short label shown in the TUI.
pub const DEFAULT_BENCHMARKS: &[(&str, &str)] = &[
    ("SPY", "S&P 500"),
    ("QQQ", "Nasdaq"),
    ("IWM", "Russell"),
    ("DIA", "Dow"),
    ("$VIX", "VIX"),
    ("TLT", "Bonds"),
    ("GLD", "Gold"),
];

const REFRESH_SECS: u64 = 30;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BenchmarkQuote {
    pub symbol: String,
    pub label: String,
    pub last: f64,
    pub change: Option<f64>,
    pub change_pct: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MarketConditionsSnapshot {
    pub quotes: Vec<BenchmarkQuote>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

impl MarketConditionsSnapshot {
    pub fn age_secs(&self) -> Option<i64> {
        self.fetched_at
            .map(|t| (Utc::now() - t).num_seconds().max(0))
    }
}

pub async fn refresh_market_conditions(
    market: &MarketDataApi,
    snapshot: &mut MarketConditionsSnapshot,
) {
    match fetch_benchmark_quotes(market, DEFAULT_BENCHMARKS).await {
        Ok(quotes) => {
            snapshot.quotes = quotes;
            snapshot.fetched_at = Some(Utc::now());
            snapshot.last_error = None;
        }
        Err(err) => {
            snapshot.last_error = Some(err.to_string());
        }
    }
}

pub async fn fetch_benchmark_quotes(
    market: &MarketDataApi,
    benchmarks: &[(&str, &str)],
) -> Result<Vec<BenchmarkQuote>> {
    if benchmarks.is_empty() {
        return Ok(Vec::new());
    }

    let symbols: Vec<String> = benchmarks
        .iter()
        .map(|(sym, _)| sym.trim().to_uppercase())
        .collect();
    let joined = symbols.join(",");
    let raw = market
        .quotes()
        .get_quotes(&joined, Some("quote"), None)
        .await?;

    let mut out = Vec::new();
    for (sym, label) in benchmarks {
        let key = sym.trim().to_uppercase();
        if let Some(q) = parse_benchmark_quote(&key, label, &raw) {
            if q.last > 0.0 {
                out.push(q);
            }
        }
    }
    if out.is_empty() && !benchmarks.is_empty() {
        anyhow::bail!("no benchmark quotes returned");
    }
    Ok(out)
}

fn parse_benchmark_quote(symbol: &str, label: &str, raw: &Value) -> Option<BenchmarkQuote> {
    let entry = extract_symbol_entry(raw, symbol);
    let quote = entry
        .get("quote")
        .cloned()
        .unwrap_or_else(|| entry.clone());
    let regular = entry.get("regular").cloned().unwrap_or(Value::Null);
    let last = quote_f64(&quote, "lastPrice")
        .or_else(|| quote_f64(&regular, "regularMarketLastPrice"))?;
    if last <= 0.0 {
        return None;
    }
    Some(BenchmarkQuote {
        symbol: symbol.to_string(),
        label: label.to_string(),
        last,
        change: quote_f64(&quote, "netChange")
            .or_else(|| quote_f64(&regular, "regularMarketNetChange")),
        change_pct: quote_percent_change(&quote, Some(&regular)),
        bid: quote_f64(&quote, "bidPrice"),
        ask: quote_f64(&quote, "askPrice"),
    })
}

fn extract_symbol_entry(raw: &Value, symbol: &str) -> Value {
    let sym = symbol.trim().to_uppercase();
    if let Some(entry) = raw.get(&sym) {
        return entry.clone();
    }
    if let Some(entry) = raw.get(symbol) {
        return entry.clone();
    }
    if let Some(obj) = raw.as_object() {
        for (k, v) in obj {
            if k.eq_ignore_ascii_case(&sym) || k == symbol {
                return v.clone();
            }
        }
    }
    raw.clone()
}

fn quote_f64(quote: &Value, field: &str) -> Option<f64> {
    let v = quote.get(field)?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.trim().parse().ok())
}

fn quote_percent_change(quote: &Value, regular: Option<&Value>) -> Option<f64> {
    quote_f64(quote, "netPercentChangeInDouble")
        .or_else(|| quote_f64(quote, "netPercentChange"))
        .or_else(|| quote_f64(quote, "markPercentChange"))
        .or_else(|| quote_f64(quote, "percentChange"))
        .or_else(|| regular.and_then(quote_percent_change_regular))
        .or_else(|| computed_percent_change(quote))
        .or_else(|| regular.and_then(computed_percent_change_regular))
}

fn quote_percent_change_regular(regular: &Value) -> Option<f64> {
    quote_f64(regular, "regularMarketPercentChangeInDouble")
        .or_else(|| quote_f64(regular, "regularMarketPercentChange"))
}

fn computed_percent_change_regular(regular: &Value) -> Option<f64> {
    let net = quote_f64(regular, "regularMarketNetChange")?;
    let base = quote_f64(regular, "regularMarketPreviousClose")
        .or_else(|| quote_f64(regular, "regularMarketClose"))
        .filter(|p| *p > 0.0)?;
    Some(net / base * 100.0)
}

fn computed_percent_change(quote: &Value) -> Option<f64> {
    let net = quote_f64(quote, "netChange")?;
    let base = quote_f64(quote, "closePrice").filter(|p| *p > 0.0)?;
    Some(net / base * 100.0)
}

#[allow(dead_code)]
pub fn market_conditions_to_json(snapshot: &MarketConditionsSnapshot) -> Value {
    serde_json::json!({
        "fetched_at": snapshot.fetched_at,
        "age_secs": snapshot.age_secs(),
        "last_error": snapshot.last_error,
        "benchmarks": snapshot.quotes,
    })
}

/// Compact TUI lines: `SPY  $598.12  +0.42%`
pub fn market_conditions_lines(snapshot: &MarketConditionsSnapshot) -> Vec<Line<'static>> {
    if snapshot.quotes.is_empty() {
        let msg = snapshot
            .last_error
            .as_deref()
            .map(|e| format!("(quotes unavailable: {e})"))
            .unwrap_or_else(|| "(fetching market quotes…)".into());
        return vec![Line::from(Span::styled(
            msg,
            Style::default().fg(Color::DarkGray),
        ))];
    }

    let mut lines = Vec::new();
    let chunks: Vec<_> = snapshot.quotes.chunks(2).collect();
    for pair in chunks {
        let mut spans = Vec::new();
        for (i, q) in pair.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  │  "));
            }
            spans.extend(benchmark_spans(q));
        }
        lines.push(Line::from(spans));
    }

    if let Some(age) = snapshot.age_secs() {
        lines.push(Line::from(Span::styled(
            format!("updated {age}s ago"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn benchmark_spans(q: &BenchmarkQuote) -> Vec<Span<'static>> {
    let sym = if q.symbol.starts_with('$') {
        q.symbol.clone()
    } else {
        q.symbol.clone()
    };
    let price = if q.last >= 1000.0 {
        format!("${:.0}", q.last)
    } else if q.last >= 100.0 {
        format!("${:.1}", q.last)
    } else {
        format!("${:.2}", q.last)
    };
    let chg = q
        .change_pct
        .map(|p| format!("{:+.2}%", p))
        .unwrap_or_else(|| "—".into());
    let chg_style = match q.change_pct {
        Some(p) if p >= 0.25 => Style::default().fg(Color::Green),
        Some(p) if p <= -0.25 => Style::default().fg(Color::Red),
        Some(_) => Style::default().fg(Color::Yellow),
        None => Style::default().fg(Color::DarkGray),
    };
    vec![
        Span::styled(
            format!("{sym} "),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(price),
        Span::raw(" "),
        Span::styled(chg, chg_style),
    ]
}

pub fn spawn_market_conditions_feed(
    market: std::sync::Arc<MarketDataApi>,
    snapshot: std::sync::Arc<std::sync::Mutex<MarketConditionsSnapshot>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(REFRESH_SECS));
        interval.tick().await;
        loop {
            let mut fresh = MarketConditionsSnapshot::default();
            refresh_market_conditions(&market, &mut fresh).await;
            if let Ok(mut guard) = snapshot.lock() {
                *guard = fresh;
            }
            interval.tick().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_batch_quote() {
        let raw = json!({
            "SPY": {
                "quote": {
                    "lastPrice": 598.12,
                    "netChange": 2.5,
                    "netPercentChange": 0.42
                }
            },
            "$VIX": {
                "quote": {
                    "lastPrice": 14.2,
                    "netPercentChange": -3.1
                }
            }
        });
        let quotes = DEFAULT_BENCHMARKS
            .iter()
            .filter_map(|(sym, label)| parse_benchmark_quote(sym, label, &raw))
            .collect::<Vec<_>>();
        assert_eq!(quotes.len(), 2);
        assert!((quotes[0].last - 598.12).abs() < 0.01);
        assert!((quotes[0].change_pct.unwrap() - 0.42).abs() < 0.01);
    }

    #[test]
    fn parses_schwab_net_percent_change_field() {
        let raw = json!({
            "SPY": {
                "quote": {
                    "lastPrice": 741.97,
                    "netChange": -3.79,
                    "netPercentChange": -0.50820639
                }
            }
        });
        let q = parse_benchmark_quote("SPY", "S&P 500", &raw).unwrap();
        assert!((q.change_pct.unwrap() - (-0.50820639)).abs() < 0.0001);
    }

    #[test]
    fn computes_percent_from_net_and_close_when_missing() {
        let raw = json!({
            "IWM": {
                "quote": {
                    "lastPrice": 295.53,
                    "netChange": -3.79,
                    "closePrice": 299.32
                }
            }
        });
        let q = parse_benchmark_quote("IWM", "Russell", &raw).unwrap();
        let pct = q.change_pct.unwrap();
        assert!((pct - (-1.266)).abs() < 0.01);
    }

    #[test]
    fn parses_live_schwab_batch_with_all_benchmarks() {
        let raw = json!({
            "SPY": {
                "quote": {
                    "lastPrice": 742.91,
                    "netChange": -2.85,
                    "netPercentChange": -0.38216048
                }
            },
            "QQQ": {
                "quote": {
                    "lastPrice": 711.53,
                    "netChange": -13.64,
                    "netPercentChange": -1.88093826
                }
            },
            "$VIX": {
                "quote": {
                    "lastPrice": 16.67,
                    "netChange": 0.08,
                    "netPercentChange": 0.4822182
                }
            },
            "TLT": {
                "quote": {
                    "lastPrice": 85.44,
                    "netChange": -0.12,
                    "closePrice": 85.56
                }
            }
        });
        let quotes = DEFAULT_BENCHMARKS
            .iter()
            .filter_map(|(sym, label)| parse_benchmark_quote(sym, label, &raw))
            .collect::<Vec<_>>();
        assert_eq!(quotes.len(), 4);
        for q in &quotes {
            assert!(
                q.change_pct.is_some(),
                "{} missing daily %",
                q.symbol
            );
        }
        let lines = market_conditions_lines(&MarketConditionsSnapshot {
            quotes,
            fetched_at: Some(Utc::now()),
            last_error: None,
        });
        let rendered = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.clone()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains(" —"), "expected % not em-dash: {rendered}");
    }

    #[test]
    fn market_lines_not_empty_when_quotes_present() {
        let snap = MarketConditionsSnapshot {
            quotes: vec![BenchmarkQuote {
                symbol: "SPY".into(),
                label: "S&P 500".into(),
                last: 598.0,
                change: Some(1.0),
                change_pct: Some(0.2),
                bid: None,
                ask: None,
            }],
            fetched_at: Some(Utc::now()),
            last_error: None,
        };
        let lines = market_conditions_lines(&snap);
        assert!(!lines.is_empty());
    }
}
