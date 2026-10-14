//! Five-field cron in a named timezone, plus an optional same-day window.

use std::collections::BTreeSet;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone)]
pub struct Cron {
    minute: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
}

#[derive(Debug, Clone)]
struct Field {
    any: bool,
    values: BTreeSet<u32>,
}

/// Inclusive local clock window. `09:30`–`16:00` keeps 16:00 and drops 09:00.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    start_min: u32,
    end_min: u32,
}

impl Cron {
    pub fn parse(raw: &str) -> Result<Self> {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.len() != 5 {
            bail!("cron must be 5 fields (minute hour day-of-month month day-of-week), got `{raw}`");
        }
        Ok(Self {
            minute: parse_field(parts[0], 0, 59, false)?,
            hour: parse_field(parts[1], 0, 23, false)?,
            dom: parse_field(parts[2], 1, 31, false)?,
            month: parse_field(parts[3], 1, 12, false)?,
            dow: parse_field(parts[4], 0, 7, true)?,
        })
    }

    fn matches(&self, dt: DateTime<Tz>) -> bool {
        if !self.minute.contains(dt.minute()) || !self.hour.contains(dt.hour()) || !self.month.contains(dt.month()) {
            return false;
        }
        let dom_ok = self.dom.contains(dt.day());
        let dow_ok = self.dow.contains(dt.weekday().num_days_from_sunday());
        // Vixie cron: when both day fields are restricted, either one may match.
        if !self.dom.any && !self.dow.any {
            dom_ok || dow_ok
        } else {
            (self.dom.any || dom_ok) && (self.dow.any || dow_ok)
        }
    }
}

impl Field {
    fn contains(&self, value: u32) -> bool {
        self.any || self.values.contains(&value)
    }
}

pub fn parse_timezone(raw: &str) -> Result<Tz> {
    Tz::from_str(raw.trim()).with_context(|| format!("unknown timezone `{raw}`"))
}

pub fn parse_window(start: &str, end: &str) -> Result<Window> {
    let start_min = parse_hhmm(start)?;
    let end_min = parse_hhmm(end)?;
    if start_min > end_min {
        bail!("active_between {start} is after {end}");
    }
    Ok(Window { start_min, end_min })
}

fn parse_hhmm(raw: &str) -> Result<u32> {
    let (hour, minute) = raw
        .trim()
        .split_once(':')
        .with_context(|| format!("clock time `{raw}` must be HH:MM"))?;
    let hour: u32 = hour.parse().with_context(|| format!("bad hour in `{raw}`"))?;
    let minute: u32 = minute.parse().with_context(|| format!("bad minute in `{raw}`"))?;
    if hour > 23 || minute > 59 {
        bail!("clock time `{raw}` is out of range");
    }
    Ok(hour * 60 + minute)
}

pub fn in_window(dt: DateTime<Tz>, window: Option<Window>) -> bool {
    let Some(window) = window else {
        return true;
    };
    let mins = dt.hour() * 60 + dt.minute();
    mins >= window.start_min && mins <= window.end_min
}

/// Most recent cron instant at or before `now` that also sits in the window.
pub fn latest_fire(cron: &Cron, window: Option<Window>, now: DateTime<Tz>) -> Option<DateTime<Tz>> {
    let mut cursor = trunc_minute(now);
    for _ in 0..(8 * 24 * 60) {
        if cron.matches(cursor) && in_window(cursor, window) {
            return Some(cursor);
        }
        cursor = add_minutes(cursor, -1)?;
    }
    None
}

/// Soonest cron instant strictly after `now` that sits in the window.
pub fn next_fire(cron: &Cron, window: Option<Window>, now: DateTime<Tz>) -> Option<DateTime<Tz>> {
    let mut cursor = add_minutes(trunc_minute(now), 1)?;
    for _ in 0..(370 * 24 * 60) {
        if cron.matches(cursor) && in_window(cursor, window) {
            return Some(cursor);
        }
        cursor = add_minutes(cursor, 1)?;
    }
    None
}

