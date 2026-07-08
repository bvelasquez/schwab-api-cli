use std::path::{Path, PathBuf};

/// Stable filename stem from the rules YAML path (e.g. `options-rules.example`).
pub fn rules_runtime_stem(rules_path: &Path) -> String {
    rules_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("agent")
        .to_string()
}

fn rules_runtime_dir(rules_path: &Path) -> PathBuf {
    rules_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_state_path(rules_path: &Path) -> PathBuf {
    let dir = rules_runtime_dir(rules_path);
    let stem = rules_runtime_stem(rules_path);
    dir.join(format!("agent-state-{stem}.json"))
}

pub fn sim_state_path(rules_path: &Path) -> PathBuf {
    let dir = rules_runtime_dir(rules_path);
    let stem = rules_runtime_stem(rules_path);
    dir.join(format!("agent-sim-state-{stem}.json"))
}

pub fn journal_path(rules_path: &Path) -> PathBuf {
    let dir = rules_runtime_dir(rules_path);
    let stem = rules_runtime_stem(rules_path);
    dir.join(format!("agent-journal-{stem}.jsonl"))
}

pub fn sim_journal_path(rules_path: &Path) -> PathBuf {
    let dir = rules_runtime_dir(rules_path);
    let stem = rules_runtime_stem(rules_path);
    dir.join(format!("agent-sim-journal-{stem}.jsonl"))
}

pub fn active_state_path(rules_path: &Path, simulate: bool) -> PathBuf {
    if simulate {
        sim_state_path(rules_path)
    } else {
        default_state_path(rules_path)
    }
}

pub fn pid_path(rules_path: &Path) -> PathBuf {
    let dir = rules_runtime_dir(rules_path);
    let stem = rules_runtime_stem(rules_path);
    dir.join(format!("agent-{stem}.pid"))
}

pub fn log_path(rules_path: &Path) -> PathBuf {
    let dir = rules_runtime_dir(rules_path);
    let stem = rules_runtime_stem(rules_path);
    dir.join(format!("agent-{stem}.log"))
}

/// Append a line to the per-rules agent log (used by background daemon stdout and watch-mode ticks).
pub fn append_agent_log(rules_path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let log = log_path(rules_path);
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Load persisted state for a rules file, migrating legacy `agent-state.json` when agent_id matches.
pub fn load_agent_state(rules_path: &Path, agent_id: &str) -> super::state::AgentState {
    load_agent_state_from(rules_path, agent_id, false)
}

pub fn load_sim_agent_state(rules_path: &Path, agent_id: &str) -> super::state::AgentState {
    load_agent_state_from(rules_path, agent_id, true)
}

fn load_agent_state_from(rules_path: &Path, agent_id: &str, simulate: bool) -> super::state::AgentState {
    use super::state::{load_state, save_state};

    let state_path = active_state_path(rules_path, simulate);
    if state_path.exists() {
        return load_state(&state_path).unwrap_or_default();
    }

    if simulate {
        return load_state(&state_path).unwrap_or_default();
    }

    let legacy = rules_runtime_dir(rules_path).join("agent-state.json");
    if legacy.exists() {
        if let Ok(state) = load_state(&legacy) {
            if state.agent_id.is_empty() || state.agent_id == agent_id {
                let canonical = default_state_path(rules_path);
                let _ = save_state(&canonical, &state);
                return state;
            }
        }
    }

    load_state(&state_path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn runtime_paths_derive_from_rules_filename() {
        let rules = Path::new("rules/options-rules.example.yaml");
        assert_eq!(
            default_state_path(rules),
            Path::new("rules/agent-state-options-rules.example.json")
        );
        assert_eq!(
            pid_path(rules),
            Path::new("rules/agent-options-rules.example.pid")
        );
        assert_eq!(
            log_path(rules),
            Path::new("rules/agent-options-rules.example.log")
        );
    }

    #[test]
    fn distinct_rules_files_get_distinct_state_paths() {
        let a = default_state_path(Path::new("rules/options-rules.example.yaml"));
        let b = default_state_path(Path::new("rules/my-options.yaml"));
        assert_ne!(a, b);
    }
}
