use anyhow::Result;
use schwab_market_data::MarketDataApi;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::rules::{EntryConfig, IntradayConfig, TechnicalConfig, TraderRules};

#[derive(Debug, Clone)]
pub struct Candle {
    pub close: f64,
    pub high: f64,
    pub low: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TechnicalSnapshot {
    pub symbol: String,
    pub last: f64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub spread_pct: Option<f64>,
    pub sma_9: Option<f64>,
    pub sma_20: Option<f64>,
    pub sma_50: Option<f64>,
    pub rsi_14: Option<f64>,
    pub atr_14: Option<f64>,
    pub volume_sma_20: Option<f64>,
    pub relative_volume: Option<f64>,
    pub above_sma_9: Option<bool>,
    pub above_sma_20: Option<bool>,
    pub above_sma_50: Option<bool>,
    pub intraday: bool,
}

pub async fn fetch_technical_snapshot(
    market: &Arc<MarketDataApi>,
    rules: &TraderRules,
    symbol: &str,
) -> Result<TechnicalSnapshot> {
    let symbol = symbol.trim().to_uppercase();
    let quote_raw = market
        .quotes()
        .get_quote(&symbol, Some("quote"), None)
        .await?;
    let quote = extract_quote(&quote_raw, &symbol);
    let last = quote_f64(&quote, "lastPrice").unwrap_or(0.0);
    let bid = quote_f64(&quote, "bidPrice");
    let ask = quote_f64(&quote, "askPrice");
    let spread_pct = match (bid, ask, last) {
        (Some(b), Some(a), l) if l > 0.0 => Some(((a - b) / l) * 100.0),
        _ => None,
    };

    let hist = rules.effective_history();
    let history = market
        .price_history()
        .get(
            &symbol,
            Some(&hist.period_type),
            Some(hist.period),
            Some(&hist.frequency_type),
            None,
            None,
            None,
            None,
            None,
        )
        .await?;
    let candles = parse_candles(&history);

    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let volumes: Vec<f64> = candles.iter().map(|c| c.volume).collect();
    let sma_9 = sma(&closes, 9);
    let sma_20 = sma(&closes, 20);
    let sma_50 = sma(&closes, 50);
    let rsi_14 = rsi(&closes, 14);
    let atr_14 = atr(&candles, 14);
    let volume_sma_20 = sma(&volumes, 20);
    let relative_volume = candles.last().and_then(|last_bar| {
        volume_sma_20
            .filter(|avg| *avg > 0.0)
            .map(|avg| last_bar.volume / avg)
    });

    Ok(TechnicalSnapshot {
        symbol: symbol.clone(),
        last,
        bid,
        ask,
        spread_pct,
        sma_9,
        sma_20,
        sma_50,
        rsi_14,
        atr_14,
        volume_sma_20,
        relative_volume,
        above_sma_9: sma_9.map(|s| last >= s),
        above_sma_20: sma_20.map(|s| last >= s),
        above_sma_50: sma_50.map(|s| last >= s),
        intraday: rules.is_intraday(),
    })
}

pub fn passes_entry_filters(
    snap: &TechnicalSnapshot,
    entry: &EntryConfig,
    tech: &TechnicalConfig,
    rules: &TraderRules,
) -> Option<String> {
    if snap.volume_sma_20.is_none() {
        return Some("missing volume_sma_20".into());
    }
    if snap.rsi_14.is_none() {
        return Some("missing rsi_14".into());
    }
    if snap.spread_pct.is_none() {
        return Some("missing spread_pct".into());
    }

    if snap.last < entry.min_price_usd {
        return Some(format!("price {:.2} below min", snap.last));
    }
    if let Some(vol) = snap.volume_sma_20 {
        if vol < entry.min_avg_volume_20d {
            return Some(format!("avg volume {vol:.0} below min"));
        }
    }
    if let Some(spread) = snap.spread_pct {
        if spread > entry.max_spread_pct {
            return Some(format!("spread {spread:.2}% too wide"));
        }
    }
    for period in &entry.require_above_sma {
        match *period {
            9 => {
                if snap.above_sma_9 == Some(false) {
                    return Some("below SMA 9".into());
                }
            }
            20 => {
                if snap.above_sma_20 == Some(false) {
                    return Some("below SMA 20".into());
                }
            }
            50 => {
                if snap.above_sma_50 == Some(false) {
                    return Some("below SMA 50".into());
                }
            }
            _ => {}
        }
    }
    if let Some(rsi) = snap.rsi_14 {
        if rsi < entry.rsi_14_range[0] || rsi > entry.rsi_14_range[1] {
            return Some(format!("RSI {rsi:.1} outside range"));
        }
    }

    if rules.is_intraday() {
        if let Some(reason) = passes_intraday_filters(snap, &rules.playbook.intraday) {
            return Some(reason);
        }
    }

    let _ = tech;
    None
}

fn passes_intraday_filters(snap: &TechnicalSnapshot, cfg: &IntradayConfig) -> Option<String> {
    if let Some(rv) = snap.relative_volume {
        if rv < cfg.min_relative_volume {
            return Some(format!(
                "relative volume {rv:.2} below min {:.2}",
                cfg.min_relative_volume
            ));
        }
    }
    if let Some(rsi) = snap.rsi_14 {
        if rsi < cfg.momentum_rsi_min {
            return Some(format!(
                "RSI {rsi:.1} below momentum floor {:.1}",
                cfg.momentum_rsi_min
            ));
        }
    }
    for period in &cfg.require_above_sma {
        match *period {
            9 => {
                if snap.above_sma_9 == Some(false) {
                    return Some("intraday: below SMA 9".into());
                }
            }
            20 => {
                if snap.above_sma_20 == Some(false) {
                    return Some("intraday: below SMA 20".into());
                }
            }
            _ => {}
        }
    }
    None
}

pub fn technical_to_json(snap: &TechnicalSnapshot) -> Value {
    serde_json::to_value(snap).unwrap_or(json!({}))
}

fn extract_quote(raw: &Value, symbol: &str) -> Value {
    if let Some(entry) = raw.get(&symbol) {
        return entry.get("quote").cloned().unwrap_or(json!({}));
    }
    raw.get("quote")
        .cloned()
        .unwrap_or_else(|| raw.clone())
}

fn quote_f64(quote: &Value, field: &str) -> Option<f64> {
    quote.get(field).and_then(|v| v.as_f64())
}

fn parse_candles(history: &Value) -> Vec<Candle> {
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

fn sma(values: &[f64], period: usize) -> Option<f64> {
    if values.len() < period || period == 0 {
        return None;
    }
    let slice = &values[values.len() - period..];
    Some(slice.iter().sum::<f64>() / period as f64)
}

fn rsi(closes: &[f64], period: usize) -> Option<f64> {
    if closes.len() <= period {
        return None;
    }
    let mut gains = 0.0;
    let mut losses = 0.0;
    for i in (closes.len() - period)..closes.len() {
        let diff = closes[i] - closes[i - 1];
        if diff >= 0.0 {
            gains += diff;
        } else {
            losses -= diff;
        }
    }
    if losses == 0.0 {
        return Some(100.0);
    }
    let rs = gains / losses;
    Some(100.0 - (100.0 / (1.0 + rs)))
}

fn atr(candles: &[Candle], period: usize) -> Option<f64> {
    if candles.len() <= period {
        return None;
    }
    let mut trs = Vec::new();
    for i in 1..candles.len() {
        let high = candles[i].high;
        let low = candles[i].low;
        let prev_close = candles[i - 1].close;
        let tr = (high - low)
            .max((high - prev_close).abs())
            .max((low - prev_close).abs());
        trs.push(tr);
    }
    sma(&trs, period)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_computes_tail() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((sma(&v, 3).unwrap() - 4.0).abs() < 0.01);
    }
}
