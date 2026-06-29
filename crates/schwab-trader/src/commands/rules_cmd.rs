use anyhow::Result;
use serde_json::json;

use crate::cli::RulesCommands;
use crate::config::TraderRuntime;
use crate::rules::{validate_rules_file, TraderRules};

pub async fn run(runtime: &TraderRuntime, command: RulesCommands) -> Result<()> {
    match command {
        RulesCommands::Validate { rules_file } => {
            let data = validate_rules_file(&rules_file)?;
            runtime.emit(
                schwab_cli::output::ResponseEnvelope::ok("trader rules validate", data)
                    .with_inputs(json!({ "rules_file": rules_file })),
            );
        }
        RulesCommands::Show { rules_file } => {
            let rules = TraderRules::load(&rules_file)?;
            runtime.emit(
                schwab_cli::output::ResponseEnvelope::ok(
                    "trader rules show",
                    serde_json::to_value(&rules)?,
                )
                .with_inputs(json!({ "rules_file": rules_file })),
            );
        }
    }
    Ok(())
}
