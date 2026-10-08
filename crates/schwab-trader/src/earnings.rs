//! Earnings blackout. A confirmed calendar date wins over the 91-day heuristic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

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

#[derive(Debug, Clone, Default)]
pub struct EarningsCalendar {
    /// Symbol → sorted confirmed announcement dates.
    pub dates: HashMap<String, Vec<NaiveDate>>,
}

impl EarningsCalendar {
    pub fn from_yaml(raw: &str) -> anyhow::Result<Self> {
        let value: serde_json::Value = serde_yaml::from_str(raw)?;
        let mut dates = HashMap::new();
        let symbols = value.get("symbols").cloned().unwrap_or(value);
        let Some(obj) = symbols.as_object() else {
            anyhow::bail!("earnings calendar must be a map of symbol → [dates]");
        };
        for (sym, list) in obj {
            let Some(arr) = list.as_array() else {
                continue;
            };
            let mut parsed = Vec::new();
            for item in arr {
                if let Some(s) = item.as_str() {
                    if let Ok(d) = NaiveDate::parse_from_str(&s[..10.min(s.len())], "%Y-%m-%d") {
                        parsed.push(d);
                    }
                }
            }
            parsed.sort();
            parsed.dedup();
            if !parsed.is_empty() {
                dates.insert(sym.trim().to_uppercase(), parsed);
            }
        }
        Ok(Self { dates })
    }

    pub fn next_on_or_after(&self, symbol: &str, today: NaiveDate) -> Option<NaiveDate> {
        self.dates
            .get(&symbol.trim().to_uppercase())
            .and_then(|ds| ds.iter().copied().find(|d| *d >= today))
    }

    pub fn previous_before(&self, symbol: &str, today: NaiveDate) -> Option<NaiveDate> {
        self.dates
            .get(&symbol.trim().to_uppercase())
            .and_then(|ds| ds.iter().copied().filter(|d| *d < today).next_back())
    }
}

static CALENDAR_CACHE: Mutex<Option<(String, String, EarningsCalendar)>> = Mutex::new(None);

/// Load `path`, reusing the parse when the file bytes have not changed.
pub fn load_calendar_cached(path: &str) -> anyhow::Result<EarningsCalendar> {
    let raw = std::fs::read_to_string(Path::new(path))?;
    let mut guard = CALENDAR_CACHE
        .lock()
        .map_err(|_| anyhow::anyhow!("earnings calendar cache poisoned"))?;
    if let Some((cached_path, cached_raw, cal)) = guard.as_ref() {
        if cached_path == path && cached_raw == &raw {
            return Ok(cal.clone());
        }
    }
    let cal = EarningsCalendar::from_yaml(&raw)?;
    *guard = Some((path.to_string(), raw, cal.clone()));
    Ok(cal)
}

/// Calendar date when one is on file; otherwise the 91-day heuristic when
/// `last` is known. Returns None when neither source has a date.
pub fn resolve_next_earnings(
    symbol: &str,
    last: Option<NaiveDate>,
    today: NaiveDate,
    calendar: Option<&EarningsCalendar>,
) -> Option<EarningsEstimate> {
    if let Some(next) = calendar.and_then(|c| c.next_on_or_after(symbol, today)) {
        let last_date = calendar
            .and_then(|c| c.previous_before(symbol, today))
            .or(last)
            .unwrap_or(next);
        let days_until = (next - today).num_days();
        return Some(EarningsEstimate {
            last_earnings_date: last_date,
            estimated_next_earnings: next,
            days_until_estimated: days_until,
            confidence: "calendar",
        });
    }
    last.map(|d| estimate_next_earnings(d, today))
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
pub fn earnings_blackout_reason(estimate: &EarningsEstimate, lead_days: u32) -> Option<String> {
    if lead_days == 0 {
        return None;
    }
    if estimate.days_until_estimated >= 0 && estimate.days_until_estimated <= lead_days as i64 {
        return Some(format!(
            "within {lead_days}d of estimated earnings {} (last={}, confidence={})",
            estimate.estimated_next_earnings, estimate.last_earnings_date, estimate.confidence
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

    #[test]
    fn calendar_date_replaces_the_heuristic() {
        let raw = r#"
symbols:
  AAPL: ["2026-01-30", "2026-10-30"]
"#;
        let cal = EarningsCalendar::from_yaml(raw).unwrap();
        let last = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let est = resolve_next_earnings("aapl", Some(last), today, Some(&cal)).unwrap();
        assert_eq!(est.confidence, "calendar");
        assert_eq!(
            est.estimated_next_earnings,
            NaiveDate::from_ymd_opt(2026, 10, 30).unwrap()
        );
        assert_eq!(
            est.last_earnings_date,
            NaiveDate::from_ymd_opt(2026, 1, 30).unwrap()
        );
        let heuristic = estimate_next_earnings(last, today);
        assert_ne!(
            est.estimated_next_earnings,
            heuristic.estimated_next_earnings
        );
    }
}
