//! Realized volatility for IV/RV entry gates (credit-spread edge filter).

use anyhow::{Context, Result};
use schwab_market_data::MarketDataApi;

use crate::rules::RvEstimator;

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

/// Weights on the daily / weekly / monthly variance components of the HAR-RV forecast.
/// Corsi (2009) estimates roughly 0.36 / 0.28 / 0.28 plus a small intercept on realized
/// variance. These are renormalized to sum to 1 with no intercept, and are fixed rather
/// than fitted, so the forecast has no look-ahead and nothing to overfit.
const HAR_WEIGHTS: (f64, f64, f64) = (0.4, 0.3, 0.3);
const HAR_WEEK: usize = 5;
const HAR_MONTH: usize = 22;

/// Annualized vol forecast (%) from daily closes: a weighted blend of the last day's
/// squared return, the 5-day mean squared return, and the 22-day mean squared return.
/// `None` when there are fewer than 23 closes. Close-to-close data stands in for the
/// intraday realized variance the original HAR model uses, so the daily term is noisy.
pub fn har_rv_forecast_annualized_pct(closes: &[f64]) -> Option<f64> {
    let returns: Vec<f64> = closes
        .windows(2)
        .filter(|w| w[0] > 0.0 && w[1] > 0.0)
        .map(|w| (w[1] / w[0]).ln())
        .collect();
    if returns.len() < HAR_MONTH {
        return None;
    }
    let mean_sq = |n: usize| -> f64 {
        let tail = &returns[returns.len() - n..];
        tail.iter().map(|r| r * r).sum::<f64>() / n as f64
    };
    let daily = returns[returns.len() - 1].powi(2);
    let var = HAR_WEIGHTS.0 * daily + HAR_WEIGHTS.1 * mean_sq(HAR_WEEK) + HAR_WEIGHTS.2 * mean_sq(HAR_MONTH);
    let vol = (var * 252.0).sqrt() * 100.0;
    (vol > 0.0).then_some(vol)
}

/// Fetch daily closes and compute annualized realized vol for `symbol`.
pub async fn fetch_realized_vol_pct(
    market: &MarketDataApi,
    symbol: &str,
    lookback: usize,
) -> Result<Option<f64>> {
    fetch_vol_estimate_pct(market, symbol, lookback, RvEstimator::Lookback).await
}

/// Fetch daily closes and estimate vol with the chosen estimator.
pub async fn fetch_vol_estimate_pct(
    market: &MarketDataApi,
    symbol: &str,
    lookback: usize,
    estimator: RvEstimator,
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

    let rv = match estimator {
        RvEstimator::Lookback => Some(realized_vol_annualized_pct(&closes, lookback)).filter(|v| *v > 0.0),
        RvEstimator::Har => har_rv_forecast_annualized_pct(&closes),
    };
    Ok(rv)
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

    fn closes_from_returns(returns: &[f64]) -> Vec<f64> {
        let mut px = 100.0;
        let mut out = vec![px];
        for r in returns {
            px *= r.exp();
            out.push(px);
        }
        out
    }

    #[test]
    fn har_needs_a_month_of_returns() {
        let closes = closes_from_returns(&[0.01; 21]);
        assert!(har_rv_forecast_annualized_pct(&closes).is_none());
        let closes = closes_from_returns(&[0.01; 22]);
        assert!(har_rv_forecast_annualized_pct(&closes).is_some());
    }

    #[test]
    fn har_equals_flat_vol_when_every_day_moves_the_same() {
        // Every |return| is 1%, so all three components agree: sqrt(252) * 1%.
        let returns: Vec<f64> = (0..40).map(|i| if i % 2 == 0 { 0.01 } else { -0.01 }).collect();
        let got = har_rv_forecast_annualized_pct(&closes_from_returns(&returns)).unwrap();
        assert!((got - 252.0_f64.sqrt()).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn har_rises_after_a_shock_day_more_than_trailing_vol_does() {
        let mut calm: Vec<f64> = (0..39).map(|i| if i % 2 == 0 { 0.005 } else { -0.005 }).collect();
        let before = har_rv_forecast_annualized_pct(&closes_from_returns(&calm)).unwrap();
        calm.push(-0.04);
        let closes = closes_from_returns(&calm);
        let after = har_rv_forecast_annualized_pct(&closes).unwrap();
        let trailing = realized_vol_annualized_pct(&closes, 20);
        assert!(after > before * 1.8, "before {before} after {after}");
        // The daily term makes HAR react faster than the 20-day trailing estimate here.
        assert!(after > trailing, "har {after} trailing {trailing}");
    }

    #[test]
    fn iv_rv_ratio_none_when_missing() {
        assert!(iv_rv_ratio(None, Some(15.0)).is_none());
        assert!(iv_rv_ratio(Some(20.0), None).is_none());
        assert!((iv_rv_ratio(Some(22.0), Some(20.0)).unwrap() - 1.1).abs() < 1e-9);
    }
}
