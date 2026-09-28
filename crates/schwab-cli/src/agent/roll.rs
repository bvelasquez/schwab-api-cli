//! Defensive rolling: when a credit vertical hits the mechanical stop, prefer
//! close + reopen farther OTM / later DTE over eating a full stop (v1: verticals only).

use serde_json::{json, Value};

use crate::rules::{RollConfig, VerticalEntryRules};

use super::state::TrackedPosition;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollSkipReason {
    Disabled,
    NotVertical,
    MissingEntryParams,
    LowDte,
    ShortTooClose,
    MaxRollsPosition,
    MaxRollsDay,
    PortfolioRisk,
}

impl RollSkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "roll_disabled",
            Self::NotVertical => "roll_not_vertical",
            Self::MissingEntryParams => "roll_missing_entry_params",
            Self::LowDte => "roll_low_dte",
            Self::ShortTooClose => "roll_short_too_close",
            Self::MaxRollsPosition => "roll_max_per_position",
            Self::MaxRollsDay => "roll_max_per_day",
            Self::PortfolioRisk => "roll_portfolio_risk",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RollEligibility<'a> {
    pub tracked: &'a TrackedPosition,
    pub mark_dte: i64,
    pub short_otm_pct: Option<f64>,
    pub rolls_today: u32,
    /// `reserved_risk_usd` including this position.
    pub reserved_risk_usd: f64,
    pub max_portfolio_risk_usd: f64,
}

/// All eligibility gates for attempting a defensive roll on a stop_loss.
pub fn roll_eligible(cfg: &RollConfig, input: &RollEligibility<'_>) -> Result<(), RollSkipReason> {
    if !cfg.enabled {
        return Err(RollSkipReason::Disabled);
    }
    if !input.tracked.strategy.eq_ignore_ascii_case("vertical") {
        return Err(RollSkipReason::NotVertical);
    }
    if input.tracked.entry_params.is_none() {
        return Err(RollSkipReason::MissingEntryParams);
    }
    if input.mark_dte < cfg.min_dte_remaining as i64 {
        return Err(RollSkipReason::LowDte);
    }
    match input.short_otm_pct {
        Some(otm) if otm >= cfg.min_short_otm_pct => {}
        Some(_) => return Err(RollSkipReason::ShortTooClose),
        None => return Err(RollSkipReason::ShortTooClose),
    }
    if input.tracked.rolls_used >= cfg.max_rolls_per_position {
        return Err(RollSkipReason::MaxRollsPosition);
    }
    if cfg.max_rolls_per_day > 0 && input.rolls_today >= cfg.max_rolls_per_day {
        return Err(RollSkipReason::MaxRollsDay);
    }
    // After close, reserved drops by this position's max_loss; replacement ≈ same size.
    let after_close = (input.reserved_risk_usd - input.tracked.max_loss_usd).max(0.0);
    let replacement = input.tracked.max_loss_usd.max(0.0);
    if after_close + replacement > input.max_portfolio_risk_usd + 1e-6 {
        return Err(RollSkipReason::PortfolioRisk);
    }
    Ok(())
}

/// `new_credit - close_debit >= -entry_credit * max_debit_pct / 100`
pub fn roll_money_ok(
    entry_credit: f64,
    close_debit: f64,
    new_credit: f64,
    max_debit_pct_of_entry_credit: f64,
) -> bool {
    if entry_credit <= f64::EPSILON {
        return false;
    }
    let net = new_credit - close_debit;
    let floor = -entry_credit * (max_debit_pct_of_entry_credit / 100.0);
    net >= floor - 1e-9
}

pub fn roll_net(close_debit: f64, new_credit: f64) -> f64 {
    new_credit - close_debit
}

