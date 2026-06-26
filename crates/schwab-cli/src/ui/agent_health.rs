use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Shared between the embedded agent task and the watch TUI.
#[derive(Debug, Clone)]
pub struct AgentWatchHealth {
    pub loop_running: bool,
    pub started_at: Instant,
    pub ticks_completed: u64,
    pub last_error: Option<String>,
    /// Set when Schwab OAuth cannot be refreshed — user must `schwab auth login`.
    pub auth_required: bool,
}

impl AgentWatchHealth {
    pub fn starting() -> Self {
        Self {
            loop_running: true,
            started_at: Instant::now(),
            ticks_completed: 0,
            last_error: None,
            auth_required: false,
        }
    }

    pub fn record_tick(&mut self) {
        self.ticks_completed += 1;
        self.last_error = None;
        self.auth_required = false;
    }

    pub fn record_error(&mut self, err: &str) {
        self.last_error = Some(err.to_string());
        if is_fatal_auth_error(err) {
            self.auth_required = true;
            self.loop_running = false;
        }
    }
}

pub type SharedAgentHealth = Arc<Mutex<AgentWatchHealth>>;

pub fn new_shared_health() -> SharedAgentHealth {
    Arc::new(Mutex::new(AgentWatchHealth::starting()))
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

pub fn is_fatal_auth_error(err: &str) -> bool {
    err.contains("invalid_grant")
        || err.contains("Refresh token is invalid")
        || err.contains("No refresh token on disk")
        || err.contains("Run `schwab auth login`")
        || err.contains("Not authenticated")
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
}
