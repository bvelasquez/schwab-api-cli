//! Interpret Schwab `/markets` hours responses for session-aware open/closed checks.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// Whether equity options (EQO) are in **regular** session right now.
///
/// Schwab's `isOpen` flag can stay `true` outside regular hours (e.g. after 4:00 PM ET
/// on a trading day). Prefer `sessionHours.regularMarket` windows when present.
pub fn eqo_regular_session_open(hours: &Value, now: DateTime<Utc>) -> Option<bool> {
    let windows = eqo_regular_windows(hours)?;
    Some(in_any_window(&windows, now))
}

fn eqo_regular_windows(hours: &Value) -> Option<Vec<(DateTime<chrono::FixedOffset>, DateTime<chrono::FixedOffset>)>> {
    let raw = hours
        .pointer("/option/EQO/sessionHours/regularMarket")
        .or_else(|| hours.pointer("/option/option/sessionHours/regularMarket"))?
        .as_array()?;

    let mut windows = Vec::new();
    for w in raw {
        let start_s = w.get("start")?.as_str()?;
        let end_s = w.get("end")?.as_str()?;
        let start = DateTime::parse_from_rfc3339(start_s).ok()?;
        let end = DateTime::parse_from_rfc3339(end_s).ok()?;
        windows.push((start, end));
    }
    if windows.is_empty() {
        None
    } else {
        Some(windows)
    }
}

fn in_any_window(
    windows: &[(DateTime<chrono::FixedOffset>, DateTime<chrono::FixedOffset>)],
    now: DateTime<Utc>,
) -> bool {
    for (start, end) in windows {
        let t = now.with_timezone(&start.timezone());
        if t >= *start && t <= *end {
            return true;
        }
    }
    false
}

/// Best-effort EQO regular-session open flag from a Schwab hours payload.
pub fn option_market_open_from_hours(hours: &Value, now: DateTime<Utc>) -> Option<bool> {
    if let Some(open) = eqo_regular_session_open(hours, now) {
        return Some(open);
    }
    hours
        .pointer("/option/EQO/isOpen")
        .or_else(|| hours.pointer("/option/option/isOpen"))
        .and_then(|v| v.as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn regular_window_closed_after_4pm_et() {
        let hours = json!({
            "option": {
                "EQO": {
                    "isOpen": true,
                    "sessionHours": {
                        "regularMarket": [{
                            "start": "2026-06-25T09:30:00-04:00",
                            "end": "2026-06-25T16:00:00-04:00"
                        }]
                    }
                }
            }
        });
        let after_close = DateTime::parse_from_rfc3339("2026-06-25T17:48:00-04:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(option_market_open_from_hours(&hours, after_close), Some(false));
    }

    #[test]
    fn regular_window_open_midday_et() {
        let hours = json!({
            "option": {
                "EQO": {
                    "isOpen": true,
                    "sessionHours": {
                        "regularMarket": [{
                            "start": "2026-06-25T09:30:00-04:00",
                            "end": "2026-06-25T16:00:00-04:00"
                        }]
                    }
                }
            }
        });
        let midday = DateTime::parse_from_rfc3339("2026-06-25T12:00:00-04:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(option_market_open_from_hours(&hours, midday), Some(true));
    }

    #[test]
    fn pst_248pm_is_closed() {
        let hours = json!({
            "option": {
                "EQO": {
                    "isOpen": true,
                    "sessionHours": {
                        "regularMarket": [{
                            "start": "2026-06-25T09:30:00-04:00",
                            "end": "2026-06-25T16:00:00-04:00"
                        }]
                    }
                }
            }
        });
        // 2:48 PM PDT = 5:48 PM EDT
        let pst = DateTime::parse_from_rfc3339("2026-06-25T14:48:00-07:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(option_market_open_from_hours(&hours, pst), Some(false));
    }
}
