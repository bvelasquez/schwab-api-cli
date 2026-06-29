use anyhow::Result;
use serde_json::json;

use crate::cli::JournalCommands;
use crate::config::TraderRuntime;
use crate::journal;

pub async fn run(runtime: &TraderRuntime, command: JournalCommands) -> Result<()> {
    match command {
        JournalCommands::List { rules_file, limit } => {
            let events = journal::read_recent(&rules_file, limit)?;
            runtime.emit(
                schwab_cli::output::ResponseEnvelope::ok("trader journal list", json!({ "events": events }))
                    .with_inputs(json!({ "rules_file": rules_file, "limit": limit })),
            );
        }
        JournalCommands::Stats { rules_file } => {
            let stats = journal::stats_from_journal(&rules_file)?;
            runtime.emit(
                schwab_cli::output::ResponseEnvelope::ok("trader journal stats", stats)
                    .with_inputs(json!({ "rules_file": rules_file })),
            );
        }
    }
    Ok(())
}
