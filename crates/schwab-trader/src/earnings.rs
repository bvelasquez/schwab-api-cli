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

/// Calendar date wins when `source` is `calendar` and a date is present.
/// Otherwise the 91-day heuristic from `last`. None when both are missing.
pub fn resolve_next_earnings(
    last: Option<NaiveDate>,
    calendar_next: Option<NaiveDate>,
    today: NaiveDate,
    source: &str,
) -> Option<EarningsEstimate> {
    if source == "calendar" {
        if let Some(next) = calendar_next {
            return Some(EarningsEstimate {
                last_earnings_date: last.unwrap_or(next),
                estimated_next_earnings: next,
                days_until_estimated: (next - today).num_days(),
                confidence: "calendar",
            });
        }
    }
    last.map(|d| estimate_next_earnings(d, today))
}

/// `rules/earnings-calendar.json`: `{ "AAPL": "2026-10-30", ... }`.
/// Missing file or symbol returns None (caller falls back to the heuristic).
pub fn calendar_next(symbol: &str, rel: Option<&str>) -> Option<NaiveDate> {
    let rel = rel.unwrap_or("earnings-calendar.json");
    let path = std::path::Path::new(rel);
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let date = value.get(symbol.trim().to_uppercase())?.as_str()?;
    NaiveDate::parse_from_str(&date[..10.min(date.len())], "%Y-%m-%d").ok()
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

    #[test]
    fn calendar_date_beats_the_91_day_heuristic() {
        let last = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let announced = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        let est = resolve_next_earnings(Some(last), Some(announced), today, "calendar").unwrap();
        assert_eq!(est.estimated_next_earnings, announced);
        assert_eq!(est.confidence, "calendar");
        assert_eq!(est.days_until_estimated, 22);
        let fallback = resolve_next_earnings(Some(last), None, today, "calendar").unwrap();
        assert_eq!(fallback.confidence, "heuristic");
    }
}
