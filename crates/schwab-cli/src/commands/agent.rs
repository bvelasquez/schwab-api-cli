use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::agent::{default_state_path, load_state, log_path, pid_path, run_agent_loop, spawn_background, state_summary, stop_daemon};
use crate::cli::AgentCommands;
use crate::config::RuntimeConfig;
use crate::output::ResponseEnvelope;
use crate::rules::{rules_json_schema, RulesConfig};

pub async fn run(runtime: &RuntimeConfig, command: AgentCommands) -> Result<()> {
    match command {
        AgentCommands::Schema => {
            runtime.emit(ResponseEnvelope::ok("agent schema", rules_json_schema()));
        }
        AgentCommands::Validate { file } => {
            let rules = RulesConfig::load(&file)?;
            runtime.emit(ResponseEnvelope::ok(
                "agent validate",
                json!({
                    "valid": true,
                    "agent_id": rules.agent_id,
                    "accounts": rules.accounts.len(),
                    "watchlist": rules.watchlist,
                    "llm_enabled": rules.llm.enabled,
                    "telegram_enabled": rules.notify.telegram.enabled,
                }),
            ));
        }
        AgentCommands::Status { rules_file } => {
            let path = rules_file
                .map(|p| default_state_path(&p))
                .unwrap_or_else(|| {
                    directories::ProjectDirs::from("com", "schwabinvestbot", "schwab")
                        .map(|d| d.data_local_dir().join("agent-state.json"))
                        .unwrap_or_else(|| PathBuf::from("agent-state.json"))
                });
            let state = load_state(&path)?;
            runtime.emit(ResponseEnvelope::ok(
                "agent status",
                json!({ "state_path": path, "state": state_summary(&state) }),
            ));
        }
        AgentCommands::Run { file, once, background } => {
            if background {
                if once {
                    anyhow::bail!("--background cannot be combined with --once");
                }
                let mut extra = Vec::new();
                if runtime.dry_run {
                    extra.push("--dry-run".into());
                }
                if runtime.trust {
                    extra.push("--trust".into());
                }
                if runtime.yes {
                    extra.push("--yes".into());
                }
                if runtime.output == crate::output::OutputFormat::Json {
                    extra.push("--json".into());
                }
                let pid = spawn_background(&file, &extra)?;
                runtime.emit(ResponseEnvelope::ok(
                    "agent background",
                    json!({
                        "pid": pid,
                        "pid_file": pid_path(&file),
                        "log_file": log_path(&file),
                        "rules": file,
                    }),
                ));
                return Ok(());
            }
            run_agent_loop(runtime, &file, once).await?;
        }
        AgentCommands::Stop { file } => {
            stop_daemon(&file)?;
            runtime.emit(ResponseEnvelope::ok(
                "agent stop",
                json!({ "stopped": true, "rules": file }),
            ));
        }
    }
    Ok(())
}
