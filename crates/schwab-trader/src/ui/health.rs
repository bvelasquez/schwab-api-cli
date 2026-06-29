use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct AgentHealth {
    pub loop_running: bool,
    pub last_error: Option<String>,
}

pub type SharedAgentHealth = Arc<Mutex<AgentHealth>>;

pub fn new_shared_health() -> SharedAgentHealth {
    Arc::new(Mutex::new(AgentHealth {
        loop_running: true,
        last_error: None,
    }))
}
