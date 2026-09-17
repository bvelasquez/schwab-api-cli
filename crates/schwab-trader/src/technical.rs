use anyhow::Result;
use serde_json::{json, Value};

use crate::history_features::{compute_history_features, HistoryFeatures};
use crate::market_ctx::MarketCtx;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_features: Option<HistoryFeatures>,
    /// Last reported earnings date from Schwab fundamentals (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_earnings_date: Option<String>,
    /// Heuristic next earnings ≈ last + 91d (advanced to future).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_next_earnings: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_until_estimated_earnings: Option<i64>,
    /// Always `heuristic` when estimate is present — not a confirmed calendar date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earnings_estimate_confidence: Option<String>,
}

pub async fn fetch_technical_snapshot(
    market: &MarketCtx,
    rules: &TraderRules,
    symbol: &str,
) -> Result<TechnicalSnapshot> {
    fetch_technical_snapshot_with_benchmark(market, rules, symbol, None).await
}

pub async fn fetch_technical_snapshot_with_benchmark(
    market: &MarketCtx,
    rules: &TraderRules,
    symbol: &str,
    benchmark_candles: Option<&[Candle]>,
) -> Result<TechnicalSnapshot> {
    let symbol = symbol.trim().to_uppercase();
    let (last, bid, ask) = market.quote_last_bid_ask(&symbol).await?;
    let spread_pct = match (bid, ask, last) {
        (Some(b), Some(a), l) if l > 0.0 => Some(((a - b) / l) * 100.0),
        _ => None,
    };

    let hist = rules.effective_history();
    let candles = market
        .daily_candles_with_config(
            &symbol,
            &hist.period_type,
            hist.period,
            &hist.frequency_type,
        )
        .await?;

    let bench_owned;
    let bench_for_features: Option<&[Candle]> = if let Some(b) = benchmark_candles {
        Some(b)
    } else {
        let bench_sym = rules.adaptation.regime.benchmark_symbol.trim();
        if bench_sym.is_empty() {
            None
        } else {
            bench_owned = market
                .daily_candles_with_config(bench_sym, "year", 1, "daily")
                .await
                .unwrap_or_default();
            if bench_owned.is_empty() {
                None
            } else {
                Some(bench_owned.as_slice())
            }
        }
    };

    let mut snap = build_technical_snapshot(
        &symbol,
        last,
        bid,
        ask,
        spread_pct,
        &candles,
        rules.is_intraday(),
    )?;
    if candles.len() >= 30 {
        snap.history_features = Some(compute_history_features(
            &candles,
            last,
            bench_for_features,
        ));
    }
    if rules.playbook.filters.no_trade_before_earnings_days > 0 {
        enrich_earnings_estimate(market, &mut snap).await;
    }
    Ok(snap)
}

async fn enrich_earnings_estimate(market: &MarketCtx, snap: &mut TechnicalSnapshot) {
    let Ok(fundamental) = market.quote_fundamental(&snap.symbol).await else {
        return;
    };
    let Some(last) = crate::earnings::parse_last_earnings_date(&fundamental) else {
        return;
    };
    let today = match market {
        MarketCtx::Replay { as_of, .. } => as_of.date_naive(),
        MarketCtx::Live { .. } => crate::earnings::today_et_naive(),
    };
    let est = crate::earnings::estimate_next_earnings(last, today);
    snap.last_earnings_date = Some(est.last_earnings_date.to_string());
    snap.estimated_next_earnings = Some(est.estimated_next_earnings.to_string());
    snap.days_until_estimated_earnings = Some(est.days_until_estimated);
    snap.earnings_estimate_confidence = Some(est.confidence.to_string());
}

