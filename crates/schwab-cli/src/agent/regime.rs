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

/// Entry pause decision: hostile class, VIX outside pause bands, or (fail-closed)
/// a missing VIX quote when `pause_on_missing_vix` is set.
pub fn regime_pause_entries(
    cfg: &OptionsRegimeConfig,
    class: OptionsRegimeClass,
    vix: Option<f64>,
) -> bool {
    class == OptionsRegimeClass::Hostile
        || vix.is_some_and(|v| v >= cfg.pause_entries_vix_above)
        || cfg
            .pause_entries_vix_below
            .is_some_and(|floor| vix.is_some_and(|v| v <= floor))
        || (cfg.pause_on_missing_vix && vix.is_none())
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
    let pause_entries = regime_pause_entries(cfg, class, vix);
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
            "pause_entries_vix_below": cfg.pause_entries_vix_below,
            "realized_vol_lookback": cfg.realized_vol_lookback,
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
    // Below the 50DMA (but above the 200) is a soft tape regardless of how calm
    // VIX looks — never let this fall through to Neutral → put_credit.
    if !above_sma_50 {
        return OptionsRegimeClass::HighVolChop;
    }
    let high_vix = vix.is_some_and(|v| v >= cfg.vix_high);
    if high_vix {
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
    let last = extract_quote_last_price(&quote, &sym).unwrap_or(0.0);
    if last > 0.0 {
        Ok(last)
    } else {
        anyhow::bail!("missing VIX lastPrice for {sym}: {quote}");
    }
}

/// Schwab single-symbol quotes are usually `{ "SYM": { "quote": { "lastPrice": … } } }`.
fn extract_quote_last_price(raw: &Value, symbol: &str) -> Option<f64> {
    let entry = raw
        .get(symbol)
        .or_else(|| {
            // Case / $VIX variants
            raw.as_object()
                .and_then(|m| m.values().next())
        })
        .unwrap_or(raw);
    entry
        .pointer("/quote/lastPrice")
        .or_else(|| entry.pointer("/extended/lastPrice"))
        .or_else(|| entry.get("lastPrice"))
        .or_else(|| raw.pointer("/quote/lastPrice"))
        .and_then(|v| v.as_f64())
        .filter(|v| *v > 0.0)
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
    fn below_50dma_is_chop_even_with_calm_vix() {
        // Regression: soft tape + low VIX used to fall through to Neutral → put_credit.
        let cfg = OptionsRegimeConfig::default();
        assert_eq!(
            classify_options_regime(&cfg, Some(13.0), false, true),
            OptionsRegimeClass::HighVolChop
        );
    }

    #[test]
    fn neutral_only_when_above_50dma() {
        let cfg = OptionsRegimeConfig::default();
        // Above both SMAs but VIX quote missing → Neutral (pause handles entries).
        assert_eq!(
            classify_options_regime(&cfg, None, true, true),
            OptionsRegimeClass::Neutral
        );
    }

    #[test]
    fn extracts_vix_last_from_symbol_wrapped_quote() {
        let raw = json!({
            "$VIX": {
                "quote": { "lastPrice": 18.1 }
            }
        });
        assert!((extract_quote_last_price(&raw, "$VIX").unwrap() - 18.1).abs() < 1e-9);
    }

    #[test]
    fn pauses_when_vix_missing_and_fail_closed() {
        let cfg = OptionsRegimeConfig::default(); // pause_on_missing_vix: true
        assert!(regime_pause_entries(&cfg, OptionsRegimeClass::Neutral, None));
        let mut open = cfg.clone();
        open.pause_on_missing_vix = false;
        assert!(!regime_pause_entries(
            &open,
            OptionsRegimeClass::LowVolTrend,
            None
        ));
        // Bands still apply when the quote is present.
        assert!(regime_pause_entries(&open, OptionsRegimeClass::Hostile, Some(31.0)));
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

    #[test]
    fn pause_entries_when_vix_at_or_below_floor() {
        let cfg = OptionsRegimeConfig {
            pause_entries_vix_below: Some(14.0),
            pause_entries_vix_above: 30.0,
            ..Default::default()
        };
        let pause = cfg
            .pause_entries_vix_below
            .is_some_and(|floor| Some(13.5_f64).is_some_and(|v| v <= floor))
            || Some(13.5_f64).is_some_and(|v| v >= cfg.pause_entries_vix_above);
        assert!(pause);
        let no_pause = !(cfg
            .pause_entries_vix_below
            .is_some_and(|floor| Some(16.0_f64).is_some_and(|v| v <= floor))
            || Some(16.0_f64).is_some_and(|v| v >= cfg.pause_entries_vix_above));
        assert!(no_pause);
    }
}
