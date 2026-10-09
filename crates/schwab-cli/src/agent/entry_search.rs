//! Search every expiry, short strike, and width that the rules allow, then
//! keep the candidate with the best credit/width (POP breaks ties).
//!
//! The previous picker took the first expiry and the strike nearest the middle
//! of the delta band. A neighbor in the same band often cleared the credit
//! floor when that one strike did not.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde_json::Value;

use crate::options::{days_to_expiry, parse_expiry};
use crate::rules::VerticalEntryRules;

use super::chains_util::{atm_implied_vol_pct, contract_at_strike};
use super::spread_analytics::{
    compute_vertical_analytics, entry_analytics_reject_code, SpreadAnalytics, VerticalAnalyticsInput,
};

#[derive(Debug, Clone, Default)]
pub struct EntryFunnelCounts {
    pub candidates_seen: u32,
    pub admitted: u32,
    pub rejects: BTreeMap<String, u32>,
}

impl EntryFunnelCounts {
    pub fn bump(&mut self, gate: &str) {
        self.candidates_seen = self.candidates_seen.saturating_add(1);
        *self.rejects.entry(gate.to_string()).or_insert(0) += 1;
    }

    pub fn merge(&mut self, other: &EntryFunnelCounts) {
        self.candidates_seen = self.candidates_seen.saturating_add(other.candidates_seen);
        self.admitted = self.admitted.saturating_add(other.admitted);
        for (gate, n) in &other.rejects {
            *self.rejects.entry(gate.clone()).or_insert(0) += n;
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChosenVertical {
    pub expiry: NaiveDate,
    pub short_strike: f64,
    pub long_strike: f64,
    pub width: f64,
    pub credit: f64,
    pub credit_to_width_pct: f64,
    pub pop_pct: f64,
}

#[derive(Debug, Clone)]
pub struct VerticalSearch {
    pub best: Option<ChosenVertical>,
    pub funnel: EntryFunnelCounts,
}

pub fn widths_to_try(entry: &VerticalEntryRules) -> Vec<f64> {
    let listed: Vec<f64> = if entry.widths.is_empty() {
        vec![entry.max_width]
    } else {
        entry
            .widths
            .iter()
            .copied()
            .filter(|w| *w > 0.0 && *w <= entry.max_width + 0.011)
            .collect()
    };
    if listed.is_empty() {
        vec![entry.max_width]
    } else {
        listed
    }
}

pub fn expiries_in_window(
    chain: &Value,
    map_key: &str,
    dte_min: u32,
    dte_max: u32,
    today: NaiveDate,
) -> Vec<(NaiveDate, Value)> {
    let Some(map) = chain.get(map_key).and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, strikes) in map {
        let date_part = key.split(':').next().unwrap_or(key);
        let Ok(expiry) = parse_expiry(date_part) else {
            continue;
        };
        let dte = days_to_expiry(expiry, today);
        if dte >= dte_min as i64 && dte <= dte_max as i64 {
            out.push((expiry, strikes.clone()));
        }
    }
    out.sort_by_key(|(expiry, _)| *expiry);
    out
}

pub fn funnel_skip_reason(funnel: &EntryFunnelCounts) -> String {
    let top = funnel
        .rejects
        .iter()
        .max_by_key(|(_, n)| *n)
        .map(|(gate, n)| format!("{gate} rejected {n}"));
    match top {
        Some(detail) => format!(
            "no qualifying spread ({detail} of {} candidates)",
            funnel.candidates_seen
        ),
        None => "no qualifying spread".into(),
    }
}

/// Rank spreads on one chain. `thesis` returns a gate id when the candidate
/// would already fail an exit thesis check.
pub fn search_verticals(
    expiries: &[(NaiveDate, Value)],
    entry: &VerticalEntryRules,
    is_put: bool,
    spot: f64,
    today: NaiveDate,
    realized_vol_pct: Option<f64>,
    underlying_change_pct: Option<f64>,
    mut thesis: impl FnMut(&SpreadAnalytics) -> Option<&'static str>,
) -> VerticalSearch {
    let mut funnel = EntryFunnelCounts::default();
    if expiries.is_empty() {
        funnel.bump("no_expiry");
        return VerticalSearch { best: None, funnel };
    }
    let widths = widths_to_try(entry);
    let mut best: Option<ChosenVertical> = None;
    for (expiry, strike_map) in expiries {
        let shorts = strikes_in_delta_band(strike_map, entry.short_delta_min, entry.short_delta_max, is_put);
        if shorts.is_empty() {
            funnel.bump("no_short");
            continue;
        }
        let dte = days_to_expiry(*expiry, today);
        let atm_iv = atm_implied_vol_pct(strike_map, spot);
        for short in shorts {
            let short_iv = contract_field(strike_map, short, "volatility");
            for width in &widths {
                match consider_candidate(
                    entry,
                    strike_map,
                    *expiry,
                    dte,
                    short,
                    *width,
                    is_put,
                    spot,
                    atm_iv,
                    short_iv,
                    realized_vol_pct,
                    underlying_change_pct,
                    &mut thesis,
                ) {
                    Ok(chosen) => {
                        funnel.candidates_seen = funnel.candidates_seen.saturating_add(1);
                        let replace = match &best {
                            None => true,
                            Some(current) => {
                                chosen.credit_to_width_pct > current.credit_to_width_pct + 1e-9
                                    || ((chosen.credit_to_width_pct - current.credit_to_width_pct)
                                        .abs()
                                        <= 1e-9
                                        && chosen.pop_pct > current.pop_pct)
                            }
                        };
                        if replace {
                            if best.is_some() {
                                funnel.bump_only("ranked_lower");
                            }
                            best = Some(chosen);
                        } else {
                            funnel.bump_only("ranked_lower");
                        }
                    }
                    Err(gate) => funnel.bump(gate),
                }
            }
        }
    }
    if best.is_some() {
        funnel.admitted = 1;
    }
    VerticalSearch { best, funnel }
}

impl EntryFunnelCounts {
    fn bump_only(&mut self, gate: &str) {
        *self.rejects.entry(gate.to_string()).or_insert(0) += 1;
    }
}

fn consider_candidate(
    entry: &VerticalEntryRules,
    strike_map: &Value,
    expiry: NaiveDate,
    dte: i64,
    short: f64,
    width: f64,
    is_put: bool,
    spot: f64,
    atm_iv: Option<f64>,
    short_iv: Option<f64>,
    realized_vol_pct: Option<f64>,
    underlying_change_pct: Option<f64>,
    thesis: &mut impl FnMut(&SpreadAnalytics) -> Option<&'static str>,
) -> Result<ChosenVertical, &'static str> {
    let long = pick_wing(strike_map, short, width, is_put).ok_or("wing")?;
    let actual_width = (short - long).abs();
    if actual_width < width * 0.5 {
        return Err("wing");
    }
    let credit = spread_credit(strike_map, short, long).ok_or("quote")?;
    if credit < entry.min_credit {
        return Err("credit");
    }
    let ctw = (credit / actual_width) * 100.0;
    let min_ctw = entry.min_credit_to_width_pct.unwrap_or(12.5);
    if ctw < min_ctw {
        return Err("credit_width");
    }
    if quote_width_sum(strike_map, short, long) > credit * QUOTE_WIDTH_TO_CREDIT {
        return Err("quote_width");
    }
    let analytics = compute_vertical_analytics(VerticalAnalyticsInput {
        is_put_spread: is_put,
        underlying_price: spot,
        short_strike: short,
        long_strike: long,
        credit,
        dte,
        chain_iv_pct: atm_iv.or(short_iv),
        pop_iv_pct: short_iv,
        realized_vol_pct,
        short_delta: contract_field(strike_map, short, "delta"),
        long_delta: contract_field(strike_map, long, "delta"),
        short_theta: contract_field(strike_map, short, "theta"),
        long_theta: contract_field(strike_map, long, "theta"),
        contracts: entry.max_contracts_per_trade.max(1),
        underlying_change_pct,
    });
    if let Some(code) = entry_analytics_reject_code(entry, &analytics) {
        return Err(code);
    }
    if let Some(code) = thesis(&analytics) {
        return Err(code);
    }
    Ok(ChosenVertical {
        expiry,
        short_strike: short,
        long_strike: long,
        width: actual_width,
        credit,
        credit_to_width_pct: ctw,
        pop_pct: analytics.spread_pop_pct.unwrap_or(0.0),
    })
}

const QUOTE_WIDTH_TO_CREDIT: f64 = 1.0;

fn strikes_in_delta_band(strike_map: &Value, min: f64, max: f64, puts: bool) -> Vec<f64> {
    let Some(obj) = strike_map.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, contracts) in obj {
        let Ok(strike) = key.parse::<f64>() else {
            continue;
        };
        let Some(delta) = contracts
            .as_array()
            .and_then(|a| a.first())
            .and_then(|c| c.get("delta"))
            .and_then(|v| v.as_f64())
        else {
            continue;
        };
        if puts && delta > 0.0 {
            continue;
        }
        if !puts && delta < 0.0 {
            continue;
        }
        let abs = delta.abs();
        if abs >= min && abs <= max {
            out.push(strike);
        }
    }
    out
}

