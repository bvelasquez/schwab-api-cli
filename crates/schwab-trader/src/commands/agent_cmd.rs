use anyhow::{Context, Result};
use serde_json::json;

use crate::agent::runner::{run_agent_loop, AgentRunOptions};
use crate::agent::state::load_state;
use crate::cli::AgentCommands;
use crate::config::TraderRuntime;
use crate::rules::TraderRules;

pub async fn run(runtime: &TraderRuntime, command: AgentCommands) -> Result<()> {
    match command {
        AgentCommands::Run {
            rules_file,
            once,
            background,
        } => {
            if background {
                anyhow::bail!("background mode not implemented yet; run in foreground");
            }
            run_agent_loop(
                runtime,
                &rules_file,
                AgentRunOptions {
                    once,
                    health: None,
                },
            )
            .await
        }
        AgentCommands::Status { rules_file } => {
            let rules = TraderRules::load(&rules_file)?;
            let state = load_state(&rules_file, &rules.trader_id)?;
            runtime.emit(
                schwab_cli::output::ResponseEnvelope::ok(
                    "trader agent status",
                    json!({
                        "trader_id": rules.trader_id,
                        "state": state.summary(),
                        "state_path": crate::agent::paths::state_path(&rules_file),
                    }),
                )
                .with_inputs(json!({ "rules_file": rules_file })),
            );
            Ok(())
        }
        AgentCommands::Reload { rules_file } => {
            let pid_file = crate::agent::paths::pid_path(&rules_file);
            let pid = schwab_cli::rules_reload::send_sighup_to_pid_file(&pid_file)
                .with_context(|| {
                    format!(
                        "no running trader pid at {} — edit the YAML (loop polls within 1s) or send SIGHUP to the watch process",
                        pid_file.display()
                    )
                })?;
            runtime.emit(
                schwab_cli::output::ResponseEnvelope::ok(
                    "trader agent reload",
                    json!({
                        "signalled": "SIGHUP",
                        "pid": pid,
                        "rules_file": rules_file,
                        "note": "process keeps running; invalid YAML keeps previous rules",
                    }),
                )
                .with_inputs(json!({ "rules_file": rules_file })),
            );
            Ok(())
        }
    }
}
