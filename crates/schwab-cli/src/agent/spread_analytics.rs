//! Credit-spread analytics: POP, break-even, expected move, net theta.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Enriched spread metrics for TUI, LLM context, and entry filters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpreadAnalytics {
    pub is_put_spread: bool,
    /// True when this analytics blob describes a 4-leg iron condor.
    #[serde(default)]
    pub is_iron_condor: bool,
    pub underlying_price: f64,
    pub short_strike: f64,
    pub long_strike: f64,
    pub width: f64,
    pub credit: f64,
    pub dte: i64,
    pub chain_iv_pct: Option<f64>,
    /// Annualized realized vol (%) of the underlying over the configured lookback.
    pub realized_vol_pct: Option<f64>,
    /// `chain_iv_pct / realized_vol_pct` when both are available.
    pub iv_rv_ratio: Option<f64>,
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
    // --- Iron condor wing fields (None for verticals) ---
    #[serde(default)]
    pub put_short: Option<f64>,
    #[serde(default)]
    pub put_long: Option<f64>,
    #[serde(default)]
    pub call_short: Option<f64>,
    #[serde(default)]
    pub call_long: Option<f64>,
    #[serde(default)]
    pub put_break_even_price: Option<f64>,
    #[serde(default)]
    pub call_break_even_price: Option<f64>,
    #[serde(default)]
    pub put_short_otm_pct: Option<f64>,
    #[serde(default)]
    pub call_short_otm_pct: Option<f64>,
    #[serde(default)]
    pub put_short_delta: Option<f64>,
    #[serde(default)]
    pub call_short_delta: Option<f64>,
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
    pub realized_vol_pct: Option<f64>,
    pub short_delta: Option<f64>,
    pub long_delta: Option<f64>,
    pub short_theta: Option<f64>,
    pub long_theta: Option<f64>,
    pub contracts: u32,
    pub underlying_change_pct: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct IronCondorAnalyticsInput {
    pub underlying_price: f64,
    pub put_short: f64,
    pub put_long: f64,
    pub call_short: f64,
    pub call_long: f64,
    pub credit: f64,
    pub dte: i64,
    pub chain_iv_pct: Option<f64>,
    pub realized_vol_pct: Option<f64>,
    pub put_short_delta: Option<f64>,
    pub put_long_delta: Option<f64>,
    pub call_short_delta: Option<f64>,
    pub call_long_delta: Option<f64>,
    pub put_short_theta: Option<f64>,
    pub put_long_theta: Option<f64>,
    pub call_short_theta: Option<f64>,
    pub call_long_theta: Option<f64>,
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

    let realized_vol_pct = input.realized_vol_pct.filter(|v| *v > 0.0);
    let iv_rv_ratio = match (iv, realized_vol_pct) {
        (Some(iv_pct), Some(rv)) if rv > 0.0 => Some(iv_pct / rv),
        _ => None,
    };

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
        is_iron_condor: false,
        underlying_price: input.underlying_price,
        short_strike: input.short_strike,
        long_strike: input.long_strike,
        width,
        credit,
        dte: input.dte,
        chain_iv_pct: iv,
        realized_vol_pct,
        iv_rv_ratio,
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
        put_short: None,
        put_long: None,
        call_short: None,
        call_long: None,
        put_break_even_price: None,
        call_break_even_price: None,
        put_short_otm_pct: None,
        call_short_otm_pct: None,
        put_short_delta: None,
        call_short_delta: None,
    }
}