pub fn build_technical_snapshot(
    symbol: &str,
    last: f64,
    bid: Option<f64>,
    ask: Option<f64>,
    spread_pct: Option<f64>,
    candles: &[Candle],
    intraday: bool,
) -> Result<TechnicalSnapshot> {
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let volumes: Vec<f64> = candles.iter().map(|c| c.volume).collect();
    let sma_9 = sma(&closes, 9);
    let sma_20 = sma(&closes, 20);
    let sma_50 = sma(&closes, 50);
    let rsi_14 = rsi(&closes, 14);
    let atr_14 = atr(candles, 14);
    let volume_sma_20 = sma(&volumes, 20);
    let relative_volume = candles.last().and_then(|last_bar| {
        volume_sma_20
            .filter(|avg| *avg > 0.0)
            .map(|avg| last_bar.volume / avg)
    });

    Ok(TechnicalSnapshot {
        symbol: symbol.to_string(),
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
        intraday,
        history_features: None,
        last_earnings_date: None,
        estimated_next_earnings: None,
        days_until_estimated_earnings: None,
        earnings_estimate_confidence: None,
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

    // ATR/horizon/recent-range exit caps need supporting data; do not fall back to fixed %.
    if atr_required_for_exit_caps(rules) {
        match snap.atr_14 {
            Some(atr) if atr > 0.0 => {}
            _ => return Some("missing atr_14 for ATR/horizon exit caps".into()),
        }
    }
    if recent_range_cap_required(rules) {
        let lookback = rules
            .playbook
            .exit
            .profit_target_recent_range_cap
            .lookback_days;
        match snap
            .history_features
            .as_ref()
            .and_then(|h| h.high_for_lookback(lookback))
        {
            Some(h) if h > 0.0 => {}
            _ => {
                return Some(format!(
                    "missing recent high ({lookback}d) for recent-range target cap"
                ))
            }
        }
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
    for period in &entry.require_below_sma {
        match *period {
            9 => {
                if snap.above_sma_9 == Some(true) {
                    return Some("above SMA 9 (pullback required)".into());
                }
            }
            20 => {
                if snap.above_sma_20 == Some(true) {
                    return Some("above SMA 20 (pullback required)".into());
                }
            }
            50 => {
                if snap.above_sma_50 == Some(true) {
                    return Some("above SMA 50 (pullback required)".into());
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

    if let Some(min_rs) = rules.playbook.filters.min_rs_vs_benchmark_30d {
        match snap
            .history_features
            .as_ref()
            .and_then(|h| h.rs_vs_benchmark_30d_pct)
        {
            Some(rs) if rs < min_rs => {
                return Some(format!(
                    "RS vs benchmark 30d {rs:.1}% below min {min_rs:.1}%"
                ));
            }
            None => return Some("missing rs_vs_benchmark_30d".into()),
            _ => {}
        }
    }

    if let Some(min_dist) = rules.playbook.filters.min_distance_from_52w_high_pct {
        match snap
            .history_features
            .as_ref()
            .and_then(|h| h.pct_from_52w_high)
        {
            Some(pct) if pct > -min_dist => {
                return Some(format!(
                    "within {min_dist:.1}% of 52w high ({pct:.1}% from high)"
                ));
            }
            None => return Some("missing pct_from_52w_high".into()),
            _ => {}
        }
    }

    if let Some(min_rr) = rules.playbook.filters.min_reward_risk.filter(|v| *v > 0.0) {
        // Use the stop the position would actually get (ATR-capped when
        // configured) so the R:R gate is consistent with live brackets.
        let stop_pct = crate::capital::effective_stop_loss_pct(snap.last, rules, snap.atr_14);
        if stop_pct <= 0.0 {
            return Some("stop_loss_pct must be > 0 for min_reward_risk".into());
        }
        let range = crate::capital::ExitRangeContext::from_history(
            rules,
            snap.history_features.as_ref(),
        );
        let target_pct =
            crate::capital::effective_profit_target_pct(snap.last, rules, snap.atr_14, range);
        let rr = target_pct / stop_pct;
        if rr + f64::EPSILON < min_rr {
            return Some(format!(
                "reward/risk {rr:.2} below min {min_rr:.2} (target {target_pct:.1}% / stop {stop_pct:.1}%)"
            ));
        }
    }

    if let Some(min_mult) = rules
        .playbook
        .filters
        .min_stop_atr_multiple
        .filter(|v| *v > 0.0)
    {
        let stop_pct = crate::capital::effective_stop_loss_pct(snap.last, rules, snap.atr_14);
        match snap.atr_14 {
            Some(atr) if atr > 0.0 && snap.last > 0.0 => {
                let atr_pct = (atr / snap.last) * 100.0;
                if atr_pct <= 0.0 {
                    return Some("ATR% is zero".into());
                }
                let mult = stop_pct / atr_pct;
                if mult + f64::EPSILON < min_mult {
                    return Some(format!(
                        "stop {stop_pct:.1}% is only {mult:.2}× ATR ({atr_pct:.1}%); need ≥{min_mult:.2}×"
                    ));
                }
            }
            _ => return Some("missing atr_14 for min_stop_atr_multiple".into()),
        }
    }

    let lead = rules.playbook.filters.no_trade_before_earnings_days;
    if lead > 0 {
        if let (Some(days), Some(conf)) = (
            snap.days_until_estimated_earnings,
            snap.earnings_estimate_confidence.as_deref(),
        ) {
            if days >= 0 && days <= lead as i64 {
                return Some(format!(
                    "within {lead}d of estimated earnings {} (last={}, confidence={})",
                    snap.estimated_next_earnings.as_deref().unwrap_or("?"),
                    snap.last_earnings_date.as_deref().unwrap_or("?"),
                    conf
                ));
            }
        }
        // Missing earnings data: fail-open (heuristic only; don't block all entries).
    }

    let _ = tech;
    None
}

/// True when exit geometry depends on ATR (caps must not silently fall back to fixed %).
pub fn atr_required_for_exit_caps(rules: &TraderRules) -> bool {
    let exit = &rules.playbook.exit;
    exit.profit_target_atr_cap.enabled
        || exit.profit_target_horizon_cap.enabled
        || exit.stop_loss_atr_cap.enabled
}

/// True when recent-range target cap is enabled (needs history high).
pub fn recent_range_cap_required(rules: &TraderRules) -> bool {
    rules.playbook.exit.profit_target_recent_range_cap.enabled
}

/// Shrink position size when price is in the soft zone below the 52w-high block threshold.
pub fn near_52w_high_size_scalar(rules: &TraderRules, snap: &TechnicalSnapshot) -> f64 {
    let filters = &rules.playbook.filters;
    let Some(scalar) = filters.near_52w_high_size_scalar else {
        return 1.0;
    };
    let Some(soft_zone) = filters.near_52w_high_soft_zone_pct else {
        return 1.0;
    };
    let min_dist = filters
        .min_distance_from_52w_high_pct
        .unwrap_or(soft_zone);
    let Some(pct) = snap
        .history_features
        .as_ref()
        .and_then(|h| h.pct_from_52w_high)
    else {
        return 1.0;
    };
    if pct <= -soft_zone {
        1.0
    } else if pct <= -min_dist {
        scalar
    } else {
        1.0
    }
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
    use crate::rules::TraderRules;

    #[test]
    fn sma_computes_tail() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((sma(&v, 3).unwrap() - 4.0).abs() < 0.01);
    }

    #[test]
    fn build_snapshot_from_candles() {
        let candles: Vec<Candle> = (0..60)
            .map(|i| Candle {
                close: 100.0 + i as f64,
                high: 101.0 + i as f64,
                low: 99.0 + i as f64,
                volume: 1_000_000.0,
            })
            .collect();
        let snap = build_technical_snapshot("TEST", 159.0, None, None, None, &candles, false)
            .unwrap();
        assert!(snap.sma_20.is_some());
        assert!(snap.rsi_14.is_some());
    }

    fn base_snap() -> TechnicalSnapshot {
        TechnicalSnapshot {
            symbol: "TEST".into(),
            last: 100.0,
            bid: Some(99.9),
            ask: Some(100.1),
            spread_pct: Some(0.2),
            sma_9: Some(99.0),
            sma_20: Some(98.0),
            sma_50: Some(95.0),
            rsi_14: Some(55.0),
            atr_14: Some(2.0),
            volume_sma_20: Some(2_000_000.0),
            relative_volume: Some(1.2),
            above_sma_9: Some(true),
            above_sma_20: Some(true),
            above_sma_50: Some(true),
            intraday: false,
            history_features: Some(HistoryFeatures {
                bars_available: 60,
                return_30d_pct: Some(5.0),
                return_90d_pct: Some(10.0),
                pct_from_52w_high: Some(-8.0),
                pct_from_52w_low: Some(20.0),
                high_20d: Some(104.0),
                low_20d: Some(95.0),
                high_60d: Some(108.0),
                low_60d: Some(90.0),
                high_90d: Some(110.0),
                low_90d: Some(88.0),
                range_60d_pct: Some(18.0),
                sma_200: Some(90.0),
                above_sma_200: Some(true),
                rs_vs_benchmark_30d_pct: Some(2.0),
                rs_vs_benchmark_90d_pct: Some(3.0),
            }),
            last_earnings_date: None,
            estimated_next_earnings: None,
            days_until_estimated_earnings: None,
            earnings_estimate_confidence: None,
        }
    }

    #[test]
    fn rejects_poor_reward_risk_when_atr_caps_target() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.exit.profit_target_pct = 8.0;
        rules.playbook.exit.stop_loss_pct = 5.0;
        rules.playbook.exit.profit_target_atr_cap.enabled = true;
        rules.playbook.exit.profit_target_atr_cap.atr_multiple = 2.5;
        rules.playbook.filters.min_reward_risk = Some(1.25);
        // ATR 1.2% → capped target 3.0% → RR 0.6
        let mut snap = base_snap();
        snap.atr_14 = Some(1.2);
        let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
        assert!(
            reason.as_deref().is_some_and(|r| r.contains("reward/risk")),
            "got {reason:?}"
        );
    }

    #[test]
    fn rejects_stop_inside_daily_atr_noise() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.exit.stop_loss_pct = 5.0;
        rules.playbook.filters.min_stop_atr_multiple = Some(1.5);
        // ATR 4% → stop is only 1.25× ATR
        let mut snap = base_snap();
        snap.atr_14 = Some(4.0);
        let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
        assert!(
            reason.as_deref().is_some_and(|r| r.contains("ATR")),
            "got {reason:?}"
        );
    }

    #[test]
    fn accepts_balanced_atr_geometry() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.exit.profit_target_pct = 8.0;
        rules.playbook.exit.stop_loss_pct = 5.0;
        rules.playbook.exit.profit_target_atr_cap.enabled = true;
        rules.playbook.exit.profit_target_atr_cap.atr_multiple = 2.5;
        rules.playbook.filters.min_reward_risk = Some(1.25);
        rules.playbook.filters.min_stop_atr_multiple = Some(1.5);
        // ATR 2.5% → target min(8, 6.25)=6.25 → RR 1.25; stop/ATR = 2.0
        let mut snap = base_snap();
        snap.atr_14 = Some(2.5);
        let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
        assert!(reason.is_none(), "got {reason:?}");
    }

    #[test]
    fn rejects_missing_atr_when_exit_caps_enabled() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.exit.profit_target_horizon_cap.enabled = true;
        let mut snap = base_snap();
        snap.atr_14 = None;
        let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("missing atr_14")),
            "got {reason:?}"
        );
    }

    #[test]
    fn require_below_sma_defaults_empty_and_inert() {
        let rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        assert!(rules.playbook.entry.require_below_sma.is_empty());
        let snap = base_snap();
        let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
        assert!(reason.is_none(), "got {reason:?}");
    }

    #[test]
    fn rejects_extension_above_required_sma() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.entry.require_below_sma = vec![9];
        let snap = base_snap(); // above_sma_9 = Some(true) → extended, not a pullback
        let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
        assert!(
            reason.as_deref().is_some_and(|r| r.contains("pullback required")),
            "got {reason:?}"
        );
    }

    #[test]
    fn accepts_shallow_pullback_below_sma9() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.entry.require_below_sma = vec![9];
        let mut snap = base_snap();
        snap.above_sma_9 = Some(false); // pullback into the 9-day MA, still above SMA20/50
        let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
        assert!(reason.is_none(), "got {reason:?}");
    }
}
