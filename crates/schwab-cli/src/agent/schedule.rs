use chrono::{DateTime, Utc};

use crate::rules::{OvernightConfig, ScheduleConfig};

use super::state::AgentState;

/// Agent operating mode for the current wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSession {
    /// Option market open — full reconcile, marks, entries, monitor LLM.
    RegularHours,
    /// Market closed with overnight digest enabled — reconcile + optional web digest.
    Overnight,
    /// Market closed and overnight disabled — reconcile + hours check only.
    Idle,
}

impl AgentSession {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RegularHours => "regular",
            Self::Overnight => "overnight",
            Self::Idle => "idle",
        }
    }
}

pub struct SessionTransition {
    pub session: AgentSession,
    pub just_opened: bool,
    pub sleep_seconds: u64,
}

/// Resolve session from market hours flag and schedule config.
pub fn resolve_session(
    market_open: bool,
    schedule: &ScheduleConfig,
    previous_session: Option<&str>,
) -> SessionTransition {
    if market_open {
        let just_opened = matches!(previous_session, Some("overnight") | Some("idle"));
        return SessionTransition {
            session: AgentSession::RegularHours,
            just_opened,
            sleep_seconds: schedule.tick_interval_seconds.max(5),
        };
    }

    if schedule.overnight.enabled {
        SessionTransition {
            session: AgentSession::Overnight,
            just_opened: false,
            sleep_seconds: schedule.overnight.tick_interval_seconds.max(300),
        }
    } else {
        SessionTransition {
            session: AgentSession::Idle,
            just_opened: false,
            sleep_seconds: schedule.tick_interval_seconds.max(5),
        }
    }
}

pub fn should_run_overnight_digest(
    state: &AgentState,
    overnight: &OvernightConfig,
    now: DateTime<Utc>,
) -> bool {
    if !overnight.web_digest {
        return false;
    }
    if overnight.skip_llm_when_flat && state.open_positions.is_empty() {
        return false;
    }

    let min_secs = overnight.tick_interval_seconds.max(300);
    match state.last_overnight_digest_at {
        None => true,
        Some(at) => (now - at).num_seconds() >= min_secs as i64,
    }
}

pub fn should_run_monitor_review(
    regular_tick_count: u64,
    last_llm_tick: Option<u64>,
    every: u64,
) -> bool {
    let every = every.max(1);
    match last_llm_tick {
        None => true,
        Some(last) => regular_tick_count.saturating_sub(last) >= every,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule_with_overnight() -> ScheduleConfig {
        ScheduleConfig {
            tick_interval_seconds: 120,
            market_hours_only: true,
            timezone: "America/New_York".into(),
            overnight: OvernightConfig {
                enabled: true,
                tick_interval_seconds: 3600,
                web_digest: true,
                skip_llm_when_flat: true,
                alert_on_risk_only: true,
            },
        }
    }

    #[test]
    fn closed_with_overnight_is_overnight_session() {
        let t = resolve_session(false, &schedule_with_overnight(), Some("regular"));
        assert_eq!(t.session, AgentSession::Overnight);
        assert_eq!(t.sleep_seconds, 3600);
    }

    #[test]
    fn open_after_overnight_sets_just_opened() {
        let t = resolve_session(true, &schedule_with_overnight(), Some("overnight"));
        assert!(t.just_opened);
        assert_eq!(t.session, AgentSession::RegularHours);
    }

    #[test]
    fn closed_without_overnight_is_idle() {
        let schedule = ScheduleConfig::default();
        let t = resolve_session(false, &schedule, None);
        assert_eq!(t.session, AgentSession::Idle);
    }

    #[test]
    fn overnight_digest_respects_interval() {
        let cfg = OvernightConfig {
            enabled: true,
            tick_interval_seconds: 3600,
            web_digest: true,
            skip_llm_when_flat: false,
            alert_on_risk_only: true,
        };
        let mut state = AgentState::default();
        state.open_positions.insert(
            "IWM|2026-07-31".into(),
            super::super::state::TrackedPosition {
                position_id: "IWM|2026-07-31".into(),
                account_hash: "x".into(),
                underlying: "IWM".into(),
                expiry: "2026-07-31".into(),
                strategy: "vertical".into(),
                opened_at: Utc::now(),
                entry_credit: Some(0.25),
                max_loss_usd: 175.0,
                contracts: 1,
                entry_params: None,
            },
        );
        assert!(should_run_overnight_digest(&state, &cfg, Utc::now()));
        state.last_overnight_digest_at = Some(Utc::now());
        assert!(!should_run_overnight_digest(&state, &cfg, Utc::now()));
    }
}
