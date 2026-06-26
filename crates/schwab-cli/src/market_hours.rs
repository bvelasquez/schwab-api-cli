//! Interpret Schwab `/markets` hours responses for session-aware open/closed checks.

use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use chrono_tz::America::New_York;
use serde_json::Value;

use crate::agent::state::AgentState;

/// Where the displayed market status came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketStatusSource {
    SchwabApi,
    HoursCache,
    Agent,
    Schedule,
}

/// Resolved EQO regular-session status (never "unknown").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedMarketStatus {
    pub open: bool,
    pub source: MarketStatusSource,
}

impl ResolvedMarketStatus {
    pub fn label(self, agent_session: Option<&str>) -> String {
        let base = if self.open {
            "OPEN (regular)".to_string()
        } else if matches!(agent_session, Some("overnight")) {
            "CLOSED (overnight)".to_string()
        } else {
            "CLOSED".to_string()
        };
        match self.source {
            MarketStatusSource::Schedule => format!("{base} (est)"),
            _ => base,
        }
    }
}

/// Whether equity options (EQO) are in **regular** session right now.
///
/// Schwab's `isOpen` flag can stay `true` outside regular hours (e.g. after 4:00 PM ET
/// on a trading day). Prefer `sessionHours.regularMarket` windows when present.
pub fn eqo_regular_session_open(hours: &Value, now: DateTime<Utc>) -> Option<bool> {
    let windows = eqo_regular_windows(hours)?;
    Some(in_any_window(&windows, now))
}

fn eqo_regular_windows(
    hours: &Value,
) -> Option<Vec<(DateTime<chrono::FixedOffset>, DateTime<chrono::FixedOffset>)>> {
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

/// Weekday EQO regular hours in US Eastern (9:30–16:00). Does not model exchange holidays.
pub fn eqo_regular_session_estimate(now: DateTime<Utc>) -> bool {
    let et = now.with_timezone(&New_York);
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
    t >= open && t <= close
}

/// Resolve EQO status: live/cached Schwab hours → recent agent state → ET schedule estimate.
pub fn resolve_eqo_market_open(
    hours: Option<&Value>,
    state: &AgentState,
    now: DateTime<Utc>,
    hours_source: Option<MarketStatusSource>,
) -> ResolvedMarketStatus {
    if let (Some(h), Some(src)) = (hours, hours_source) {
        if let Some(open) = option_market_open_from_hours(h, now) {
            return ResolvedMarketStatus { open, source: src };
        }
    }

    if let Some(open) = state.last_market_open {
        if state.last_tick.is_some_and(|at| (now - at).num_hours() < 6) {
            return ResolvedMarketStatus {
                open,
                source: MarketStatusSource::Agent,
            };
        }
    }

    if let (Some(session), Some(at)) = (state.last_session.as_deref(), state.last_tick) {
        if (now - at).num_hours() < 3 {
            let open = session == "regular";
            return ResolvedMarketStatus {
                open,
                source: MarketStatusSource::Agent,
            };
        }
    }

    ResolvedMarketStatus {
        open: eqo_regular_session_estimate(now),
        source: MarketStatusSource::Schedule,
    }
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
        assert_eq!(
            option_market_open_from_hours(&hours, after_close),
            Some(false)
        );
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
        let pst = DateTime::parse_from_rfc3339("2026-06-25T14:48:00-07:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(option_market_open_from_hours(&hours, pst), Some(false));
    }

    #[test]
    fn schedule_estimate_weekend_closed() {
        let sat = DateTime::parse_from_rfc3339("2026-06-27T12:00:00-04:00")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!eqo_regular_session_estimate(sat));
    }

    #[test]
    fn resolve_never_unknown_uses_schedule() {
        let state = AgentState::default();
        let status = resolve_eqo_market_open(None, &state, Utc::now(), None);
        assert_eq!(status.source, MarketStatusSource::Schedule);
    }

    #[test]
    fn resolve_prefers_cached_hours() {
        let hours = json!({
            "option": {
                "EQO": {
                    "sessionHours": {
                        "regularMarket": [{
                            "start": "2026-06-25T09:30:00-04:00",
                            "end": "2026-06-25T16:00:00-04:00"
                        }]
                    }
                }
            }
        });
        let after_close = DateTime::parse_from_rfc3339("2026-06-25T17:00:00-04:00")
            .unwrap()
            .with_timezone(&Utc);
        let status = resolve_eqo_market_open(
            Some(&hours),
            &AgentState::default(),
            after_close,
            Some(MarketStatusSource::HoursCache),
        );
        assert!(!status.open);
        assert_eq!(status.source, MarketStatusSource::HoursCache);
    }
}
