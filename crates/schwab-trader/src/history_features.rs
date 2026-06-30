//! Derived multi-bar features for LLM context and ranking (from cached or live history).

use serde::Serialize;
use serde_json::{json, Value};

use crate::technical::Candle;

#[derive(Debug, Clone, Serialize, Default)]
pub struct HistoryFeatures {
    pub bars_available: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_30d_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_90d_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct_from_52w_high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct_from_52w_low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sma_200: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub above_sma_200: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rs_vs_benchmark_30d_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rs_vs_benchmark_90d_pct: Option<f64>,
}

pub fn compute_history_features(
    symbol_candles: &[Candle],
    last_price: f64,
    benchmark_candles: Option<&[Candle]>,
) -> HistoryFeatures {
    let closes: Vec<f64> = symbol_candles.iter().map(|c| c.close).collect();
    if closes.is_empty() || last_price <= 0.0 {
        return HistoryFeatures::default();
    }

    let return_30d_pct = period_return(&closes, 30);
    let return_90d_pct = period_return(&closes, 90);

    let window_52w = closes.len().min(252);
    let slice_52w = &closes[closes.len().saturating_sub(window_52w)..];
    let high_52w = slice_52w.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let low_52w = slice_52w.iter().copied().fold(f64::INFINITY, f64::min);
    let pct_from_52w_high = if high_52w > 0.0 {
        Some(((last_price / high_52w) - 1.0) * 100.0)
    } else {
        None
    };
    let pct_from_52w_low = if low_52w > 0.0 {
        Some(((last_price / low_52w) - 1.0) * 100.0)
    } else {
        None
    };

    let sma_200 = sma(&closes, 200);
    let above_sma_200 = sma_200.map(|s| last_price >= s);

    let (rs_30, rs_90) = benchmark_candles
        .map(|bench| {
            let bench_closes: Vec<f64> = bench.iter().map(|c| c.close).collect();
            let sym_30 = period_return(&closes, 30);
            let sym_90 = period_return(&closes, 90);
            let bench_30 = period_return(&bench_closes, 30);
            let bench_90 = period_return(&bench_closes, 90);
            (
                relative_strength(sym_30, bench_30),
                relative_strength(sym_90, bench_90),
            )
        })
        .unwrap_or((None, None));

    HistoryFeatures {
        bars_available: closes.len(),
        return_30d_pct: return_30d_pct,
        return_90d_pct: return_90d_pct,
        pct_from_52w_high,
        pct_from_52w_low,
        sma_200,
        above_sma_200,
        rs_vs_benchmark_30d_pct: rs_30,
        rs_vs_benchmark_90d_pct: rs_90,
    }
}

pub fn history_features_to_json(features: &HistoryFeatures) -> Value {
    serde_json::to_value(features).unwrap_or(json!({}))
}

fn period_return(closes: &[f64], days: usize) -> Option<f64> {
    if closes.len() <= days {
        return None;
    }
    let start = closes[closes.len() - days - 1];
    let end = *closes.last()?;
    if start <= 0.0 {
        return None;
    }
    Some(((end / start) - 1.0) * 100.0)
}

fn relative_strength(sym: Option<f64>, bench: Option<f64>) -> Option<f64> {
    match (sym, bench) {
        (Some(s), Some(b)) => Some(s - b),
        _ => None,
    }
}

fn sma(values: &[f64], period: usize) -> Option<f64> {
    if values.len() < period || period == 0 {
        return None;
    }
    let slice = &values[values.len() - period..];
    Some(slice.iter().sum::<f64>() / period as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rising(n: usize) -> Vec<Candle> {
        (0..n)
            .map(|i| Candle {
                close: 100.0 + i as f64,
                high: 101.0 + i as f64,
                low: 99.0 + i as f64,
                volume: 1_000_000.0,
            })
            .collect()
    }

    #[test]
    fn computes_positive_return() {
        let candles = rising(250);
        let f = compute_history_features(&candles, 349.0, None);
        assert!(f.return_30d_pct.unwrap() > 0.0);
        assert!(f.above_sma_200.unwrap_or(false));
    }

    #[test]
    fn relative_strength_vs_benchmark() {
        let sym = rising(100);
        let bench: Vec<Candle> = (0..100)
            .map(|i| Candle {
                close: 100.0 + i as f64 * 0.5,
                high: 0.0,
                low: 0.0,
                volume: 1.0,
            })
            .collect();
        let f = compute_history_features(&sym, 199.0, Some(&bench));
        assert!(f.rs_vs_benchmark_30d_pct.unwrap() > 0.0);
    }
}
