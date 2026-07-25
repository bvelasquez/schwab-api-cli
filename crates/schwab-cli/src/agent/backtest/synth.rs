//! Synthetic vertical / iron-condor candidates from BS + VIX IV.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

use crate::agent::spread_analytics::{
    compute_vertical_analytics, entry_analytics_pass, VerticalAnalyticsInput,
};
use crate::agent::volatility::realized_vol_annualized_pct;
use crate::rules::{RulesConfig, VerticalEntryRules};

use super::bs::{bs_delta, vertical_credit, years_from_dte};

#[derive(Debug, Clone)]
pub struct SynthVertical {
    pub underlying: String,
    pub expiry: NaiveDate,
    pub is_put: bool,
    pub short_strike: f64,
    pub long_strike: f64,
    pub credit: f64,
    pub dte: i64,
    pub short_delta: f64,
    pub long_delta: f64,
    pub iv_pct: f64,
    pub realized_vol_pct: f64,
    pub contracts: u32,
}

/// Next Friday on or after `from` with DTE in `[dte_min, dte_max]` relative to `today`.
pub fn pick_expiry(today: NaiveDate, dte_min: u32, dte_max: u32) -> Option<(NaiveDate, i64)> {
    let mut d = today + Duration::days(dte_min as i64);
    // Advance to Friday
    while d.weekday() != Weekday::Fri {
        d += Duration::days(1);
    }
    let end = today + Duration::days(dte_max as i64 + 7);
    while d <= end {
        let dte = (d - today).num_days();
        if dte >= dte_min as i64 && dte <= dte_max as i64 {
            return Some((d, dte));
        }
        d += Duration::days(7);
    }
    None
}

fn round_strike(spot: f64, strike: f64) -> f64 {
    // $1 grid for index ETFs in the typical range.
    if spot >= 50.0 {
        strike.round()
    } else {
        (strike * 2.0).round() / 2.0
    }
}

fn strike_candidates(spot: f64, is_put: bool) -> Vec<f64> {
    let mut out = Vec::new();
    let lo = (spot * 0.85).floor();
    let hi = (spot * 1.15).ceil();
    let mut s = lo;
    while s <= hi {
        out.push(s);
        s += 1.0;
    }
    if is_put {
        out.retain(|k| *k < spot);
    } else {
        out.retain(|k| *k > spot);
    }
    out
}

pub fn pick_vertical(
    underlying: &str,
    today: NaiveDate,
    spot: f64,
    iv_pct: f64,
    closes: &[f64],
    entry: &VerticalEntryRules,
    is_put: bool,
    contracts: u32,
) -> Option<SynthVertical> {
    let (expiry, dte) = pick_expiry(today, entry.dte_min, entry.dte_max)?;
    let t = years_from_dte(dte);
    let sigma = (iv_pct / 100.0).max(0.01);
    let target = ((entry.short_delta_min + entry.short_delta_max) / 2.0).clamp(0.05, 0.40);

    let mut best: Option<(f64, f64, f64)> = None; // short, |delta|-target, delta
    for strike in strike_candidates(spot, is_put) {
        let delta = bs_delta(is_put, spot, strike, t, 0.0, sigma);
        let abs = delta.abs();
        if abs < entry.short_delta_min || abs > entry.short_delta_max {
            continue;
        }
        let err = (abs - target).abs();
        match best {
            Some((_, e, _)) if err >= e => {}
            _ => best = Some((strike, err, delta)),
        }
    }
    let (short_strike, _, short_delta) = best?;
    let long_raw = if is_put {
        short_strike - entry.max_width
    } else {
        short_strike + entry.max_width
    };
    let long_strike = round_strike(spot, long_raw);
    if (short_strike - long_strike).abs() < entry.max_width * 0.4 {
        return None;
    }
    let credit = vertical_credit(is_put, spot, short_strike, long_strike, dte, iv_pct);
    if credit < entry.min_credit {
        return None;
    }
    let long_delta = bs_delta(is_put, spot, long_strike, t, 0.0, sigma);
    let lookback = 20usize;
    let rv = realized_vol_annualized_pct(closes, lookback);
    Some(SynthVertical {
        underlying: underlying.to_uppercase(),
        expiry,
        is_put,
        short_strike,
        long_strike,
        credit,
        dte,
        short_delta,
        long_delta,
        iv_pct,
        realized_vol_pct: rv,
        contracts: contracts.max(1),
    })
}