fn pick_wing(strike_map: &Value, short_strike: f64, width: f64, puts: bool) -> Option<f64> {
    let target = if puts {
        short_strike - width
    } else {
        short_strike + width
    };
    let obj = strike_map.as_object()?;
    let candidates: Vec<f64> = obj
        .keys()
        .filter_map(|k| k.parse::<f64>().ok())
        .filter(|s| {
            if puts {
                *s < short_strike - f64::EPSILON
            } else {
                *s > short_strike + f64::EPSILON
            }
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if let Some(exact) = candidates.iter().copied().find(|s| (*s - target).abs() < 0.011) {
        return Some(exact);
    }
    let within: Vec<f64> = candidates
        .iter()
        .copied()
        .filter(|s| {
            let w = if puts {
                short_strike - *s
            } else {
                *s - short_strike
            };
            w <= width + 0.011
        })
        .collect();
    let pool = if within.is_empty() { candidates } else { within };
    pool.into_iter()
        .min_by(|a, b| (*a - target).abs().partial_cmp(&(*b - target).abs()).unwrap())
}

fn spread_credit(strike_map: &Value, short: f64, long: f64) -> Option<f64> {
    let short_bid = leg_price(strike_map, short, "bid")?;
    let long_ask = leg_price(strike_map, long, "ask")?;
    let credit = short_bid - long_ask;
    if credit > 0.0 {
        Some(credit)
    } else {
        None
    }
}

fn leg_price(strike_map: &Value, strike: f64, field: &str) -> Option<f64> {
    let contract = contract_at_strike(strike_map, strike)?;
    positive_field(contract, field).or_else(|| positive_field(contract, "mark"))
}

fn quote_width_sum(strike_map: &Value, short: f64, long: f64) -> f64 {
    quote_width(strike_map, short).unwrap_or(f64::INFINITY)
        + quote_width(strike_map, long).unwrap_or(f64::INFINITY)
}

fn quote_width(strike_map: &Value, strike: f64) -> Option<f64> {
    let contract = contract_at_strike(strike_map, strike)?;
    let bid = positive_field(contract, "bid")?;
    let ask = positive_field(contract, "ask")?;
    if ask < bid {
        return None;
    }
    Some(ask - bid)
}

fn contract_field(strike_map: &Value, strike: f64, field: &str) -> Option<f64> {
    contract_at_strike(strike_map, strike).and_then(|c| c.get(field).and_then(|v| v.as_f64()))
}

fn positive_field(contract: &Value, field: &str) -> Option<f64> {
    contract
        .get(field)
        .and_then(|v| v.as_f64())
        .filter(|v| *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::VerticalEntryRules;

    fn entry() -> VerticalEntryRules {
        VerticalEntryRules {
            min_credit: 0.30,
            max_width: 5.0,
            short_delta_min: 0.12,
            short_delta_max: 0.20,
            min_pop_pct: Some(60.0),
            min_distance_to_be_pct: Some(3.0),
            min_credit_to_width_pct: Some(6.0),
            min_short_otm_pct: Some(3.0),
            min_iv_rv_ratio: None,
            ..Default::default()
        }
    }

    fn contract(delta: f64, bid: f64, ask: f64, vol: f64) -> Value {
        serde_json::json!([{
            "delta": delta,
            "bid": bid,
            "ask": ask,
            "mark": (bid + ask) / 2.0,
            "volatility": vol,
            "theta": -0.05
        }])
    }

    #[test]
    fn picks_higher_credit_width_inside_the_delta_band() {
        // Midpoint delta is 0.16. That strike pays 7% of width. The 0.19 strike
        // in the same band pays 15%. The old picker would have taken 0.16.
        let map = serde_json::json!({
            "750.0": contract(-0.50, 20.0, 20.1, 18.0),
            "710.0": contract(-0.19, 3.00, 3.05, 20.0),
            "705.0": contract(-0.16, 2.20, 2.25, 20.5),
            "700.0": contract(-0.16, 2.00, 2.05, 21.0),
            "695.0": contract(-0.12, 1.60, 1.65, 22.0)
        });
        // 710/705 credit = 3.00 - 2.25 = 0.75 → 15% of $5
        // 700/695 credit = 2.00 - 1.65 = 0.35 → 7% of $5
        // Both deltas are in 0.12-0.20. 700's delta is -0.16 (same as 705).
        // 705 is the wing for 710, not a short we need. 700 short uses 695.
        let expiry = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 10, 8).unwrap();
        let search = search_verticals(
            &[(expiry, map)],
            &entry(),
            true,
            750.0,
            today,
            Some(16.0),
            Some(0.2),
            |_| None,
        );
        let best = search.best.expect("a spread clears");
        assert!(
            (best.short_strike - 710.0).abs() < 0.01,
            "picked {}",
            best.short_strike
        );
        assert!(best.credit_to_width_pct > 10.0);
        assert_eq!(search.funnel.admitted, 1);
        assert!(search.funnel.candidates_seen >= 1);
    }

    #[test]
    fn empty_expiry_window_is_a_no_expiry_reject() {
        let search = search_verticals(
            &[],
            &entry(),
            true,
            750.0,
            NaiveDate::from_ymd_opt(2026, 10, 8).unwrap(),
            None,
            None,
            |_| None,
        );
        assert!(search.best.is_none());
        assert_eq!(search.funnel.rejects.get("no_expiry").copied(), Some(1));
    }
}
