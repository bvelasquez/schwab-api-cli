//! Credit-spread analytics: POP, break-even, expected move, net theta.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Enriched spread metrics for TUI, LLM context, and entry filters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpreadAnalytics {
    pub is_put_spread: bool,
    pub underlying_price: f64,
    pub short_strike: f64,
    pub long_strike: f64,
    pub width: f64,
    pub credit: f64,
    pub dte: i64,
    pub chain_iv_pct: Option<f64>,
    pub short_delta: Option<f64>,
    pub long_delta: Option<f64>,
    pub short_theta: Option<f64>,
    pub long_theta: Option<f64>,
    /// Position theta $/day per spread (positive = decay helps seller).
    pub net_theta_per_day_usd: Option<f64>,
    pub short_otm_pct: Option<f64>,
    pub approx_short_otm_prob_pct: Option<f64>,
    pub break_even_price: Option<f64>,
    pub distance_to_be_usd: Option<f64>,
    pub distance_to_be_pct: Option<f64>,
    pub expected_move_1sigma_usd: Option<f64>,
    pub expected_move_1sigma_pct: Option<f64>,
    pub short_strike_inside_1sigma: Option<bool>,
    pub spread_pop_pct: Option<f64>,
    pub credit_to_width_pct: Option<f64>,
    pub max_loss_per_spread_usd: Option<f64>,
    pub risk_reward_ratio: Option<f64>,
    pub underlying_change_pct: Option<f64>,
    pub distance_to_short_strike_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct VerticalAnalyticsInput {
    pub is_put_spread: bool,
    pub underlying_price: f64,
    pub short_strike: f64,
    pub long_strike: f64,
    pub credit: f64,
    pub dte: i64,
    pub chain_iv_pct: Option<f64>,
    pub short_delta: Option<f64>,
    pub long_delta: Option<f64>,
    pub short_theta: Option<f64>,
    pub long_theta: Option<f64>,
    pub contracts: u32,
    pub underlying_change_pct: Option<f64>,
}

pub fn compute_vertical_analytics(input: VerticalAnalyticsInput) -> SpreadAnalytics {
    let width = (input.short_strike - input.long_strike).abs();
    let credit = input.credit.max(0.0);
    let contracts = input.contracts.max(1);

    let iv = input
        .chain_iv_pct
        .or_else(|| strike_iv_fallback(input.short_delta))
        .filter(|v| *v > 0.0);

    let (short_otm_pct, distance_to_be_usd, break_even) =
        if input.underlying_price > f64::EPSILON {
            if input.is_put_spread {
                let be = input.short_strike - credit;
                let dist = input.underlying_price - be;
                (
                    Some(((input.underlying_price - input.short_strike) / input.underlying_price)
                        * 100.0),
                    Some(dist),
                    Some(be),
                )
            } else {
                let be = input.short_strike + credit;
                let dist = be - input.underlying_price;
                (
                    Some(((input.short_strike - input.underlying_price) / input.underlying_price)
                        * 100.0),
                    Some(dist),
                    Some(be),
                )
            }
        } else {
            (None, None, None)
        };

    let distance_to_be_pct = break_even.zip(Some(input.underlying_price)).map(|(be, spot)| {
        if input.is_put_spread {
            ((spot - be) / spot) * 100.0
        } else {
            ((be - spot) / spot) * 100.0
        }
    });

    let (expected_move_1sigma_usd, expected_move_1sigma_pct) =
        iv.and_then(|iv_pct| {
            expected_move(input.underlying_price, iv_pct, input.dte)
        })
        .map(|em| (Some(em), Some((em / input.underlying_price) * 100.0)))
        .unwrap_or((None, None));

    let short_strike_inside_1sigma = expected_move_1sigma_usd.map(|em| {
        if input.is_put_spread {
            (input.underlying_price - input.short_strike) < em
        } else {
            (input.short_strike - input.underlying_price) < em
        }
    });

    let approx_short_otm_prob_pct = input.short_delta.map(|d| {
        if input.is_put_spread {
            (1.0 + d) * 100.0
        } else {
            (1.0 - d) * 100.0
        }
    });

    let distance_to_short_strike_usd = if input.underlying_price > f64::EPSILON {
        Some(if input.is_put_spread {
            input.underlying_price - input.short_strike
        } else {
            input.short_strike - input.underlying_price
        })
    } else {
        None
    };

    let spread_pop_pct = break_even.and_then(|be| {
        iv.and_then(|iv_pct| {
            probability_above_price(input.underlying_price, be, iv_pct, input.dte)
        })
    });

    let credit_to_width_pct = if width > f64::EPSILON {
        Some((credit / width) * 100.0)
    } else {
        None
    };

    let max_loss = ((width - credit).max(0.0)) * 100.0;
    let risk_reward_ratio = if max_loss > f64::EPSILON {
        Some((credit * 100.0) / max_loss)
    } else {
        None
    };

    let net_theta_per_day_usd = match (input.short_theta, input.long_theta) {
        (Some(st), Some(lt)) => {
            // Position theta: (-1)*short + (+1)*long per share; ×100 per contract.
            let per_share = lt - st;
            Some(per_share * 100.0 * contracts as f64)
        }
        _ => None,
    };

    SpreadAnalytics {
        is_put_spread: input.is_put_spread,
        underlying_price: input.underlying_price,
        short_strike: input.short_strike,
        long_strike: input.long_strike,
        width,
        credit,
        dte: input.dte,
        chain_iv_pct: iv,
        short_delta: input.short_delta,
        long_delta: input.long_delta,
        short_theta: input.short_theta,
        long_theta: input.long_theta,
        net_theta_per_day_usd,
        short_otm_pct,
        approx_short_otm_prob_pct,
        break_even_price: break_even,
        distance_to_be_usd,
        distance_to_be_pct,
        expected_move_1sigma_usd,
        expected_move_1sigma_pct,
        short_strike_inside_1sigma,
        spread_pop_pct,
        credit_to_width_pct,
        max_loss_per_spread_usd: Some(max_loss),
        risk_reward_ratio,
        underlying_change_pct: input.underlying_change_pct,
        distance_to_short_strike_usd,
    }
}