/// Outcome of searching for + pricing a roll replacement, evaluated *before* the
/// being-rolled position is closed (the search is read-only market data). Encodes the
/// decision of whether to proceed with close+reopen or fall back to a plain stop_loss.
#[derive(Debug, Clone, PartialEq)]
pub enum RollPrecheck {
    /// A candidate exists and its economics clear `max_debit_pct_of_entry_credit`.
    Proceed,
    /// No candidate, or one existed but its economics failed — caller should not
    /// close the position for a roll; the normal stop_loss exit path should run instead.
    NoCandidate(String),
}

/// Decide whether a defensive roll should proceed to close+reopen, given the result of
/// searching for a replacement candidate (`candidate_credit`, `None` if the search found
/// nothing — pass the caller's own reason string for that case) and the roll's
/// debit-tolerance economics. Pure and independent of I/O so the close only ever
/// happens once a real, affordable candidate is confirmed.
pub fn precheck_roll_candidate(
    candidate_credit: Option<f64>,
    no_candidate_reason: &str,
    entry_credit: f64,
    close_debit: f64,
    max_debit_pct_of_entry_credit: f64,
) -> RollPrecheck {
    let Some(new_credit) = candidate_credit else {
        return RollPrecheck::NoCandidate(no_candidate_reason.to_string());
    };
    if !roll_money_ok(entry_credit, close_debit, new_credit, max_debit_pct_of_entry_credit) {
        return RollPrecheck::NoCandidate(format!(
            "roll_debit_too_large:net={:.3}",
            roll_net(close_debit, new_credit)
        ));
    }
    RollPrecheck::Proceed
}

/// Overlay the corrected `exit_reason` and a `roll_attempt` note onto a defensive
/// roll's close/open detail when the reopen failed — returned to the caller so the
/// per-tick action/journal output is honest even though the underlying close event
/// (already written) stays labeled `defensive_roll` in the append-only journal.
pub fn roll_failed_close_detail(base: &Value, failed_reason: &str) -> Value {
    let mut detail = base.clone();
    if let Some(obj) = detail.as_object_mut() {
        obj.insert("exit_reason".into(), json!("stop_loss"));
        obj.insert("roll_attempt".into(), json!({ "failed": failed_reason }));
    }
    detail
}

/// Cap |short_delta| for the replacement: min(entry max, target, original − 0.04).
pub fn roll_biased_short_delta_max(
    entry_short_delta_max: f64,
    target_short_delta_max: f64,
    original_short_delta: Option<f64>,
) -> f64 {
    let from_original = original_short_delta
        .map(|d| (d.abs() - 0.04).max(0.01))
        .unwrap_or(target_short_delta_max);
    entry_short_delta_max
        .min(target_short_delta_max)
        .min(from_original)
}

/// Prefer later DTE: max(entry dte_min, closed_dte + extension).
pub fn roll_biased_dte_min(entry_dte_min: u32, closed_dte: i64, min_dte_extension: u32) -> u32 {
    let extended = (closed_dte.max(0) as u32).saturating_add(min_dte_extension);
    entry_dte_min.max(extended)
}

/// Width ≤ original (or entry max_width).
pub fn roll_biased_max_width(entry_max_width: f64, original_width: Option<f64>) -> f64 {
    match original_width {
        Some(w) if w > f64::EPSILON => entry_max_width.min(w),
        _ => entry_max_width,
    }
}

pub fn original_width_from_params(params: &Value) -> Option<f64> {
    let short = params.get("short_strike")?.as_f64()?;
    let long = params.get("long_strike")?.as_f64()?;
    Some((short - long).abs())
}

pub fn spread_type_from_tracked(tracked: &TrackedPosition) -> Option<String> {
    tracked
        .entry_params
        .as_ref()
        .and_then(|p| p.get("spread_type"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            // Infer from strikes if present.
            let p = tracked.entry_params.as_ref()?;
            let short = p.get("short_strike")?.as_f64()?;
            let long = p.get("long_strike")?.as_f64()?;
            if short > long {
                Some("put_credit".into())
            } else {
                Some("call_credit".into())
            }
        })
}