/// Combined analytics for an iron condor (put credit + call credit wings).
pub fn compute_iron_condor_analytics(input: IronCondorAnalyticsInput) -> SpreadAnalytics {
    let put_width = (input.put_short - input.put_long).abs();
    let call_width = (input.call_long - input.call_short).abs();
    let width = put_width.max(call_width);
    let credit = input.credit.max(0.0);
    let contracts = input.contracts.max(1);
    let spot = input.underlying_price;

    let put = compute_vertical_analytics(VerticalAnalyticsInput {
        is_put_spread: true,
        underlying_price: spot,
        short_strike: input.put_short,
        long_strike: input.put_long,
        credit: 0.0,
        dte: input.dte,
        chain_iv_pct: input.chain_iv_pct,
        realized_vol_pct: input.realized_vol_pct,
        short_delta: input.put_short_delta,
        long_delta: input.put_long_delta,
        short_theta: input.put_short_theta,
        long_theta: input.put_long_theta,
        contracts,
        underlying_change_pct: input.underlying_change_pct,
    });
    let call = compute_vertical_analytics(VerticalAnalyticsInput {
        is_put_spread: false,
        underlying_price: spot,
        short_strike: input.call_short,
        long_strike: input.call_long,
        credit: 0.0,
        dte: input.dte,
        chain_iv_pct: input.chain_iv_pct,
        realized_vol_pct: input.realized_vol_pct,
        short_delta: input.call_short_delta,
        long_delta: input.call_long_delta,
        short_theta: input.call_short_theta,
        long_theta: input.call_long_theta,
        contracts,
        underlying_change_pct: input.underlying_change_pct,
    });

    // True IC breakevens use the full credit against each short wing.
    let put_be = input.put_short - credit;
    let call_be = input.call_short + credit;
    let put_otm = if spot > f64::EPSILON {
        Some(((spot - input.put_short) / spot) * 100.0)
    } else {
        None
    };
    let call_otm = if spot > f64::EPSILON {
        Some(((input.call_short - spot) / spot) * 100.0)
    } else {
        None
    };
    let short_otm_pct = match (put_otm, call_otm) {
        (Some(p), Some(c)) => Some(p.min(c)),
        (Some(p), None) => Some(p),
        (None, Some(c)) => Some(c),
        _ => None,
    };

    let dist_put = spot - put_be;
    let dist_call = call_be - spot;
    let (distance_to_be_usd, distance_to_be_pct) = if spot > f64::EPSILON {
        let dist = dist_put.min(dist_call);
        (Some(dist), Some((dist / spot) * 100.0))
    } else {
        (None, None)
    };

    let iv = put.chain_iv_pct.or(call.chain_iv_pct);
    let (expected_move_1sigma_usd, expected_move_1sigma_pct) = put
        .expected_move_1sigma_usd
        .or(call.expected_move_1sigma_usd)
        .map(|em| (Some(em), Some((em / spot.max(1.0)) * 100.0)))
        .unwrap_or((None, None));

    let short_strike_inside_1sigma = match (
        put.short_strike_inside_1sigma,
        call.short_strike_inside_1sigma,
    ) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };

    // P(put_be < S_T < call_be) ≈ P(S > put_be) − P(S > call_be)
    let spread_pop_pct = iv.and_then(|iv_pct| {
        let above_put = probability_above_price(spot, put_be, iv_pct, input.dte)?;
        let above_call = probability_above_price(spot, call_be, iv_pct, input.dte)?;
        Some((above_put - above_call).clamp(0.0, 100.0))
    });

    let short_delta = match (input.put_short_delta, input.call_short_delta) {
        (Some(p), Some(c)) => {
            if p.abs() >= c.abs() {
                Some(p)
            } else {
                Some(c)
            }
        }
        (Some(p), None) => Some(p),
        (None, Some(c)) => Some(c),
        _ => None,
    };

    let approx_short_otm_prob_pct = match (input.put_short_delta, input.call_short_delta) {
        (Some(pd), Some(cd)) => Some(((1.0 + pd) * (1.0 - cd) * 100.0).clamp(0.0, 100.0)),
        (Some(pd), None) => Some((1.0 + pd) * 100.0),
        (None, Some(cd)) => Some((1.0 - cd) * 100.0),
        _ => None,
    };

    let put_dist = (spot - input.put_short).abs();
    let call_dist = (input.call_short - spot).abs();
    let (near_short, near_long, near_is_put) = if put_dist <= call_dist {
        (input.put_short, input.put_long, true)
    } else {
        (input.call_short, input.call_long, false)
    };

    let net_theta_per_day_usd = match (
        put.net_theta_per_day_usd,
        call.net_theta_per_day_usd,
    ) {
        (Some(p), Some(c)) => Some(p + c),
        (Some(p), None) => Some(p),
        (None, Some(c)) => Some(c),
        _ => None,
    };

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

    SpreadAnalytics {
        is_put_spread: near_is_put,
        is_iron_condor: true,
        underlying_price: spot,
        short_strike: near_short,
        long_strike: near_long,
        width,
        credit,
        dte: input.dte,
        chain_iv_pct: iv,
        realized_vol_pct: put.realized_vol_pct.or(call.realized_vol_pct),
        iv_rv_ratio: put.iv_rv_ratio.or(call.iv_rv_ratio),
        short_delta,
        long_delta: if near_is_put {
            input.put_long_delta
        } else {
            input.call_long_delta
        },
        short_theta: if near_is_put {
            input.put_short_theta
        } else {
            input.call_short_theta
        },
        long_theta: if near_is_put {
            input.put_long_theta
        } else {
            input.call_long_theta
        },
        net_theta_per_day_usd,
        short_otm_pct,
        approx_short_otm_prob_pct,
        break_even_price: None,
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
        distance_to_short_strike_usd: Some(put_dist.min(call_dist)),
        put_short: Some(input.put_short),
        put_long: Some(input.put_long),
        call_short: Some(input.call_short),
        call_long: Some(input.call_long),
        put_break_even_price: Some(put_be),
        call_break_even_price: Some(call_be),
        put_short_otm_pct: put_otm,
        call_short_otm_pct: call_otm,
        put_short_delta: input.put_short_delta,
        call_short_delta: input.call_short_delta,
    }
}