/// Composite 0–100 score for TUI win-chance meter.
pub fn spread_win_score(
    profit_pct: f64,
    analytics: &SpreadAnalytics,
    pct_cushion_from_stop: f64,
) -> f64 {
    let pop = analytics.spread_pop_pct.unwrap_or(50.0) / 100.0;
    let pnl = ((profit_pct + 30.0) / 80.0).clamp(0.0, 1.0);
    let cushion = (analytics.distance_to_be_pct.unwrap_or(0.0) / 15.0).clamp(0.0, 1.0);
    let stop_room = (pct_cushion_from_stop / 100.0).clamp(0.0, 1.0);
    let delta_comfort = analytics
        .short_delta
        .map(|d| (0.45 - d.abs()) / 0.35)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    (pop * 0.35 + pnl * 0.25 + cushion * 0.20 + stop_room * 0.10 + delta_comfort * 0.10) * 100.0
}

pub fn entry_analytics_pass(entry: &crate::rules::VerticalEntryRules, a: &SpreadAnalytics) -> bool {
    if let Some(min) = entry.min_pop_pct {
        if a.spread_pop_pct.unwrap_or(0.0) < min {
            return false;
        }
    }
    if let Some(min) = entry.min_distance_to_be_pct {
        if a.distance_to_be_pct.unwrap_or(0.0) < min {
            return false;
        }
    }
    let min_ctw = entry.min_credit_to_width_pct.unwrap_or(12.5);
    if a.credit_to_width_pct.unwrap_or(0.0) < min_ctw {
        return false;
    }
    true
}

pub fn analytics_to_json(a: &SpreadAnalytics) -> Value {
    serde_json::to_value(a).unwrap_or(json!({}))
}

pub fn analytics_from_json(v: &Value) -> Option<SpreadAnalytics> {
    serde_json::from_value(v.clone()).ok()
}

/// 1σ expected move in dollars (lognormal, IV as annualized decimal %).
pub fn expected_move(spot: f64, iv_pct: f64, dte: i64) -> Option<f64> {
    if spot <= 0.0 || iv_pct <= 0.0 || dte <= 0 {
        return None;
    }
    let iv = iv_pct / 100.0;
    let t = dte as f64 / 365.0;
    Some(spot * iv * t.sqrt())
}

/// P(S_T > price) at expiry under lognormal (risk-neutral, zero rates).
pub fn probability_above_price(spot: f64, price: f64, iv_pct: f64, dte: i64) -> Option<f64> {
    if spot <= 0.0 || price <= 0.0 || iv_pct <= 0.0 || dte <= 0 {
        return None;
    }
    let iv = iv_pct / 100.0;
    let t = dte as f64 / 365.0;
    let denom = iv * t.sqrt();
    if denom <= f64::EPSILON {
        return None;
    }
    let d = (spot / price).ln() / denom;
    Some(normal_cdf(d) * 100.0)
}

