//! Market regime detection for profile selection (benchmark trend + vol).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::market_ctx::MarketCtx;
use crate::rules::{RegimeConfig, TraderRules};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegimeClass {
    LowVolTrend,
    HighVolChop,
    ElevatedVol,
    Neutral,
}

impl RegimeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowVolTrend => "low_vol_trend",
            Self::HighVolChop => "high_vol_chop",
            Self::ElevatedVol => "elevated_vol",
            Self::Neutral => "neutral",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeSnapshot {
    pub class: String,
    pub benchmark_symbol: String,
    pub vix_symbol: String,
    pub benchmark_last: f64,
    pub vix: Option<f64>,
    pub above_sma_50: bool,
    pub above_sma_200: bool,
    pub realized_vol_annualized_pct: f64,
    pub realized_vol_percentile: f64,
    pub recommended_profile: String,
    pub signals: Value,
}

pub async fn detect_regime(market: &MarketCtx, rules: &TraderRules) -> Result<RegimeSnapshot> {
    let cfg = &rules.adaptation.regime;
    if !rules.adaptation.enabled || !cfg.enabled {
        return Ok(neutral_snapshot(cfg, &rules.adaptation.default_profile));
    }

    let benchmark = cfg.benchmark_symbol.trim().to_uppercase();
    let candles = market
        .daily_candles_with_config(&benchmark, "year", 1, "daily")
        .await?;
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let last = *closes.last().unwrap_or(&0.0);
    let sma_50 = sma(&closes, 50);
    let sma_200 = sma(&closes, 200);
    let above_sma_50 = sma_50.is_some_and(|s| last >= s);
    let above_sma_200 = sma_200.is_some_and(|s| last >= s);

    let realized_vol = realized_vol_annualized_pct(&closes, cfg.realized_vol_lookback);
    let realized_vol_percentile =
        realized_vol_percentile(&closes, cfg.realized_vol_lookback, cfg.realized_vol_history);

    let vix = fetch_vix(market, &cfg.vix_symbol).await.ok();

    let class = classify_regime(
        cfg,
        vix,
        above_sma_50,
        above_sma_200,
        realized_vol_percentile,
    );
    let recommended = rules
        .adaptation
        .profile_map
        .get(class.as_str())
        .cloned()
        .unwrap_or_else(|| rules.adaptation.default_profile.clone());

    Ok(RegimeSnapshot {
        class: class.as_str().to_string(),
        benchmark_symbol: benchmark,
        vix_symbol: cfg.vix_symbol.clone(),
        benchmark_last: last,
        vix,
        above_sma_50,
        above_sma_200,
        realized_vol_annualized_pct: realized_vol,
        realized_vol_percentile,
        recommended_profile: recommended,
        signals: json!({
            "vix_low": cfg.vix_low,
            "vix_high": cfg.vix_high,
            "realized_vol_high_percentile": cfg.realized_vol_high_percentile,
        }),
    })
}

fn neutral_snapshot(cfg: &RegimeConfig, default_profile: &str) -> RegimeSnapshot {
    RegimeSnapshot {
        class: RegimeClass::Neutral.as_str().to_string(),
        benchmark_symbol: cfg.benchmark_symbol.clone(),
        vix_symbol: cfg.vix_symbol.clone(),
        benchmark_last: 0.0,
        vix: None,
        above_sma_50: false,
        above_sma_200: false,
        realized_vol_annualized_pct: 0.0,
        realized_vol_percentile: 50.0,
        recommended_profile: default_profile.to_string(),
        signals: json!({}),
    }
}

pub fn classify_regime(
    cfg: &RegimeConfig,
    vix: Option<f64>,
    above_sma_50: bool,
    above_sma_200: bool,
    realized_vol_percentile: f64,
) -> RegimeClass {
    let high_vix = vix.is_some_and(|v| v >= cfg.vix_high);
    let low_vix = vix.is_some_and(|v| v <= cfg.vix_low);
    let high_realized = realized_vol_percentile >= cfg.realized_vol_high_percentile;

    if high_vix || (high_realized && !above_sma_50) {
        return RegimeClass::HighVolChop;
    }
    if high_realized || vix.is_some_and(|v| v > cfg.vix_low && v < cfg.vix_high) {
        return RegimeClass::ElevatedVol;
    }
    if low_vix && above_sma_50 && above_sma_200 {
        return RegimeClass::LowVolTrend;
    }
    RegimeClass::Neutral
}

async fn fetch_vix(market: &MarketCtx, symbol: &str) -> Result<f64> {
    let (last, _, _) = market.quote_last_bid_ask(symbol).await?;
    if last > 0.0 {
        Ok(last)
    } else {
        anyhow::bail!("missing VIX lastPrice")
    }
}

fn sma(values: &[f64], period: usize) -> Option<f64> {
    if values.len() < period || period == 0 {
        return None;
    }
    let slice = &values[values.len() - period..];
    Some(slice.iter().sum::<f64>() / period as f64)
}

fn realized_vol_annualized_pct(closes: &[f64], lookback: usize) -> f64 {
    if closes.len() <= lookback + 1 {
        return 0.0;
    }
    let mut returns = Vec::new();
    let start = closes.len().saturating_sub(lookback + 1);
    for i in start + 1..closes.len() {
        let prev = closes[i - 1];
        if prev > 0.0 {
            returns.push((closes[i] / prev - 1.0).ln());
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

fn realized_vol_percentile(closes: &[f64], lookback: usize, history: usize) -> f64 {
    if closes.len() <= lookback + 2 {
        return 50.0;
    }
    let window_end = closes.len().saturating_sub(lookback + 1);
    let start = window_end.saturating_sub(history);
    let mut samples = Vec::new();
    for i in start..window_end {
        if i + lookback + 1 > closes.len() {
            break;
        }
        let slice = &closes[i..=i + lookback];
        let vol = realized_vol_annualized_pct(slice, lookback);
        if vol > 0.0 {
            samples.push(vol);
        }
    }
    if samples.is_empty() {
        return 50.0;
    }
    let current = realized_vol_annualized_pct(closes, lookback);
    let below = samples.iter().filter(|v| **v <= current).count();
    below as f64 / samples.len() as f64 * 100.0
}

impl RegimeSnapshot {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RegimeConfig;

    #[test]
    fn low_vix_uptrend_is_low_vol_trend() {
        let cfg = RegimeConfig::default();
        let class = classify_regime(&cfg, Some(14.0), true, true, 30.0);
        assert_eq!(class, RegimeClass::LowVolTrend);
    }

    #[test]
    fn high_vix_is_high_vol_chop() {
        let cfg = RegimeConfig::default();
        let class = classify_regime(&cfg, Some(30.0), true, true, 40.0);
        assert_eq!(class, RegimeClass::HighVolChop);
    }

    #[test]
    fn elevated_realized_vol_without_trend() {
        let cfg = RegimeConfig::default();
        let class = classify_regime(&cfg, Some(18.0), false, false, 80.0);
        assert_eq!(class, RegimeClass::HighVolChop);
    }
}
