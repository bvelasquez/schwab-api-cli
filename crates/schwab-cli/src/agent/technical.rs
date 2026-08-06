//! Technical helpers for entry timing gates (RSI(2), SMA cross) shared by the
//! live agent and the synthetic backtest. All functions are pure over closes so
//! the backtest can reuse exactly the same math.

use anyhow::{Context, Result};
use schwab_market_data::MarketDataApi;

/// Simple moving average of the last `period` closes. `None` when insufficient data.
pub fn sma_value(closes: &[f64], period: usize) -> Option<f64> {
    if closes.len() < period || period == 0 {
        return None;
    }
    let slice = &closes[closes.len() - period..];
    Some(slice.iter().sum::<f64>() / period as f64)
}

/// 2-period RSI (simple averaging, no Wilder smoothing — standard for RSI(2)).
/// Returns 0..=100; `None` when fewer than 3 closes.
pub fn rsi2(closes: &[f64]) -> Option<f64> {
    rsi(closes, 2)
}

/// N-period RSI over simple gains/losses. `None` when insufficient data.
pub fn rsi(closes: &[f64], period: usize) -> Option<f64> {
    if closes.len() <= period || period == 0 {
        return None;
    }
    let start = closes.len() - period - 1;
    let mut gains = 0.0_f64;
    let mut losses = 0.0_f64;
    for i in start + 1..closes.len() {
        let chg = closes[i] - closes[i - 1];
        if chg >= 0.0 {
            gains += chg;
        } else {
            losses += -chg;
        }
    }
    if losses == 0.0 {
        return Some(100.0);
    }
    let rs = gains / losses;
    Some(100.0 - 100.0 / (1.0 + rs))
}

/// Daily closes for a symbol (1y daily bars). Reuses the same price-history shape
/// as `regime::benchmark_trend` and `volatility::fetch_realized_vol_pct`.
pub async fn fetch_daily_closes(market: &MarketDataApi, symbol: &str) -> Result<Vec<f64>> {
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
        .with_context(|| format!("price history for technicals {sym}"))?;
    let closes: Vec<f64> = history
        .get("candles")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("close").and_then(|v| v.as_f64()))
                .collect()
        })
        .unwrap_or_default();
    if closes.is_empty() {
        anyhow::bail!("no daily closes for {sym}");
    }
    Ok(closes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsi2_oversold_on_streak_of_losses() {
        // 5 straight down days → RSI(2) near 0
        let closes = vec![110.0, 109.0, 108.0, 107.0, 106.0, 105.0];
        let r = rsi2(&closes).unwrap();
        assert!(r < 5.0, "expected oversold RSI, got {r}");
    }

    #[test]
    fn rsi2_overbought_on_streak_of_gains() {
        let closes = vec![100.0, 101.0, 102.0, 103.0, 104.0, 105.0];
        let r = rsi2(&closes).unwrap();
        assert!(r > 95.0, "expected overbought RSI, got {r}");
    }

    #[test]
    fn rsi2_half_on_flat() {
        // Equal up/down moves → ~50
        let closes = vec![100.0, 101.0, 100.0, 101.0, 100.0, 101.0];
        let r = rsi2(&closes).unwrap();
        assert!((r - 50.0).abs() < 10.0, "expected ~50 RSI, got {r}");
    }

    #[test]
    fn rsi2_requires_enough_data() {
        assert!(rsi2(&[1.0, 2.0]).is_none());
    }

    #[test]
    fn sma_and_cross() {
        let closes = vec![10.0, 10.0, 12.0, 12.0, 14.0, 14.0];
        assert!((sma_value(&closes, 3).unwrap() - 13.3333).abs() < 0.01);
        assert!(sma_value(&closes, 3).is_some_and(|s| 14.0 >= s));
        assert!(!sma_value(&closes, 3).is_some_and(|s| 12.0 >= s));
        // Insufficient data → None (callers decide fail-open/fail-closed).
        assert!(sma_value(&[1.0], 50).is_none());
    }
}