/// Composite 0–100 path-strength score for credit spreads.
/// Weights probability / OTM cushion / delta over mark-to-market P&L — options are not stocks.
pub fn spread_win_score(
    profit_pct: f64,
    analytics: &SpreadAnalytics,
    pct_cushion_from_stop: f64,
) -> f64 {
    spread_win_score_with_iv(profit_pct, analytics, pct_cushion_from_stop, None)
}

/// Like [`spread_win_score`], with optional IV crush (negative pts = helping sellers).
pub fn spread_win_score_with_iv(
    profit_pct: f64,
    analytics: &SpreadAnalytics,
    pct_cushion_from_stop: f64,
    iv_change_pts: Option<f64>,
) -> f64 {
    let pop = analytics.spread_pop_pct.unwrap_or(50.0) / 100.0;
    // OTM cushion: ~8% OTM ≈ full score (far from short strike).
    let otm = (analytics.short_otm_pct.unwrap_or(0.0) / 8.0).clamp(0.0, 1.0);
    let be_cushion = (analytics.distance_to_be_pct.unwrap_or(0.0) / 12.0).clamp(0.0, 1.0);
    let delta_comfort = analytics
        .short_delta
        .map(|d| (0.40 - d.abs()) / 0.30)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    // Positive net theta helps sellers as time passes.
    let theta = analytics
        .net_theta_per_day_usd
        .map(|t| ((t + 0.5) / 3.0).clamp(0.0, 1.0))
        .unwrap_or(0.5);
    // MTM is secondary — temporary debit widenings should not dominate.
    let pnl = ((profit_pct + 40.0) / 100.0).clamp(0.0, 1.0);
    let stop_room = (pct_cushion_from_stop / 100.0).clamp(0.0, 1.0);
    // IV crush (current < entry) boosts; expansion hurts. ±5 pts ≈ full swing.
    let iv_edge = iv_change_pts
        .map(|chg| ((-chg) / 5.0).clamp(-1.0, 1.0))
        .map(|x| (x + 1.0) / 2.0)
        .unwrap_or(0.5);
    (pop * 0.25
        + otm * 0.25
        + be_cushion * 0.10
        + delta_comfort * 0.10
        + theta * 0.10
        + iv_edge * 0.08
        + pnl * 0.07
        + stop_room * 0.05)
        * 100.0
}

/// Live edge readout: theta carry, IV crush/expansion, pace to profit target, success %.
#[derive(Debug, Clone)]
pub struct PositionMomentum {
    /// Model + path-adjusted probability of finishing as a win (0–100).
    pub success_pct: f64,
    /// `iv_now - iv_entry` (negative = crush helping credit sellers).
    pub iv_change_pts: Option<f64>,
    /// One-line human summary for the card.
    pub summary: String,
}

pub struct PositionMomentumInput<'a> {
    pub analytics: &'a SpreadAnalytics,
    pub debit_to_close: f64,
    pub target_debit: f64,
    pub profit_target_pct: f64,
    pub profit_pct: f64,
    pub pct_toward_target: f64,
    pub pct_cushion_from_stop: f64,
    pub contracts: u32,
    pub entry_chain_iv_pct: Option<f64>,
    pub entry_pop_pct: Option<f64>,
    pub dte: i64,
    pub dte_close: u32,
}

