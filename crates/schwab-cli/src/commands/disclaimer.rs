use anyhow::Result;

use crate::cli::DisclaimerCommands;
use crate::config::RuntimeConfig;
use crate::disclaimer::{self, FULL_DISCLAIMER};
use crate::output::{OutputFormat, ResponseEnvelope};
use crate::safety::require_mutation_approval;

pub async fn run(runtime: &RuntimeConfig, command: DisclaimerCommands) -> Result<()> {
    match command {
        DisclaimerCommands::Show => {
            if runtime.output == OutputFormat::Json {
                runtime.emit(ResponseEnvelope::ok(
                    "disclaimer show",
                    serde_json::json!({
                        "disclaimer": FULL_DISCLAIMER,
                        "status": disclaimer::status_json(),
                    }),
                ));
            } else {
                println!("{FULL_DISCLAIMER}");
                println!("Status: {}", disclaimer::status_json());
            }
            Ok(())
        }
        DisclaimerCommands::Accept => {
            require_mutation_approval(
                runtime,
                "disclaimer accept",
                "Record that you accept the trading risk disclaimer.",
            )?;
            let path = disclaimer::mark_accepted()?;
            runtime.emit(ResponseEnvelope::ok(
                "disclaimer accept",
                serde_json::json!({
                    "accepted": true,
                    "marker_path": path,
                    "message": "Disclaimer accepted. Live trading is permitted when other safety checks pass.",
                }),
            ));
            Ok(())
        }
        DisclaimerCommands::Status => {
            runtime.emit(ResponseEnvelope::ok(
                "disclaimer status",
                disclaimer::status_json(),
            ));
            Ok(())
        }
    }
}
