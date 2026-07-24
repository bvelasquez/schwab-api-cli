//! Earnings blackout using Schwab `lastEarningsDate` + quarterly heuristic.

use chrono::{Duration, NaiveDate, Utc};
use serde_json::Value;

/// Days in a nominal quarter for next-earnings estimate.
const QUARTER_DAYS: i64 = 91;

#[derive(Debug, Clone, PartialEq)]
pub struct EarningsEstimate {
    pub last_earnings_date: NaiveDate,
    pub estimated_next_earnings: NaiveDate,
    pub days_until_estimated: i64,
    pub confidence: &'static str,
}

/// Parse `lastEarningsDate` from a quote fundamental block (ISO or date-only).
pub fn parse_last_earnings_date(fundamental: &Value) -> Option<NaiveDate> {
    let raw = fundamental
        .get("lastEarningsDate")
        .or_else(|| fundamental.get("lastEarningsDateTime"))
        .and_then(|v| v.as_str())?;
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(d) = NaiveDate::parse_from_str(&s[..10.min(s.len())], "%Y-%m-%d") {
        return Some(d);
    }
    // e.g. 2026-04-30T00:00:00Z
    if s.len() >= 10 {
        if let Ok(d) = NaiveDate::parse_from_str(&s[0..10], "%Y-%m-%d") {
            return Some(d);
        }
    }
    None
}

pub fn estimate_next_earnings(last: NaiveDate, today: NaiveDate) -> EarningsEstimate {
    let mut next = last + Duration::days(QUARTER_DAYS);
    // Advance quarters until next is on/after today (stale lastEarningsDate).
    while next < today {
        next += Duration::days(QUARTER_DAYS);
    }
    let days_until = (next - today).num_days();
    EarningsEstimate {
        last_earnings_date: last,
        estimated_next_earnings: next,
        days_until_estimated: days_until,
        confidence: "heuristic",
    }
}

/// When `lead_days > 0` and estimate is within the window, return a reject reason.
pub fn earnings_blackout_reason(
    estimate: &EarningsEstimate,
    lead_days: u32,
) -> Option<String> {
    if lead_days == 0 {
        return None;
    }
    if estimate.days_until_estimated >= 0 && estimate.days_until_estimated <= lead_days as i64 {
        return Some(format!(
            "within {lead_days}d of estimated earnings {} (last={}, confidence={})",
            estimate.estimated_next_earnings,
            estimate.last_earnings_date,
            estimate.confidence
        ));
    }
    None
}

pub fn today_et_naive() -> NaiveDate {
    Utc::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_iso_last_earnings() {
        let f = json!({ "lastEarningsDate": "2026-04-30T00:00:00Z" });
        assert_eq!(
            parse_last_earnings_date(&f),
            Some(NaiveDate::from_ymd_opt(2026, 4, 30).unwrap())
        );
    }

    #[test]
    fn estimates_next_quarter_and_blocks_inside_window() {
        let last = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let est = estimate_next_earnings(last, today);
        // 2026-04-30 + 91 = 2026-07-30
        assert_eq!(
            est.estimated_next_earnings,
            NaiveDate::from_ymd_opt(2026, 7, 30).unwrap()
        );
        assert_eq!(est.days_until_estimated, 6);
        assert!(earnings_blackout_reason(&est, 2).is_none());
        assert!(earnings_blackout_reason(&est, 7).is_some());
    }
}