pub fn compute_position_momentum(input: PositionMomentumInput<'_>) -> PositionMomentum {
    let a = input.analytics;
    let contracts = input.contracts.max(1) as f64;
    let theta = a.net_theta_per_day_usd;
    let iv_now = a.chain_iv_pct.filter(|v| *v > 0.0);
    let iv_entry = input.entry_chain_iv_pct.filter(|v| *v > 0.0);
    let iv_change = match (iv_now, iv_entry) {
        (Some(now), Some(entry)) => Some(now - entry),
        _ => None,
    };

    let days_to_target = match theta {
        Some(t) if t > 0.05 => {
            let remaining = (input.debit_to_close - input.target_debit).max(0.0);
            if remaining <= f64::EPSILON {
                Some(0.0)
            } else {
                // θ is $/day for the whole position; debit is per-share.
                let theta_per_share = t / (100.0 * contracts);
                if theta_per_share > f64::EPSILON {
                    Some((remaining / theta_per_share).clamp(0.0, 365.0))
                } else {
                    None
                }
            }
        }
        _ => None,
    };

    let live_pop = a.spread_pop_pct;
    let path = spread_win_score_with_iv(
        input.profit_pct,
        a,
        input.pct_cushion_from_stop,
        iv_change,
    );
    // Blend model POP (finish in profit zone) with live path health + target progress.
    let pop_n = live_pop.unwrap_or(55.0) / 100.0;
    let progress_n = (input.pct_toward_target.max(0.0) / 100.0).clamp(0.0, 1.2);
    let path_n = (path / 100.0).clamp(0.0, 1.0);
    let mut success = (pop_n * 0.55 + path_n * 0.30 + progress_n.min(1.0) * 0.15) * 100.0;
    // Soft penalty when DTE is inside the forced-close window or θ is against us.
    if input.dte <= input.dte_close as i64 {
        success *= 0.85;
    }
    if theta.is_some_and(|t| t < -0.25) {
        success *= 0.92;
    }
    if iv_change.is_some_and(|c| c > 2.0) {
        success *= 0.94;
    }
    let success_pct = success.clamp(5.0, 97.0);

    let mut drivers = Vec::new();
    match theta {
        Some(t) if t >= 0.75 => drivers.push("θ strong".into()),
        Some(t) if t >= 0.15 => drivers.push("θ helping".into()),
        Some(t) if t < -0.15 => drivers.push("θ against".into()),
        _ => {}
    }
    match iv_change {
        Some(c) if c <= -1.0 => drivers.push(format!("IV crush {c:+.1}")),
        Some(c) if c >= 1.0 => drivers.push(format!("IV up {c:+.1}")),
        Some(_) => drivers.push("IV flat".into()),
        None => {
            if iv_now.is_some() {
                drivers.push("IV live".into());
            }
        }
    }
    if let Some(otm) = a.short_otm_pct {
        if otm >= 6.0 {
            drivers.push("far OTM".into());
        } else if otm < 3.0 {
            drivers.push("near short".into());
        }
    }
    if input.pct_toward_target >= 80.0 {
        drivers.push("near 50% tgt".into());
    } else if input.profit_pct >= input.profit_target_pct * 0.5 {
        drivers.push("mark progress".into());
    }

    let theta_s = match theta {
        Some(t) => format!("θ ${t:+.2}/d"),
        None => "θ —".into(),
    };
    let pace_s = match days_to_target {
        Some(0.0) => "at/above tgt".into(),
        Some(d) if d < 1.0 => format!("~{:.0}h θ→tgt", d * 24.0),
        Some(d) => format!("~{d:.0}d θ→tgt"),
        None => String::new(),
    };
    let iv_s = match (iv_now, iv_change) {
        (Some(now), Some(chg)) => format!("IV {now:.0}% ({chg:+.1} vs entry)"),
        (Some(now), None) => format!("IV {now:.0}%"),
        _ => "IV —".into(),
    };
    let pop_s = match (live_pop, input.entry_pop_pct) {
        (Some(live), Some(entry)) => format!("POP {live:.0}% (entry {entry:.0}%)"),
        (Some(live), None) => format!("POP {live:.0}%"),
        _ => String::new(),
    };

    let mut parts = vec![theta_s, iv_s];
    if !pace_s.is_empty() {
        parts.push(pace_s);
    }
    if !pop_s.is_empty() {
        parts.push(pop_s);
    }
    if !drivers.is_empty() {
        parts.push(drivers.join(" · "));
    }
    let summary = parts.join("  ·  ");

    PositionMomentum {
        success_pct,
        iv_change_pts: iv_change,
        summary,
    }
}