fn strike_iv_fallback(_delta: Option<f64>) -> Option<f64> {
    None
}

fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

fn erf(x: f64) -> f64 {
    // Abramowitz & Stegun approximation
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;
    let t = 1.0 / (1.0 + p * x);
    let y = 1.0
        - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();
    sign * y
}

/// Price rail for credit spreads: BE (left) → spot (●) → short strike.
pub fn price_cushion_rail(
    break_even: f64,
    spot: f64,
    short_strike: f64,
    is_put_spread: bool,
    width: usize,
) -> (String, f64) {
    let width = width.max(12);
    if is_put_spread {
        let lo = break_even.min(short_strike);
        let hi = short_strike.max(break_even).max(spot);
        let span = (hi - lo).max(0.01);
        let mut chars: Vec<char> = vec!['·'; width];
        let be_idx = ((break_even - lo) / span * (width.saturating_sub(1) as f64)).round() as usize;
        let short_idx =
            ((short_strike - lo) / span * (width.saturating_sub(1) as f64)).round() as usize;
        let spot_idx = ((spot.clamp(lo, hi) - lo) / span * (width.saturating_sub(1) as f64))
            .round() as usize;
        if be_idx < width {
            chars[be_idx] = 'B';
        }
        if short_idx < width && short_idx != be_idx {
            chars[short_idx] = 'S';
        }
        if spot_idx < width {
            chars[spot_idx] = '●';
        }
        let cushion_pct = ((spot - break_even) / span * 100.0).clamp(0.0, 200.0);
        (chars.into_iter().collect(), cushion_pct)
    } else {
        let lo = short_strike.min(break_even).min(spot);
        let hi = break_even.max(short_strike).max(spot);
        let span = (hi - lo).max(0.01);
        let mut chars: Vec<char> = vec!['·'; width];
        let be_idx = ((break_even - lo) / span * (width.saturating_sub(1) as f64)).round() as usize;
        let short_idx =
            ((short_strike - lo) / span * (width.saturating_sub(1) as f64)).round() as usize;
        let spot_idx = ((spot.clamp(lo, hi) - lo) / span * (width.saturating_sub(1) as f64))
            .round() as usize;
        if short_idx < width {
            chars[short_idx] = 'S';
        }
        if be_idx < width && be_idx != short_idx {
            chars[be_idx] = 'B';
        }
        if spot_idx < width {
            chars[spot_idx] = '●';
        }
        let cushion_pct = ((break_even - spot) / span * 100.0).clamp(0.0, 200.0);
        (chars.into_iter().collect(), cushion_pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_move_scales_with_sqrt_time() {
        let em30 = expected_move(300.0, 20.0, 30).unwrap();
        let em120 = expected_move(300.0, 20.0, 120).unwrap();
        assert!(em120 > em30);
    }

    #[test]
    fn put_credit_pop_above_break_even() {
        let pop = probability_above_price(300.0, 280.0, 25.0, 35).unwrap();
        assert!(pop > 60.0);
    }

    #[test]
    fn vertical_analytics_put_credit() {
        let a = compute_vertical_analytics(VerticalAnalyticsInput {
            is_put_spread: true,
            underlying_price: 299.0,
            short_strike: 282.0,
            long_strike: 280.0,
            credit: 0.25,
            dte: 36,
            chain_iv_pct: Some(28.0),
            short_delta: Some(-0.22),
            long_delta: Some(-0.15),
            short_theta: Some(-0.08),
            long_theta: Some(-0.05),
            contracts: 1,
            underlying_change_pct: Some(0.5),
        });
        assert!((a.break_even_price.unwrap() - 281.75).abs() < 0.01);
        assert!(a.spread_pop_pct.unwrap() > 55.0);
        assert!(a.distance_to_be_pct.unwrap() > 5.0);
        assert!(a.net_theta_per_day_usd.unwrap() > 0.0);
    }

    #[test]
    fn price_rail_marks_be_and_spot() {
        let (rail, _) = price_cushion_rail(281.75, 299.0, 282.0, true, 24);
        assert!(rail.contains('B'));
        assert!(rail.contains('●'));
    }
}
