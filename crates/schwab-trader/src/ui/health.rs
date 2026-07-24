use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::agent::resilience::AgentErrorClass;

#[derive(Debug, Clone, Default)]
pub struct AgentHealth {
    /// Supervisor considers the agent task alive (may be in error backoff).
    pub loop_running: bool,
    /// True while the most recent tick succeeded and we are not in failure backoff.
    pub healthy: bool,
    /// Auth requires manual `schwab auth login` before ticks can succeed.
    pub auth_fatal: bool,
    pub last_error: Option<String>,
    pub last_error_class: Option<&'static str>,
    pub consecutive_failures: u32,
    pub restart_count: u32,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
}

impl AgentHealth {
    /// True when the agent can execute sim/live exits on the next tick.
    pub fn exits_armed(&self) -> bool {
        self.loop_running && self.healthy && !self.auth_fatal
    }

    pub fn status_label(&self) -> &'static str {
        if !self.loop_running {
            "stopped"
        } else if self.auth_fatal {
            "auth_down"
        } else if !self.healthy {
            "degraded"
        } else {
            "running"
        }
    }

    pub fn record_tick_ok(&mut self) {
        self.loop_running = true;
        self.healthy = true;
        self.auth_fatal = false;
        self.last_error = None;
        self.last_error_class = None;
        self.consecutive_failures = 0;
        self.last_success_at = Some(Utc::now());
    }

    pub fn record_tick_err(&mut self, msg: String, class: AgentErrorClass) {
        self.loop_running = true;
        self.healthy = false;
        self.auth_fatal = matches!(class, AgentErrorClass::AuthFatal);
        self.last_error = Some(msg);
        self.last_error_class = Some(crate::agent::resilience::class_label(class));
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure_at = Some(Utc::now());
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

pub type SharedAgentHealth = Arc<Mutex<AgentHealth>>;

pub fn new_shared_health() -> SharedAgentHealth {
    Arc::new(Mutex::new(AgentHealth {
        loop_running: true,
        healthy: false,
        ..AgentHealth::default()
    }))
}

pub fn update_health(health: &SharedAgentHealth, f: impl FnOnce(&mut AgentHealth)) {
    if let Ok(mut g) = health.lock() {
        f(&mut g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::resilience::AgentErrorClass;

    #[test]
    fn exits_armed_requires_healthy_loop() {
        let mut h = AgentHealth {
            loop_running: true,
            healthy: true,
            ..AgentHealth::default()
        };
        assert!(h.exits_armed());
        h.record_tick_err("API error 401".into(), AgentErrorClass::Recoverable);
        assert!(!h.exits_armed());
        assert_eq!(h.status_label(), "degraded");
        h.record_tick_ok();
        assert!(h.exits_armed());
        h.record_tick_err("invalid_grant".into(), AgentErrorClass::AuthFatal);
        assert!(!h.exits_armed());
        assert_eq!(h.status_label(), "auth_down");
    }
}