pub fn entry_analytics_pass(entry: &crate::rules::VerticalEntryRules, a: &SpreadAnalytics) -> bool {
    // Delta band is mandatory. Fail closed when greeks are missing — never accept a
    // short whose |Δ| we cannot verify (OTM-% fallbacks used to sneak ~0.22Δ shorts in).
    match a.short_delta {
        Some(d) => {
            let abs = d.abs();
            if abs < entry.short_delta_min || abs > entry.short_delta_max {
                return false;
            }
        }
        None => return false,
    }
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
    if let Some(min_otm) = entry.min_short_otm_pct {
        if a.short_otm_pct.unwrap_or(0.0) < min_otm {
            return false;
        }
    }
    let min_ctw = entry.min_credit_to_width_pct.unwrap_or(12.5);
    if a.credit_to_width_pct.unwrap_or(0.0) < min_ctw {
        return false;
    }
    // Fail-closed: when the 1σ gate is on, missing IV (None) rejects — never silently pass.
    if entry.reject_short_inside_1sigma && a.short_strike_inside_1sigma != Some(false) {
        return false;
    }
    if let Some(min_ratio) = entry.min_iv_rv_ratio {
        match a.iv_rv_ratio {
            Some(ratio) if ratio >= min_ratio => {}
            _ => return false, // missing IV/RV or ratio too low — fail closed
        }
    }
    // Skip selling premium into an already-adverse day (puts on a selloff / calls on a rip).
    if let Some(max_adverse) = entry.max_adverse_day_change_pct {
        if let Some(chg) = a.underlying_change_pct {
            let adverse = if a.is_put_spread { -chg } else { chg };
            if adverse > max_adverse {
                return false;
            }
        }
    }
    true
}

/// Human-readable reason when [`entry_analytics_pass`] would return false.
pub fn entry_analytics_reject_reason(
    entry: &crate::rules::VerticalEntryRules,
    a: &SpreadAnalytics,
) -> Option<String> {
    match a.short_delta {
        Some(d) => {
            let abs = d.abs();
            if abs < entry.short_delta_min || abs > entry.short_delta_max {
                return Some(format!(
                    "short |delta| {abs:.3} outside {:.2}-{:.2}",
                    entry.short_delta_min, entry.short_delta_max
                ));
            }
        }
        None => return Some("missing short delta".into()),
    }
    if let Some(min) = entry.min_pop_pct {
        let pop = a.spread_pop_pct.unwrap_or(0.0);
        if pop < min {
            return Some(format!("POP {pop:.1}% below min {min:.1}%"));
        }
    }
    if let Some(min) = entry.min_distance_to_be_pct {
        let dist = a.distance_to_be_pct.unwrap_or(0.0);
        if dist < min {
            return Some(format!(
                "distance to B/E {dist:.2}% below min {min:.1}%"
            ));
        }
    }
    if let Some(min_otm) = entry.min_short_otm_pct {
        let otm = a.short_otm_pct.unwrap_or(0.0);
        if otm < min_otm {
            return Some(format!("short OTM {otm:.2}% below min {min_otm:.1}%"));
        }
    }
    let min_ctw = entry.min_credit_to_width_pct.unwrap_or(12.5);
    let ctw = a.credit_to_width_pct.unwrap_or(0.0);
    if ctw < min_ctw {
        return Some(format!(
            "credit/width {ctw:.1}% below min {min_ctw:.1}%"
        ));
    }
    if entry.reject_short_inside_1sigma && a.short_strike_inside_1sigma != Some(false) {
        return Some("short inside 1σ expected move".into());
    }
    if let Some(min_ratio) = entry.min_iv_rv_ratio {
        match a.iv_rv_ratio {
            Some(ratio) if ratio >= min_ratio => {}
            Some(ratio) => {
                return Some(format!(
                    "IV/RV ratio {ratio:.2} below min {min_ratio:.2}"
                ));
            }
            None => return Some("IV/RV ratio unavailable".into()),
        }
    }
    if let Some(max_adverse) = entry.max_adverse_day_change_pct {
        if let Some(chg) = a.underlying_change_pct {
            let adverse = if a.is_put_spread { -chg } else { chg };
            if adverse > max_adverse {
                return Some(format!(
                    "adverse day move {adverse:.2}% exceeds max {max_adverse:.1}%"
                ));
            }
        }
    }
    None
}

