//! Realized volatility for IV/RV entry gates (credit-spread edge filter).

use anyhow::{Context, Result};
use schwab_market_data::MarketDataApi;

/// Annualized realized vol (%) from daily closes — same formula as schwab-trader regime.
pub fn realized_vol_annualized_pct(closes: &[f64], lookback: usize) -> f64 {
    if closes.len() <= lookback + 1 || lookback == 0 {
        return 0.0;
    }
    let mut returns = Vec::new();
    let start = closes.len().saturating_sub(lookback + 1);
    for i in start + 1..closes.len() {
        let prev = closes[i - 1];
        if prev > 0.0 && closes[i] > 0.0 {
            returns.push((closes[i] / prev).ln());
        }
    }
    if returns.is_empty() {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let var = returns
        .iter()
        .map(|r| (r - mean).powi(2))
        .sum::<f64>()
        / returns.len() as f64;
    (var.sqrt() * (252.0_f64).sqrt()) * 100.0
}

/// Fetch daily closes and compute annualized realized vol for `symbol`.
pub async fn fetch_realized_vol_pct(
    market: &MarketDataApi,
    symbol: &str,
    lookback: usize,
) -> Result<Option<f64>> {
    let sym = symbol.trim().to_uppercase();
    let history = market
        .price_history()
        .get(
            &sym,
            Some("year"),
            Some(1),
            Some("daily"),
            None,
            None,
            None,
            None,
            Some(true),
        )
        .await
        .with_context(|| format!("price history for realized vol {sym}"))?;

    let closes: Vec<f64> = history
        .get("candles")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("close").and_then(|v| v.as_f64()))
                .collect()
        })
        .unwrap_or_default();

    let rv = realized_vol_annualized_pct(&closes, lookback);
    if rv > 0.0 {
        Ok(Some(rv))
    } else {
        Ok(None)
    }
}

/// IV / RV ratio when both are positive; `None` if either side is missing.
pub fn iv_rv_ratio(chain_iv_pct: Option<f64>, realized_vol_pct: Option<f64>) -> Option<f64> {
    match (chain_iv_pct, realized_vol_pct) {
        (Some(iv), Some(rv)) if iv > 0.0 && rv > 0.0 => Some(iv / rv),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realized_vol_positive_on_trending_series() {
        let mut closes = Vec::new();
        let mut px = 100.0;
        for i in 0..40 {
            px *= if i % 2 == 0 { 1.01 } else { 0.99 };
            closes.push(px);
        }
        let rv = realized_vol_annualized_pct(&closes, 20);
        assert!(rv > 5.0, "rv={rv}");
    }

    #[test]
    fn iv_rv_ratio_none_when_missing() {
        assert!(iv_rv_ratio(None, Some(15.0)).is_none());
        assert!(iv_rv_ratio(Some(20.0), None).is_none());
        assert!((iv_rv_ratio(Some(22.0), Some(20.0)).unwrap() - 1.1).abs() < 1e-9);
    }
}
