//! Small, dependency-free indicators for the research paper book.
//!
//! The paper fill model uses RSI (entry guard) and ATR (trailing stop), so both
//! must be reproducible from the daily bars the arm already fetches. Everything
//! here is a pure function of `(bars, period)` and is unit tested.
//!
//! Point-in-time discipline: callers must pass bars that are strictly *before*
//! the decision date. [`before`] does that filtering; nothing here looks ahead.

use broker_core::Bar;
use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};

/// Parse a bar timestamp. Public, Schwab and the offline lab data all use
/// different shapes, so accept the common ones and epoch seconds/millis.
pub fn parse_bar_date(raw: &str) -> Option<NaiveDate> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.date_naive());
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(raw, format) {
            return Some(dt.date());
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Some(date);
    }
    if raw.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(number) = raw.parse::<i64>() {
            let seconds = if number > 10_000_000_000 {
                number / 1_000
            } else {
                number
            };
            return DateTime::from_timestamp(seconds, 0).map(|dt| dt.date_naive());
        }
    }
    None
}

/// US Eastern calendar date for a UTC instant, using the US DST rule
/// (2nd Sunday of March to 1st Sunday of November). Daily bars carry exchange
/// dates, so decisions, daily-loss buckets and cooldowns must use this instead of
/// the UTC date; a 21:00 ET tick is already the next day in UTC.
pub fn exchange_date(now: DateTime<Utc>) -> NaiveDate {
    let et_offset_hours = if us_daylight(now.date_naive()) { 4 } else { 5 };
    (now - chrono::Duration::hours(et_offset_hours)).date_naive()
}

fn us_daylight(date: NaiveDate) -> bool {
    let year = date.year();
    date >= nth_sunday(year, 3, 2) && date < nth_sunday(year, 11, 1)
}

fn nth_sunday(year: i32, month: u32, n: u32) -> NaiveDate {
    let mut date = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let mut seen = 0;
    loop {
        if date.weekday() == Weekday::Sun {
            seen += 1;
            if seen == n {
                return date;
            }
        }
        date = date.succ_opt().expect("date overflow");
    }
}

/// Bars sorted oldest → newest with duplicate dates collapsed (last wins).
pub fn sorted(bars: &[Bar]) -> Vec<Bar> {
    let mut dated: Vec<(NaiveDate, Bar)> = bars
        .iter()
        .filter_map(|bar| parse_bar_date(&bar.timestamp).map(|date| (date, bar.clone())))
        .collect();
    dated.sort_by_key(|(date, _)| *date);
    let mut out: Vec<Bar> = Vec::with_capacity(dated.len());
    for (date, bar) in dated {
        match out.last() {
            Some(last) if parse_bar_date(&last.timestamp) == Some(date) => {
                *out.last_mut().unwrap() = bar;
            }
            _ => out.push(bar),
        }
    }
    out
}

/// Bars strictly before `date`, oldest → newest. This is the only view the
/// entry and exit rules are allowed to see.
pub fn before(bars: &[Bar], date: NaiveDate) -> Vec<Bar> {
    sorted(bars)
        .into_iter()
        .filter(|bar| parse_bar_date(&bar.timestamp).is_some_and(|bar_date| bar_date < date))
        .collect()
}

pub fn closes(bars: &[Bar]) -> Vec<f64> {
    bars.iter().map(|bar| bar.close).collect()
}

/// Wilder's RSI. `None` when there are not enough closes.
pub fn rsi(closes: &[f64], period: usize) -> Option<f64> {
    if period == 0 || closes.len() <= period {
        return None;
    }
    let mut gain = 0.0;
    let mut loss = 0.0;
    for window in closes.windows(2).take(period) {
        let change = window[1] - window[0];
        if change > 0.0 {
            gain += change;
        } else {
            loss -= change;
        }
    }
    let period_f = period as f64;
    let mut avg_gain = gain / period_f;
    let mut avg_loss = loss / period_f;
    for window in closes.windows(2).skip(period) {
        let change = window[1] - window[0];
        let (up, down) = if change > 0.0 {
            (change, 0.0)
        } else {
            (0.0, -change)
        };
        avg_gain = (avg_gain * (period_f - 1.0) + up) / period_f;
        avg_loss = (avg_loss * (period_f - 1.0) + down) / period_f;
    }
    if avg_loss <= f64::EPSILON && avg_gain <= f64::EPSILON {
        // A flat series has no direction; 50 is the neutral convention.
        return Some(50.0);
    }
    if avg_loss <= f64::EPSILON {
        return Some(100.0);
    }
    let rs = avg_gain / avg_loss;
    Some(100.0 - 100.0 / (1.0 + rs))
}

/// Wilder's ATR from OHLC bars. `None` when there are not enough bars.
pub fn atr(bars: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() < period + 1 {
        return None;
    }
    let mut ranges = Vec::with_capacity(bars.len() - 1);
    for window in bars.windows(2) {
        let previous_close = window[0].close;
        let bar = &window[1];
        let high_low = bar.high - bar.low;
        let high_close = (bar.high - previous_close).abs();
        let low_close = (bar.low - previous_close).abs();
        ranges.push(high_low.max(high_close).max(low_close));
    }
    if ranges.len() < period {
        return None;
    }
    let period_f = period as f64;
    let mut value = ranges[..period].iter().sum::<f64>() / period_f;
    for range in ranges.iter().skip(period) {
        value = (value * (period_f - 1.0) + range) / period_f;
    }
    Some(value)
}

