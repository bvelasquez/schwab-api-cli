//! Lightweight market-regime detection for options strategy selection.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use schwab_market_data::MarketDataApi;

use crate::rules::OptionsRegimeConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionsRegimeClass {
    LowVolTrend,
    ElevatedVol,
    HighVolChop,
    BearishTrend,
    Hostile,
    Neutral,
}

impl OptionsRegimeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowVolTrend => "low_vol_trend",
            Self::ElevatedVol => "elevated_vol",
            Self::HighVolChop => "high_vol_chop",
            Self::BearishTrend => "bearish_trend",
            Self::Hostile => "hostile",
            Self::Neutral => "neutral",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionsRegimeSnapshot {
    pub class: String,
    pub benchmark_symbol: String,
    pub vix_symbol: String,
    pub benchmark_last: f64,
    pub vix: Option<f64>,
    pub above_sma_50: bool,
    pub above_sma_200: bool,
    pub preferred_strategy: String,
    pub pause_entries: bool,
    pub signals: Value,
}

impl OptionsRegimeSnapshot {
    pub fn to_json(&self) -> Value {
        json!({
            "class": self.class,
            "benchmark_symbol": self.benchmark_symbol,
            "vix_symbol": self.vix_symbol,
            "benchmark_last": self.benchmark_last,
            "vix": self.vix,
            "above_sma_50": self.above_sma_50,
            "above_sma_200": self.above_sma_200,
            "preferred_strategy": self.preferred_strategy,
            "pause_entries": self.pause_entries,
            "signals": self.signals,
        })
    }
}

pub async fn detect_options_regime(
    market: &MarketDataApi,
    cfg: &OptionsRegimeConfig,
) -> Result<OptionsRegimeSnapshot> {
    if !cfg.enabled {
        return Ok(neutral_snapshot(cfg));
    }

    let benchmark = cfg.benchmark_symbol.trim().to_uppercase();
    let (last, above_sma_50, above_sma_200) = benchmark_trend(market, &benchmark).await?;
    let vix = fetch_vix(market, &cfg.vix_symbol).await.ok();

    let class = classify_options_regime(cfg, vix, above_sma_50, above_sma_200);
    let pause_entries = class == OptionsRegimeClass::Hostile
        || vix.is_some_and(|v| v >= cfg.pause_entries_vix_above);
    let preferred = preferred_strategy(cfg, class);

    Ok(OptionsRegimeSnapshot {
        class: class.as_str().to_string(),
        benchmark_symbol: benchmark,
        vix_symbol: cfg.vix_symbol.clone(),
        benchmark_last: last,
        vix,
        above_sma_50,
        above_sma_200,
        preferred_strategy: preferred,
        pause_entries,
        signals: json!({
            "vix_low": cfg.vix_low,
            "vix_high": cfg.vix_high,
            "pause_entries_vix_above": cfg.pause_entries_vix_above,
        }),
    })
}

pub fn classify_options_regime(
    cfg: &OptionsRegimeConfig,
    vix: Option<f64>,
    above_sma_50: bool,
    above_sma_200: bool,
) -> OptionsRegimeClass {
    if vix.is_some_and(|v| v >= cfg.pause_entries_vix_above) {
        return OptionsRegimeClass::Hostile;
    }
    if !above_sma_50 && !above_sma_200 {
        return OptionsRegimeClass::BearishTrend;
    }
    let high_vix = vix.is_some_and(|v| v >= cfg.vix_high);
    if high_vix || (!above_sma_50 && vix.is_some_and(|v| v > cfg.vix_low)) {
        return OptionsRegimeClass::HighVolChop;
    }
    if vix.is_some_and(|v| v > cfg.vix_low && v < cfg.vix_high) {
        return OptionsRegimeClass::ElevatedVol;
    }
    if vix.is_some_and(|v| v <= cfg.vix_low) && above_sma_50 && above_sma_200 {
        return OptionsRegimeClass::LowVolTrend;
    }
    OptionsRegimeClass::Neutral
}

fn preferred_strategy(cfg: &OptionsRegimeConfig, class: OptionsRegimeClass) -> String {
    cfg.strategy_map
        .get(class.as_str())
        .cloned()
        .unwrap_or_else(|| match class {
            OptionsRegimeClass::BearishTrend => "call_credit".into(),
            OptionsRegimeClass::HighVolChop => "iron_condor".into(),
            OptionsRegimeClass::Hostile => "pause".into(),
            _ => "put_credit".into(),
        })
}

fn neutral_snapshot(cfg: &OptionsRegimeConfig) -> OptionsRegimeSnapshot {
    OptionsRegimeSnapshot {
        class: OptionsRegimeClass::Neutral.as_str().to_string(),
        benchmark_symbol: cfg.benchmark_symbol.clone(),
        vix_symbol: cfg.vix_symbol.clone(),
        benchmark_last: 0.0,
        vix: None,
        above_sma_50: true,
        above_sma_200: true,
        preferred_strategy: "put_credit".into(),
        pause_entries: false,
        signals: json!({}),
    }
}

async fn fetch_vix(market: &MarketDataApi, symbol: &str) -> Result<f64> {
    let sym = symbol.trim().to_uppercase();
    let quote = market
        .quotes()
        .get_quote(&sym, Some("quote"), None)
        .await
        .with_context(|| format!("VIX quote for {sym}"))?;
    let last = quote
        .pointer("/quote/lastPrice")
        .or_else(|| quote.pointer("/lastPrice"))
        .or_else(|| quote.get("lastPrice"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if last > 0.0 {
        Ok(last)
    } else {
        anyhow::bail!("missing VIX lastPrice")
    }
}

async fn benchmark_trend(market: &MarketDataApi, symbol: &str) -> Result<(f64, bool, bool)> {
    let history = market
        .price_history()
        .get(
            symbol,
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
        .with_context(|| format!("price history for {symbol}"))?;

    let closes: Vec<f64> = history
        .get("candles")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("close").and_then(|v| v.as_f64()))
                .collect()
        })
        .unwrap_or_default();

    let last = *closes.last().unwrap_or(&0.0);
    let sma_50 = sma(&closes, 50);
    let sma_200 = sma(&closes, 200);
    Ok((
        last,
        sma_50.is_some_and(|s| last >= s),
        sma_200.is_some_and(|s| last >= s),
    ))
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
    use crate::rules::OptionsRegimeConfig;

    #[test]
    fn bearish_when_below_both_smas() {
        let cfg = OptionsRegimeConfig::default();
        assert_eq!(
            classify_options_regime(&cfg, Some(18.0), false, false),
            OptionsRegimeClass::BearishTrend
        );
    }

    #[test]
    fn hostile_when_vix_extreme() {
        let cfg = OptionsRegimeConfig {
            pause_entries_vix_above: 30.0,
            ..Default::default()
        };
        assert_eq!(
            classify_options_regime(&cfg, Some(32.0), true, true),
            OptionsRegimeClass::Hostile
        );
    }

    #[test]
    fn chop_on_high_vix_with_trend() {
        let cfg = OptionsRegimeConfig {
            vix_high: 28.0,
            pause_entries_vix_above: 35.0,
            ..Default::default()
        };
        assert_eq!(
            classify_options_regime(&cfg, Some(29.0), true, true),
            OptionsRegimeClass::HighVolChop
        );
    }
}