/// Shared IV/RV check for iron condors (and any caller with only the ratio threshold).
pub fn passes_min_iv_rv_ratio(min_ratio: Option<f64>, iv_rv: Option<f64>) -> bool {
    match min_ratio {
        None => true,
        Some(min) => iv_rv.is_some_and(|r| r >= min),
    }
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

/// Iron condor price rail: put BE (P) → spot (●) → call BE (C).
pub fn iron_condor_price_rail(
    put_be: f64,
    call_be: f64,
    spot: f64,
    width: usize,
) -> (String, f64) {
    let width = width.max(12);
    let lo = put_be.min(spot);
    let hi = call_be.max(spot);
    let span = (hi - lo).max(0.01);
    let mut chars: Vec<char> = vec!['·'; width];
    let idx = |price: f64| {
        ((price.clamp(lo, hi) - lo) / span * (width.saturating_sub(1) as f64)).round() as usize
    };
    let put_idx = idx(put_be);
    let call_idx = idx(call_be);
    let spot_idx = idx(spot);
    if put_idx < width {
        chars[put_idx] = 'P';
    }
    if call_idx < width {
        chars[call_idx] = 'C';
    }
    if spot_idx < width {
        chars[spot_idx] = '●';
    }
    let mid = (put_be + call_be) / 2.0;
    let cushion_pct = (1.0 - ((spot - mid).abs() / (span / 2.0)).clamp(0.0, 1.0)) * 100.0;
    (chars.into_iter().collect(), cushion_pct)
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
            realized_vol_pct: Some(20.0),
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

    #[test]
    fn entry_analytics_reject_inside_1sigma_when_enabled() {
        let mut entry = crate::rules::VerticalEntryRules::default();
        entry.min_pop_pct = Some(50.0);
        entry.min_distance_to_be_pct = Some(1.0);
        entry.min_credit_to_width_pct = Some(5.0);
        entry.reject_short_inside_1sigma = true;
        entry.short_delta_min = 0.10;
        entry.short_delta_max = 0.25;

        let mut a = SpreadAnalytics {
            spread_pop_pct: Some(70.0),
            distance_to_be_pct: Some(5.0),
            credit_to_width_pct: Some(15.0),
            short_strike_inside_1sigma: Some(true),
            short_delta: Some(-0.18),
            is_put_spread: true,
            ..Default::default()
        };
        assert!(!entry_analytics_pass(&entry, &a));

        a.short_strike_inside_1sigma = Some(false);
        assert!(entry_analytics_pass(&entry, &a));

        entry.reject_short_inside_1sigma = false;
        a.short_strike_inside_1sigma = Some(true);
        assert!(entry_analytics_pass(&entry, &a));
    }

    #[test]
    fn entry_analytics_1sigma_fails_closed_when_iv_missing() {
        let mut entry = crate::rules::VerticalEntryRules::default();
        entry.min_pop_pct = Some(50.0);
        entry.min_distance_to_be_pct = Some(1.0);
        entry.min_credit_to_width_pct = Some(5.0);
        entry.reject_short_inside_1sigma = true;
        entry.short_delta_min = 0.10;
        entry.short_delta_max = 0.25;

        let a = SpreadAnalytics {
            spread_pop_pct: Some(70.0),
            distance_to_be_pct: Some(5.0),
            credit_to_width_pct: Some(15.0),
            short_strike_inside_1sigma: None,
            short_delta: Some(-0.18),
            is_put_spread: true,
            ..Default::default()
        };
        assert!(!entry_analytics_pass(&entry, &a));
    }

    #[test]
    fn entry_analytics_iv_rv_gate() {
        let mut entry = crate::rules::VerticalEntryRules::default();
        entry.min_pop_pct = Some(50.0);
        entry.min_distance_to_be_pct = Some(1.0);
        entry.min_credit_to_width_pct = Some(5.0);
        entry.min_iv_rv_ratio = Some(1.15);
        entry.short_delta_min = 0.10;
        entry.short_delta_max = 0.25;
        entry.reject_short_inside_1sigma = false;

        let mut a = SpreadAnalytics {
            spread_pop_pct: Some(70.0),
            distance_to_be_pct: Some(5.0),
            credit_to_width_pct: Some(15.0),
            short_strike_inside_1sigma: Some(false),
            iv_rv_ratio: Some(1.05),
            short_delta: Some(-0.18),
            is_put_spread: true,
            ..Default::default()
        };
        assert!(!entry_analytics_pass(&entry, &a));

        a.iv_rv_ratio = Some(1.20);
        assert!(entry_analytics_pass(&entry, &a));

        a.iv_rv_ratio = None;
        assert!(!entry_analytics_pass(&entry, &a));
    }

    #[test]
    fn entry_analytics_rejects_hot_short_delta() {
        let mut entry = crate::rules::VerticalEntryRules::default();
        entry.short_delta_min = 0.10;
        entry.short_delta_max = 0.16;
        entry.min_pop_pct = Some(50.0);
        entry.min_distance_to_be_pct = Some(1.0);
        entry.min_credit_to_width_pct = Some(5.0);
        entry.reject_short_inside_1sigma = false;

        let mut a = SpreadAnalytics {
            spread_pop_pct: Some(77.0),
            distance_to_be_pct: Some(5.9),
            credit_to_width_pct: Some(15.6),
            short_strike_inside_1sigma: Some(true),
            short_delta: Some(-0.228),
            short_otm_pct: Some(5.79),
            underlying_change_pct: Some(-1.43),
            is_put_spread: true,
            ..Default::default()
        };
        // QQQ 655/650-style pick: delta above band.
        assert!(!entry_analytics_pass(&entry, &a));

        a.short_delta = Some(-0.14);
        assert!(entry_analytics_pass(&entry, &a));

        a.short_delta = None;
        assert!(!entry_analytics_pass(&entry, &a));
    }

    #[test]
    fn entry_analytics_rejects_adverse_day_and_thin_otm() {
        let mut entry = crate::rules::VerticalEntryRules::default();
        entry.short_delta_min = 0.10;
        entry.short_delta_max = 0.16;
        entry.min_pop_pct = Some(50.0);
        entry.min_distance_to_be_pct = Some(1.0);
        entry.min_credit_to_width_pct = Some(5.0);
        entry.min_short_otm_pct = Some(6.0);
        entry.max_adverse_day_change_pct = Some(1.0);
        entry.reject_short_inside_1sigma = false;

        let mut a = SpreadAnalytics {
            spread_pop_pct: Some(77.0),
            distance_to_be_pct: Some(6.5),
            credit_to_width_pct: Some(15.0),
            short_delta: Some(-0.14),
            short_otm_pct: Some(5.5),
            underlying_change_pct: Some(-0.2),
            is_put_spread: true,
            ..Default::default()
        };
        assert!(!entry_analytics_pass(&entry, &a)); // thin OTM

        a.short_otm_pct = Some(6.5);
        a.underlying_change_pct = Some(-1.43);
        assert!(!entry_analytics_pass(&entry, &a)); // adverse day

        a.underlying_change_pct = Some(-0.5);
        assert!(entry_analytics_pass(&entry, &a));
    }

    #[test]
    fn momentum_shows_iv_crush_and_theta_pace() {
        let a = compute_iron_condor_analytics(IronCondorAnalyticsInput {
            underlying_price: 370.0,
            put_short: 340.0,
            put_long: 335.0,
            call_short: 403.0,
            call_long: 408.0,
            credit: 0.62,
            dte: 35,
            chain_iv_pct: Some(27.0),
            realized_vol_pct: None,
            put_short_delta: Some(-0.13),
            put_long_delta: Some(-0.10),
            call_short_delta: Some(0.13),
            call_long_delta: Some(0.10),
            put_short_theta: Some(-0.08),
            put_long_theta: Some(-0.05),
            call_short_theta: Some(-0.08),
            call_long_theta: Some(-0.05),
            contracts: 1,
            underlying_change_pct: Some(-0.5),
        });
        let mom = compute_position_momentum(PositionMomentumInput {
            analytics: &a,
            debit_to_close: 0.50,
            target_debit: 0.31,
            profit_target_pct: 50.0,
            profit_pct: 19.0,
            pct_toward_target: 38.0,
            pct_cushion_from_stop: 80.0,
            contracts: 1,
            entry_chain_iv_pct: Some(29.0),
            entry_pop_pct: Some(72.0),
            dte: 35,
            dte_close: 21,
        });
        assert!(mom.iv_change_pts.unwrap() < 0.0);
        assert!(mom.success_pct > 50.0);
        assert!(mom.summary.contains("IV"));
        assert!(mom.summary.contains("θ"));
        assert!(
            mom.summary.contains("crush")
                || mom.summary.contains("θ helping")
                || mom.summary.contains("θ strong")
        );
    }
}