/// Build vertical entry rules biased for a defensive roll replacement.
pub fn roll_biased_entry_rules(
    base: &VerticalEntryRules,
    roll: &RollConfig,
    closed_dte: i64,
    original_short_delta: Option<f64>,
    original_width: Option<f64>,
    contracts: u32,
) -> VerticalEntryRules {
    let mut entry = base.clone();
    entry.dte_min = roll_biased_dte_min(base.dte_min, closed_dte, roll.min_dte_extension);
    if entry.dte_min > entry.dte_max {
        entry.dte_max = entry.dte_min;
    }
    entry.short_delta_max = roll_biased_short_delta_max(
        base.short_delta_max,
        roll.target_short_delta_max,
        original_short_delta,
    );
    // Keep a usable band below the capped max.
    entry.short_delta_min = entry.short_delta_min.min(entry.short_delta_max * 0.5);
    entry.max_width = roll_biased_max_width(base.max_width, original_width);
    entry.max_contracts_per_trade = contracts.max(1);
    // Roll replaces a closed slot — don't block on open-count inside evaluate.
    entry.max_open_positions = entry.max_open_positions.saturating_add(1);
    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn vertical_tracked(rolls_used: u32) -> TrackedPosition {
        TrackedPosition {
            position_id: "p1".into(),
            account_hash: "acc".into(),
            underlying: "SPY".into(),
            expiry: "2026-09-18".into(),
            strategy: "vertical".into(),
            opened_at: Utc::now(),
            entry_credit: Some(1.0),
            max_loss_usd: 400.0,
            contracts: 1,
            entry_params: Some(json!({
                "underlying": "SPY",
                "expiry": "2026-09-18",
                "spread_type": "put_credit",
                "short_strike": 500.0,
                "long_strike": 495.0,
                "contracts": 1.0,
            })),
            rolls_used,
            ..Default::default()
        }
    }

    fn cfg() -> RollConfig {
        RollConfig::default()
    }

    #[test]
    fn eligibility_passes_defaults() {
        let mut roll = cfg();
        roll.enabled = true;
        let tracked = vertical_tracked(0);
        let input = RollEligibility {
            tracked: &tracked,
            mark_dte: 30,
            short_otm_pct: Some(2.0),
            rolls_today: 0,
            reserved_risk_usd: 400.0,
            max_portfolio_risk_usd: 4000.0,
        };
        assert!(roll_eligible(&roll, &input).is_ok());
    }

    #[test]
    fn eligibility_rejects_disabled() {
        let tracked = vertical_tracked(0);
        let input = RollEligibility {
            tracked: &tracked,
            mark_dte: 30,
            short_otm_pct: Some(2.0),
            rolls_today: 0,
            reserved_risk_usd: 400.0,
            max_portfolio_risk_usd: 4000.0,
        };
        assert_eq!(
            roll_eligible(&cfg(), &input),
            Err(RollSkipReason::Disabled)
        );
    }

    #[test]
    fn eligibility_rejects_itm_short() {
        let mut roll = cfg();
        roll.enabled = true;
        let tracked = vertical_tracked(0);
        let input = RollEligibility {
            tracked: &tracked,
            mark_dte: 30,
            short_otm_pct: Some(0.2),
            rolls_today: 0,
            reserved_risk_usd: 400.0,
            max_portfolio_risk_usd: 4000.0,
        };
        assert_eq!(
            roll_eligible(&roll, &input),
            Err(RollSkipReason::ShortTooClose)
        );
    }

    #[test]
    fn eligibility_rejects_low_dte() {
        let mut roll = cfg();
        roll.enabled = true;
        let tracked = vertical_tracked(0);
        let input = RollEligibility {
            tracked: &tracked,
            mark_dte: 10,
            short_otm_pct: Some(2.0),
            rolls_today: 0,
            reserved_risk_usd: 400.0,
            max_portfolio_risk_usd: 4000.0,
        };
        assert_eq!(roll_eligible(&roll, &input), Err(RollSkipReason::LowDte));
    }

    #[test]
    fn eligibility_rejects_max_rolls_position() {
        let mut roll = cfg();
        roll.enabled = true;
        let tracked = vertical_tracked(1);
        let input = RollEligibility {
            tracked: &tracked,
            mark_dte: 30,
            short_otm_pct: Some(2.0),
            rolls_today: 0,
            reserved_risk_usd: 400.0,
            max_portfolio_risk_usd: 4000.0,
        };
        assert_eq!(
            roll_eligible(&roll, &input),
            Err(RollSkipReason::MaxRollsPosition)
        );
    }

    #[test]
    fn eligibility_rejects_max_rolls_day() {
        let mut roll = cfg();
        roll.enabled = true;
        let tracked = vertical_tracked(0);
        let input = RollEligibility {
            tracked: &tracked,
            mark_dte: 30,
            short_otm_pct: Some(2.0),
            rolls_today: 1,
            reserved_risk_usd: 400.0,
            max_portfolio_risk_usd: 4000.0,
        };
        assert_eq!(
            roll_eligible(&roll, &input),
            Err(RollSkipReason::MaxRollsDay)
        );
    }

    #[test]
    fn money_allows_limited_debit() {
        // entry 1.00, close 2.00, new 1.80 → net -0.20 = 20% debit of entry → ok at 25%
        assert!(roll_money_ok(1.0, 2.0, 1.80, 25.0));
        // net -0.30 = 30% → reject
        assert!(!roll_money_ok(1.0, 2.0, 1.70, 25.0));
        // prefer credit
        assert!(roll_money_ok(1.0, 1.50, 1.60, 25.0));
    }

    #[test]
    fn delta_bias_takes_lower_of_target_and_original_minus() {
        assert!((roll_biased_short_delta_max(0.30, 0.12, Some(0.18)) - 0.12).abs() < 1e-9);
        assert!((roll_biased_short_delta_max(0.30, 0.12, Some(0.14)) - 0.10).abs() < 1e-9);
        assert!((roll_biased_short_delta_max(0.08, 0.12, Some(0.20)) - 0.08).abs() < 1e-9);
    }

    #[test]
    fn dte_bias_extends_past_closed() {
        assert_eq!(roll_biased_dte_min(30, 25, 7), 32);
        assert_eq!(roll_biased_dte_min(40, 25, 7), 40);
    }

    #[test]
    fn width_bias_caps_to_original() {
        assert!((roll_biased_max_width(5.0, Some(3.0)) - 3.0).abs() < 1e-9);
        assert!((roll_biased_max_width(5.0, None) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn precheck_rejects_when_search_found_nothing() {
        assert_eq!(
            precheck_roll_candidate(None, "roll_no_candidate:no short in delta band", 1.0, 1.2, 25.0),
            RollPrecheck::NoCandidate("roll_no_candidate:no short in delta band".into())
        );
    }

    #[test]
    fn precheck_rejects_when_economics_fail() {
        // entry 1.00, close 1.70, new candidate 1.50 -> net -0.20 = 20% debit, over 10% cap.
        assert_eq!(
            precheck_roll_candidate(Some(1.50), "", 1.0, 1.70, 10.0),
            RollPrecheck::NoCandidate("roll_debit_too_large:net=-0.200".into())
        );
    }

    #[test]
    fn precheck_proceeds_when_candidate_is_affordable() {
        assert_eq!(
            precheck_roll_candidate(Some(1.80), "", 1.0, 2.00, 25.0),
            RollPrecheck::Proceed
        );
    }

    #[test]
    fn failed_close_detail_overlays_stop_loss_and_roll_attempt() {
        let base = json!({ "fill_status": "FILLED", "exit_reason": "defensive_roll" });
        let out = roll_failed_close_detail(&base, "roll_open_failed:margin exceeded");
        assert_eq!(out["exit_reason"], json!("stop_loss"));
        assert_eq!(
            out["roll_attempt"]["failed"],
            json!("roll_open_failed:margin exceeded")
        );
        // Original fields are preserved.
        assert_eq!(out["fill_status"], json!("FILLED"));
    }
}