/// Fire for this local date, and only while `now` itself is inside the window.
pub fn due_fire(cron: &Cron, window: Option<Window>, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Tz>> {
    let local = now.with_timezone(&tz);
    if !in_window(local, window) {
        return None;
    }
    let fire = latest_fire(cron, window, local)?;
    if fire.date_naive() != local.date_naive() {
        return None;
    }
    Some(fire)
}

fn trunc_minute(dt: DateTime<Tz>) -> DateTime<Tz> {
    dt.with_second(0)
        .and_then(|dt| dt.with_nanosecond(0))
        .unwrap_or(dt)
}

fn add_minutes(dt: DateTime<Tz>, minutes: i64) -> Option<DateTime<Tz>> {
    dt.checked_add_signed(Duration::minutes(minutes))
}

fn parse_field(raw: &str, min: u32, max: u32, dow: bool) -> Result<Field> {
    let raw = raw.trim();
    if raw == "*" {
        return Ok(Field {
            any: true,
            values: BTreeSet::new(),
        });
    }
    let mut values = BTreeSet::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            bail!("empty cron field");
        }
        if let Some((range, step)) = part.split_once('/') {
            let step: u32 = step.parse().with_context(|| format!("bad cron step `{step}`"))?;
            if step == 0 {
                bail!("cron step is 0");
            }
            let (start, end) = if range == "*" {
                (min, if dow { 6 } else { max })
            } else {
                parse_range(range, min, max, dow)?
            };
            let mut value = start;
            while value <= end {
                values.insert(normalize(value, dow)?);
                if value == u32::MAX - step {
                    break;
                }
                value += step;
            }
        } else if part.contains('-') {
            let (start, end) = parse_range(part, min, max, dow)?;
            for value in start..=end {
                values.insert(normalize(value, dow)?);
            }
        } else {
            let value: u32 = part.parse().with_context(|| format!("bad cron value `{part}`"))?;
            values.insert(normalize(value, dow)?);
            if !dow && (value < min || value > max) {
                bail!("cron value {value} is outside {min}-{max}");
            }
        }
    }
    if values.is_empty() {
        bail!("cron field `{raw}` matched nothing");
    }
    Ok(Field { any: false, values })
}

fn parse_range(raw: &str, min: u32, max: u32, dow: bool) -> Result<(u32, u32)> {
    let (start, end) = raw.split_once('-').context("cron range")?;
    let start: u32 = start.parse().with_context(|| format!("bad cron range `{raw}`"))?;
    let end: u32 = end.parse().with_context(|| format!("bad cron range `{raw}`"))?;
    if start > end {
        bail!("cron range `{raw}` is backwards");
    }
    if !dow && (start < min || end > max) {
        bail!("cron range `{raw}` is outside {min}-{max}");
    }
    if dow && (start > 7 || end > 7) {
        bail!("day-of-week `{raw}` is outside 0-7");
    }
    Ok((start, end))
}

fn normalize(value: u32, dow: bool) -> Result<u32> {
    if dow {
        if value > 7 {
            bail!("day-of-week {value} is outside 0-7");
        }
        return Ok(if value == 7 { 0 } else { value });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Offset, TimeZone, Timelike};

    fn ny() -> Tz {
        parse_timezone("America/New_York").unwrap()
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Tz> {
        ny().with_ymd_and_hms(y, m, d, h, min, s).single().unwrap()
    }

    #[test]
    fn weekday_hour_skips_the_weekend_and_the_window_drops_nine() {
        let cron = Cron::parse("30 9-15 * * 1-5").unwrap();
        let window = parse_window("09:30", "16:00").unwrap();
        let friday = at(2026, 10, 16, 15, 30, 1);
        let next = next_fire(&cron, Some(window), friday).unwrap();
        assert_eq!(next, at(2026, 10, 19, 9, 30, 0));

        let saturday = at(2026, 10, 17, 10, 0, 0);
        let next = next_fire(&cron, Some(window), saturday).unwrap();
        assert_eq!(next, at(2026, 10, 19, 9, 30, 0));

        let hourly = Cron::parse("0 9-16 * * 1-5").unwrap();
        let monday = at(2026, 10, 19, 8, 0, 0);
        let next = next_fire(&hourly, Some(window), monday).unwrap();
        assert_eq!(next, at(2026, 10, 19, 10, 0, 0));
        let afternoon = at(2026, 10, 19, 15, 0, 0);
        let next = next_fire(&hourly, Some(window), afternoon).unwrap();
        assert_eq!(next, at(2026, 10, 19, 16, 0, 0));
    }

    #[test]
    fn spring_forward_skips_the_missing_hour_and_fall_back_keeps_both() {
        let cron = Cron::parse("30 2 * * *").unwrap();
        let before_gap = at(2026, 3, 8, 1, 0, 0);
        let next = next_fire(&cron, None, before_gap).unwrap();
        assert_eq!(next.date_naive().to_string(), "2026-03-09");
        assert_eq!((next.hour(), next.minute()), (2, 30));

        let fallback = Cron::parse("30 1 * * *").unwrap();
        let early = at(2026, 11, 1, 0, 30, 0);
        let first = next_fire(&fallback, None, early).unwrap();
        assert_eq!((first.hour(), first.minute()), (1, 30));
        assert_eq!(first.offset().fix().local_minus_utc(), -4 * 3600);
        let second = next_fire(&fallback, None, first).unwrap();
        assert_eq!((second.hour(), second.minute()), (1, 30));
        assert_eq!(second.offset().fix().local_minus_utc(), -5 * 3600);
    }
}
