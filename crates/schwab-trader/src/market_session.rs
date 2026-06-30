//! US equity session helpers — all wall-clock times are US/Eastern (America/New_York).
//!
//! Schwab equity hours are defined in Eastern Time (EST/EDT). This module never uses
//! the local machine timezone or Pacific time.

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Timelike, Utc, Weekday};
use chrono_tz::{America::New_York, Tz};

use crate::rules::TraderRules;

/// IANA timezone for US equity regular session (EST/EDT via DST rules).
pub const US_EQUITY_TIMEZONE: &str = "America/New_York";

pub fn trading_tz(tz_name: &str) -> Tz {
    tz_name.parse().unwrap_or(New_York)
}

pub fn now_et() -> DateTime<Tz> {
    now_in_tz(New_York)
}

pub fn now_in_tz(tz: Tz) -> DateTime<Tz> {
    Utc::now().with_timezone(&tz)
}

pub fn trading_day(tz_name: &str) -> NaiveDate {
    trading_day_at(Utc::now(), tz_name)
}

pub fn trading_day_at(now: DateTime<Utc>, tz_name: &str) -> NaiveDate {
    now.with_timezone(&trading_tz(tz_name)).date_naive()
}

pub fn parse_et_hms(hhmm: &str) -> Option<NaiveTime> {
    let parts: Vec<_> = hhmm.trim().split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let h: u32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Weekday equity regular session 9:30–16:00 in the configured US/Eastern timezone.
pub fn equity_regular_session_open(now: DateTime<Utc>, tz_name: &str) -> bool {
    let tz = trading_tz(tz_name);
    let et = now.with_timezone(&tz);
    if matches!(et.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let Some(open) = NaiveTime::from_hms_opt(9, 30, 0) else {
        return false;
    };
    let Some(close) = NaiveTime::from_hms_opt(16, 0, 0) else {
        return false;
    };
    let t = et.time();
    t >= open && t < close
}

pub fn entries_blocked(rules: &TraderRules) -> bool {
    if rules.schedule.market_hours_only
        && !equity_regular_session_open(Utc::now(), &rules.schedule.timezone)
    {
        return true;
    }
    if past_entry_cutoff(rules) {
        return true;
    }
    false
}

pub fn past_entry_cutoff(rules: &TraderRules) -> bool {
    let Some(cutoff) = parse_et_hms(&rules.playbook.closure.block_entries_after_et) else {
        return false;
    };
    now_in_tz(trading_tz(&rules.schedule.timezone)).time() >= cutoff
}

pub fn must_flatten_now(rules: &TraderRules) -> bool {
    must_flatten_now_at(rules, Utc::now())
}

pub fn must_flatten_now_at(rules: &TraderRules, now: DateTime<Utc>) -> bool {
    if !rules.playbook.closure.no_overnight_holds {
        return false;
    }
    let Some(flatten) = parse_et_hms(&rules.playbook.closure.flatten_by_et) else {
        return false;
    };
    if !equity_regular_session_open(now, &rules.schedule.timezone) {
        return rules.is_intraday();
    }
    now.with_timezone(&trading_tz(&rules.schedule.timezone))
        .time()
        >= flatten
}

pub fn opened_on_prior_et_day(opened_at: DateTime<Utc>, tz_name: &str) -> bool {
    opened_on_prior_et_day_at(opened_at, tz_name, Utc::now())
}

pub fn opened_on_prior_et_day_at(
    opened_at: DateTime<Utc>,
    tz_name: &str,
    now: DateTime<Utc>,
) -> bool {
    let tz = trading_tz(tz_name);
    let open_day = opened_at.with_timezone(&tz).date_naive();
    let today = now.with_timezone(&tz).date_naive();
    open_day < today
}

/// Weekday premarket window before 9:30 Eastern.
pub fn equity_premarket_window(now: DateTime<Utc>, tz_name: &str, premarket_start_et: &str) -> bool {
    let tz = trading_tz(tz_name);
    let et = now.with_timezone(&tz);
    if matches!(et.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let Some(start) = parse_et_hms(premarket_start_et) else {
        return false;
    };
    let Some(open) = NaiveTime::from_hms_opt(9, 30, 0) else {
        return false;
    };
    let t = et.time();
    t >= start && t < open
}

/// Minutes until 9:30 Eastern open on a weekday.
pub fn minutes_until_equity_open(now: DateTime<Utc>, tz_name: &str) -> i64 {
    let tz = trading_tz(tz_name);
    let et = now.with_timezone(&tz);
    if matches!(et.weekday(), Weekday::Sat | Weekday::Sun) {
        return 24 * 60;
    }
    let Some(open) = NaiveTime::from_hms_opt(9, 30, 0) else {
        return 0;
    };
    let t = et.time();
    if t >= open {
        return 0;
    }
    let open_secs = open.num_seconds_from_midnight() as i64;
    let now_secs = t.num_seconds_from_midnight() as i64;
    (open_secs - now_secs) / 60
}

/// Debug snapshot for tick output — proves Eastern clock, not local/Pacific.
pub fn market_clock_json(rules: &TraderRules) -> serde_json::Value {
    let tz_name = &rules.schedule.timezone;
    let tz = trading_tz(tz_name);
    let now = now_in_tz(tz);
    let utc = Utc::now();
    serde_json::json!({
        "timezone": tz_name,
        "now_eastern": now.to_rfc3339(),
        "now_utc": utc.to_rfc3339(),
        "weekday_eastern": format!("{:?}", now.weekday()),
        "regular_session_open": equity_regular_session_open(utc, tz_name),
        "premarket_window": equity_premarket_window(
            utc,
            tz_name,
            &rules.schedule.premarket_start_et,
        ),
        "minutes_to_open": minutes_until_equity_open(utc, tz_name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_et_time() {
        assert!(parse_et_hms("15:55").is_some());
        assert!(parse_et_hms("bad").is_none());
    }

    #[test]
    fn noon_pacific_is_afternoon_eastern_session_open() {
        let pacific = chrono_tz::America::Los_Angeles;
        // 2026-01-15 12:00 PST = 15:00 ET (winter, market open)
        let utc = pacific
            .with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(equity_regular_session_open(utc, US_EQUITY_TIMEZONE));
    }

    #[test]
    fn early_morning_pacific_is_before_eastern_open() {
        let pacific = chrono_tz::America::Los_Angeles;
        // 2026-01-15 06:00 PST = 09:00 ET (before 9:30 open)
        let utc = pacific
            .with_ymd_and_hms(2026, 1, 15, 6, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(!equity_regular_session_open(utc, US_EQUITY_TIMEZONE));
    }

    #[test]
    fn evening_pacific_is_after_eastern_close() {
        let pacific = chrono_tz::America::Los_Angeles;
        // 2026-01-15 18:00 PST = 21:00 ET (after 16:00 close)
        let utc = pacific
            .with_ymd_and_hms(2026, 1, 15, 18, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(!equity_regular_session_open(utc, US_EQUITY_TIMEZONE));
    }

    #[test]
    fn wrong_timezone_string_falls_back_to_new_york() {
        let tz = trading_tz("America/Los_Angeles");
        assert_eq!(tz, chrono_tz::America::Los_Angeles);
        // Invalid → New_York
        let fallback = trading_tz("not-a-timezone");
        assert_eq!(fallback, New_York);
    }
}