pub fn vertical_passes_entry_gates(
    rules: &RulesConfig,
    entry: &VerticalEntryRules,
    v: &SynthVertical,
    spot: f64,
) -> Result<(), String> {
    let analytics = compute_vertical_analytics(VerticalAnalyticsInput {
        is_put_spread: v.is_put,
        underlying_price: spot,
        short_strike: v.short_strike,
        long_strike: v.long_strike,
        credit: v.credit,
        dte: v.dte,
        chain_iv_pct: Some(v.iv_pct),
        realized_vol_pct: Some(v.realized_vol_pct).filter(|x| *x > 0.0),
        short_delta: Some(v.short_delta),
        long_delta: Some(v.long_delta),
        short_theta: None,
        long_theta: None,
        contracts: v.contracts,
        underlying_change_pct: None,
    });
    if !entry_analytics_pass(entry, &analytics) {
        return Err("entry_analytics".into());
    }
    if let Some(min_ratio) = entry.min_iv_rv_ratio {
        let ratio = analytics.iv_rv_ratio;
        if !crate::agent::spread_analytics::passes_min_iv_rv_ratio(Some(min_ratio), ratio) {
            return Err(format!(
                "iv_rv_ratio {:?}",
                ratio
            ));
        }
    }
    if entry.reject_short_inside_1sigma
        && analytics.short_strike_inside_1sigma == Some(true)
    {
        return Err("short_inside_1sigma".into());
    }
    // Thesis candidate gates (reuse exit helper when present).
    if let Some(reason) =
        crate::agent::exits::candidate_fails_thesis_gates(rules, &analytics)
    {
        return Err(reason.to_string());
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SynthCondor {
    pub underlying: String,
    pub expiry: NaiveDate,
    pub put_short: f64,
    pub put_long: f64,
    pub call_short: f64,
    pub call_long: f64,
    pub credit: f64,
    #[allow(dead_code)]
    pub dte: i64,
    pub iv_pct: f64,
    pub contracts: u32,
}

pub fn pick_iron_condor(
    underlying: &str,
    today: NaiveDate,
    spot: f64,
    iv_pct: f64,
    closes: &[f64],
    rules: &RulesConfig,
) -> Option<SynthCondor> {
    let ic = &rules.entry_rules.iron_condor;
    let put_entry = VerticalEntryRules {
        short_delta_min: (ic.short_delta - 0.03).max(0.05),
        short_delta_max: ic.short_delta + 0.03,
        max_width: ic.wing_width,
        min_credit: ic.min_credit / 2.0,
        dte_min: ic.dte_min,
        dte_max: ic.dte_max,
        min_iv_rv_ratio: ic.min_iv_rv_ratio,
        ..rules.entry_rules.vertical.clone()
    };
    let put = pick_vertical(
        underlying,
        today,
        spot,
        iv_pct,
        closes,
        &put_entry,
        true,
        ic.max_contracts_per_trade,
    )?;
    let call = pick_vertical(
        underlying,
        today,
        spot,
        iv_pct,
        closes,
        &put_entry,
        false,
        ic.max_contracts_per_trade,
    )?;
    if put.expiry != call.expiry {
        return None;
    }
    let credit = put.credit + call.credit;
    if credit < ic.min_credit {
        return None;
    }
    if let Some(min_ratio) = ic.min_iv_rv_ratio {
        let rv = put.realized_vol_pct;
        if rv <= 0.0 || iv_pct / rv < min_ratio {
            return None;
        }
    }
    Some(SynthCondor {
        underlying: underlying.to_uppercase(),
        expiry: put.expiry,
        put_short: put.short_strike,
        put_long: put.long_strike,
        call_short: call.short_strike,
        call_long: call.long_strike,
        credit,
        dte: put.dte,
        iv_pct,
        contracts: ic.max_contracts_per_trade.max(1),
    })
}
