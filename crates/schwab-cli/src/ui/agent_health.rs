use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};

/// Shared between the embedded agent task and the watch TUI.
#[derive(Debug, Clone)]
pub struct AgentWatchHealth {
    pub loop_running: bool,
    /// Last tick succeeded; false while in error backoff.
    pub healthy: bool,
    pub started_at: Instant,
    pub ticks_completed: u64,
    pub last_error: Option<String>,
    /// Set when Schwab OAuth cannot be refreshed — user must `schwab auth login`.
    pub auth_required: bool,
    pub consecutive_failures: u32,
    pub restart_count: u32,
    pub last_success_at: Option<DateTime<Utc>>,
}

impl AgentWatchHealth {
    pub fn starting() -> Self {
        Self {
            loop_running: true,
            healthy: false,
            started_at: Instant::now(),
            ticks_completed: 0,
            last_error: None,
            auth_required: false,
            consecutive_failures: 0,
            restart_count: 0,
            last_success_at: None,
        }
    }

    /// True when the agent can place/close option orders on the next tick.
    pub fn exits_armed(&self) -> bool {
        self.loop_running && self.healthy && !self.auth_required
    }

    pub fn status_label(&self) -> &'static str {
        if !self.loop_running {
            "stopped"
        } else if self.auth_required {
            "auth_down"
        } else if !self.healthy {
            "degraded"
        } else {
            "running"
        }
    }

    pub fn record_tick(&mut self) {
        self.ticks_completed += 1;
        self.loop_running = true;
        self.healthy = true;
        self.last_error = None;
        self.auth_required = false;
        self.consecutive_failures = 0;
        self.last_success_at = Some(Utc::now());
    }

    pub fn record_error(&mut self, err: &str) {
        self.last_error = Some(err.to_string());
        self.healthy = false;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if is_fatal_auth_error(err) {
            self.auth_required = true;
            // Keep loop_running true — agent stays up and retries after re-login.
        }
    }

    pub fn record_loop_stopped(&mut self, msg: Option<String>) {
        self.loop_running = false;
        self.healthy = false;
        if let Some(m) = msg {
            self.last_error = Some(m);
        }
    }

    pub fn record_supervisor_restart(&mut self) {
        self.restart_count = self.restart_count.saturating_add(1);
        self.loop_running = true;
        self.healthy = false;
    }
}

pub type SharedAgentHealth = Arc<Mutex<AgentWatchHealth>>;

pub fn new_shared_health() -> SharedAgentHealth {
    Arc::new(Mutex::new(AgentWatchHealth::starting()))
}

pub fn update_health(health: &SharedAgentHealth, f: impl FnOnce(&mut AgentWatchHealth)) {
    if let Ok(mut g) = health.lock() {
        f(&mut g);
    }
}

pub fn format_tick_error(err: &str) -> String {
    if is_fatal_auth_error(err) {
        return "Schwab login required — run: schwab auth login".to_string();
    }
    let needs_login = err.contains("OAuth error");
    if needs_login && !err.contains("auth login") {
        format!("{err} → run: schwab auth login")
    } else {
        err.to_string()
    }
}

/// Delegates to the 3-tier classifier in `agent::resilience` (case-insensitive superset of
/// the original exact-case patterns) so this crate has a single source of truth for what
/// counts as an auth-fatal error, instead of two matchers that could silently drift apart.
pub fn is_fatal_auth_error(err: &str) -> bool {
    crate::agent::resilience::classify_error_message(err)
        == crate::agent::resilience::AgentErrorClass::AuthFatal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_invalid_grant() {
        let err = r#"OAuth error: HTTP 400: {"error":"invalid_grant"}"#;
        assert!(is_fatal_auth_error(err));
        assert!(format_tick_error(err).contains("schwab auth login"));
    }

    #[test]
    fn auth_error_keeps_loop_running_but_disarms_exits() {
        let mut h = AgentWatchHealth::starting();
        h.record_tick();
        assert!(h.exits_armed());
        h.record_error(r#"OAuth error: invalid_grant"#);
        assert!(h.loop_running);
        assert!(h.auth_required);
        assert!(!h.exits_armed());
        assert_eq!(h.status_label(), "auth_down");
    }
}
