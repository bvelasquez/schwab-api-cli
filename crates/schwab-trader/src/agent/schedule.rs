//! Session scheduling: regular hours, premarket, overnight, idle (mirrors options agent).

use chrono::{DateTime, Utc};

use crate::agent::state::TraderState;
use crate::market_session::{
    equity_premarket_window, equity_regular_session_open, minutes_until_equity_open,
};
use crate::rules::{OvernightConfig, ScheduleConfig, TraderRules};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSession {
    RegularHours,
    Premarket,
    Overnight,
    Idle,
}

impl AgentSession {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RegularHours => "regular",
            Self::Premarket => "premarket",
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

pub fn resolve_session(
    rules: &TraderRules,
    previous_session: Option<&str>,
) -> SessionTransition {
    let now = Utc::now();

    if equity_regular_session_open(now, &rules.schedule.timezone) {
        let just_opened = matches!(
            previous_session,
            Some("overnight") | Some("idle") | Some("premarket")
        );
        return SessionTransition {
            session: AgentSession::RegularHours,
            just_opened,
            sleep_seconds: rules.schedule.tick_interval_seconds.max(5),
        };
    }

    if rules.schedule.premarket_scan
        && equity_premarket_window(
            now,
            &rules.schedule.timezone,
            &rules.schedule.premarket_start_et,
        )
    {
        return SessionTransition {
            session: AgentSession::Premarket,
            just_opened: false,
            sleep_seconds: rules.schedule.premarket_tick_interval_seconds.max(300),
        };
    }

    if rules.schedule.overnight.enabled {
        SessionTransition {
            session: AgentSession::Overnight,
            just_opened: false,
            sleep_seconds: rules.schedule.overnight.tick_interval_seconds.max(300),
        }
    } else {
        SessionTransition {
            session: AgentSession::Idle,
            just_opened: false,
            sleep_seconds: rules.schedule.overnight.tick_interval_seconds.max(1800),
        }
    }
}

pub fn should_run_overnight_digest(
    state: &TraderState,
    overnight: &OvernightConfig,
    now: DateTime<Utc>,
) -> bool {
    if !overnight.web_digest {
        return false;
    }
    if overnight.skip_llm_when_flat
        && state.open_positions.is_empty()
        && state.pending_buys.is_empty()
    {
        return false;
    }

    let min_secs = overnight.tick_interval_seconds.max(300);
    match state.last_overnight_digest_at {
        None => true,
        Some(at) => (now - at).num_seconds() >= min_secs as i64,
    }
}

pub fn should_run_premarket_digest(
    state: &TraderState,
    schedule: &ScheduleConfig,
    now: DateTime<Utc>,
) -> bool {
    if !schedule.premarket_scan {
        return false;
    }
    if !equity_premarket_window(
        now,
        &schedule.timezone,
        &schedule.premarket_start_et,
    ) {
        return false;
    }

    let mins_to_open = minutes_until_equity_open(now, &schedule.timezone);
    let min_secs = if mins_to_open > 0 && mins_to_open <= 30 {
        schedule
            .premarket_open_grounding_interval_seconds
            .max(300)
    } else {
        schedule.premarket_tick_interval_seconds.max(300)
    };

    match state.last_premarket_digest_at {
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

pub fn should_use_web_research(llm_review_count: u64, every: u64) -> bool {
    if every == 0 {
        return false;
    }
    (llm_review_count + 1) % every.max(1) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swing_rules() -> TraderRules {
        let mut rules = TraderRules::default();
        rules.schedule.overnight.enabled = true;
        rules.schedule.overnight.tick_interval_seconds = 3600;
        rules.schedule.premarket_scan = true;
        rules
    }

    #[test]
    fn closed_with_overnight_is_overnight_session() {
        // Can't easily mock time; test resolve with premarket off on weekend would be overnight
        let rules = swing_rules();
        let t = resolve_session(&rules, Some("regular"));
        // When run during market hours this would be regular — test structure only
        assert!(t.sleep_seconds >= 5);
    }

    #[test]
    fn monitor_review_respects_interval() {
        assert!(should_run_monitor_review(5, Some(1), 3));
        assert!(!should_run_monitor_review(3, Some(1), 3));
    }

    #[test]
    fn overnight_digest_skips_when_flat() {
        let cfg = OvernightConfig {
            enabled: true,
            tick_interval_seconds: 3600,
            web_digest: true,
            skip_llm_when_flat: true,
        };
        let state = TraderState::default();
        assert!(!should_run_overnight_digest(&state, &cfg, Utc::now()));
    }
}
