//! Indicator math over daily bars. Short history returns nothing.

pub fn sma(values: &[f64], period: usize) -> Option<f64> {
    if period == 0 || values.len() < period {
        return None;
    }
    let slice = &values[values.len() - period..];
    Some(slice.iter().sum::<f64>() / period as f64)
}

/// EMA seeded with the SMA of the first `period` closes. Multiplier is `2/(n+1)`.
pub fn ema(closes: &[f64], period: usize) -> Option<f64> {
    if period == 0 || closes.len() < period {
        return None;
    }
    let mut acc = sma(&closes[..period], period)?;
    let k = 2.0 / (period as f64 + 1.0);
    for close in closes.iter().skip(period) {
        acc = close * k + acc * (1.0 - k);
    }
    Some(acc)
}

/// Wilder RSI. The seed is the simple average of the first `period` changes.
pub fn rsi(closes: &[f64], period: usize) -> Option<f64> {
    if period == 0 || closes.len() <= period {
        return None;
    }
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;
    for i in 1..=period {
        let diff = closes[i] - closes[i - 1];
        if diff >= 0.0 {
            avg_gain += diff;
        } else {
            avg_loss -= diff;
        }
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;
    for i in (period + 1)..closes.len() {
        let diff = closes[i] - closes[i - 1];
        let gain = diff.max(0.0);
        let loss = (-diff).max(0.0);
        avg_gain = (avg_gain * (period as f64 - 1.0) + gain) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + loss) / period as f64;
    }
    if avg_loss == 0.0 {
        return Some(100.0);
    }
    let rs = avg_gain / avg_loss;
    Some(100.0 - (100.0 / (1.0 + rs)))
}

/// SMA of the last `period` true ranges. Needs more than `period` bars.
pub fn atr(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Option<f64> {
    if period == 0 || close.len() <= period || high.len() != close.len() || low.len() != close.len() {
        return None;
    }
    let mut trs = Vec::with_capacity(close.len() - 1);
    for i in 1..close.len() {
        let tr = (high[i] - low[i])
            .max((high[i] - close[i - 1]).abs())
            .max((low[i] - close[i - 1]).abs());
        trs.push(tr);
    }
    sma(&trs, period)
}

pub fn spread_pct(bid: f64, ask: f64) -> Option<f64> {
    if bid <= 0.0 || ask <= 0.0 || ask < bid {
        return None;
    }
    let mid = (bid + ask) / 2.0;
    if mid <= 0.0 {
        return None;
    }
    Some((ask - bid) / mid * 100.0)
}

pub fn change_pct(previous: f64, last: f64) -> Option<f64> {
    if previous == 0.0 {
        return None;
    }
    Some((last - previous) / previous * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_ema_rsi_atr_match_hand_values_and_short_series_are_empty() {
        assert!(sma(&[1.0, 2.0], 3).is_none());
        assert!((sma(&[1.0, 2.0, 3.0, 4.0, 5.0], 3).unwrap() - 4.0).abs() < 1e-9);

        assert!(ema(&[1.0, 2.0], 3).is_none());
        // Seed (1+2+3)/3 = 2, k = 0.5, then 4 -> 3, then 5 -> 4.
        assert!((ema(&[1.0, 2.0, 3.0, 4.0, 5.0], 3).unwrap() - 4.0).abs() < 1e-9);

        assert!(rsi(&[10.0, 11.0, 12.0], 3).is_none());
        let rsi = rsi(&[10.0, 11.0, 12.0, 11.0, 12.0], 3).unwrap();
        assert!((rsi - 700.0 / 9.0).abs() < 1e-9);

        let high = [10.0, 12.0, 11.0, 10.0];
        let low = [8.0, 9.0, 7.0, 8.0];
        let close = [9.0, 11.0, 8.0, 9.0];
        assert!(atr(&high, &low, &close, 14).is_none());
        // True ranges 3, 4, 2. Last two average 3.
        assert!((atr(&high, &low, &close, 2).unwrap() - 3.0).abs() < 1e-9);

        assert!((change_pct(100.0, 110.0).unwrap() - 10.0).abs() < 1e-9);
        let spread = spread_pct(100.0, 100.2).unwrap();
        assert!((spread - (0.2 / 100.1 * 100.0)).abs() < 1e-9);
    }
}