/// ATR for one symbol's raw bars, filtered to data available before `date`.
pub fn atr_before(bars: &[Bar], date: NaiveDate, period: usize) -> Option<f64> {
    atr(&before(bars, date), period)
}

/// RSI for one symbol's raw bars, filtered to data available before `date`.
pub fn rsi_before(bars: &[Bar], date: NaiveDate, period: usize) -> Option<f64> {
    rsi(&closes(&before(bars, date)), period)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: &str, open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            timestamp: date.to_string(),
            open,
            high,
            low,
            close,
            volume: 1_000.0,
        }
    }

    #[test]
    fn parses_the_shapes_we_see_in_the_wild() {
        let expected = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        for raw in [
            "2026-10-22",
            "2026-10-22T00:00:00Z",
            "2026-10-22T00:00:00+00:00",
            "2026-10-22 00:00:00",
            "1792627200",
            "1792627200000",
        ] {
            assert_eq!(parse_bar_date(raw), Some(expected), "failed on {raw}");
        }
        assert_eq!(parse_bar_date(""), None);
        assert_eq!(parse_bar_date("not a date"), None);
    }

    #[test]
    fn exchange_date_uses_the_us_dst_rule() {
        use chrono::TimeZone;
        // 01:30 UTC in late October is still the previous ET evening.
        let late = Utc.with_ymd_and_hms(2026, 10, 29, 1, 30, 0).unwrap();
        assert_eq!(
            exchange_date(late),
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()
        );
        // 14:00 UTC during a session is the same date.
        let session = Utc.with_ymd_and_hms(2026, 10, 29, 14, 0, 0).unwrap();
        assert_eq!(
            exchange_date(session),
            NaiveDate::from_ymd_opt(2026, 10, 29).unwrap()
        );
        // Winter (EST, UTC-5): 02:00 UTC is the previous ET date.
        let winter = Utc.with_ymd_and_hms(2026, 12, 1, 2, 0, 0).unwrap();
        assert_eq!(
            exchange_date(winter),
            NaiveDate::from_ymd_opt(2026, 11, 30).unwrap()
        );
    }

    #[test]
    fn sorting_dedupes_and_orders() {
        let bars = vec![
            bar("2026-10-22", 1.0, 2.0, 0.5, 1.5),
            bar("2026-10-20", 1.0, 2.0, 0.5, 1.6),
            bar("2026-10-20", 1.0, 2.0, 0.5, 1.7),
        ];
        let sorted = sorted(&bars);
        assert_eq!(sorted.len(), 2);
        assert_eq!(sorted[0].close, 1.7);
        assert_eq!(sorted[1].close, 1.5);
    }

    #[test]
    fn before_excludes_the_decision_day() {
        let bars = vec![
            bar("2026-10-20", 1.0, 2.0, 0.5, 1.0),
            bar("2026-10-21", 1.0, 2.0, 0.5, 1.0),
            bar("2026-10-22", 1.0, 2.0, 0.5, 1.0),
        ];
        let date = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let visible = before(&bars, date);
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn rsi_is_100_when_every_change_is_up() {
        let closes: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        let value = rsi(&closes, 14).unwrap();
        assert!((value - 100.0).abs() < 1e-9, "got {value}");
    }

    #[test]
    fn rsi_is_zero_when_every_change_is_down() {
        let closes: Vec<f64> = (0..20).map(|i| 100.0 - i as f64).collect();
        let value = rsi(&closes, 14).unwrap();
        assert!(value.abs() < 1e-9, "got {value}");
    }

    #[test]
    fn rsi_matches_the_classic_wilder_example() {
        // Wilder's original 14-period example, closes from his book.
        let closes = [
            44.34, 44.09, 44.15, 43.61, 44.33, 44.83, 45.10, 45.42, 45.84, 46.08, 45.89, 46.03,
            45.61, 46.28, 46.28, 46.00, 46.03, 46.41, 46.22,
        ];
        // First published value after the 14-period seed is ~70.53.
        let first = rsi(&closes[..15], 14).unwrap();
        assert!((first - 70.53).abs() < 0.1, "got {first}");
        // Published value four updates later is ~66.32.
        let later = rsi(&closes, 14).unwrap();
        assert!((later - 66.32).abs() < 0.1, "got {later}");
    }

    #[test]
    fn rsi_of_a_flat_series_is_neutral() {
        let closes = vec![100.0; 20];
        assert_eq!(rsi(&closes, 14), Some(50.0));
    }

    #[test]
    fn atr_is_zero_for_a_flat_series() {
        let bars: Vec<Bar> = (0..20)
            .map(|i| bar(&format!("2026-10-{:02}", i + 1), 100.0, 100.0, 100.0, 100.0))
            .collect();
        let value = atr(&bars, 14).unwrap();
        assert!(value.abs() < 1e-9, "got {value}");
    }

    #[test]
    fn atr_grows_with_range() {
        let bars: Vec<Bar> = (0..20)
            .map(|i| bar(&format!("2026-10-{:02}", i + 1), 100.0, 105.0, 95.0, 100.0))
            .collect();
        let value = atr(&bars, 14).unwrap();
        assert!((value - 10.0).abs() < 1e-9, "got {value}");
    }

    #[test]
    fn short_series_returns_none() {
        let bars = vec![bar("2026-10-01", 1.0, 2.0, 0.5, 1.0)];
        assert_eq!(atr(&bars, 14), None);
        assert_eq!(rsi(&[1.0, 2.0], 14), None);
    }
}
