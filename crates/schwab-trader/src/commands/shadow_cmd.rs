use anyhow::Result;
use schwab_cli::output::OutputFormat;
use serde_json::json;

use crate::cli::ShadowCommands;
use crate::config::TraderRuntime;
use crate::rules::TraderRules;
use crate::shadow::report::{build_report, format_text};

pub async fn run(runtime: &TraderRuntime, command: ShadowCommands) -> Result<()> {
    match command {
        ShadowCommands::Report { rules_file } => {
            let rules = TraderRules::load(&rules_file)?;
            let report = build_report(&rules_file, &rules)?;
            if matches!(runtime.output, OutputFormat::Json) {
                runtime.emit(
                    schwab_cli::output::ResponseEnvelope::ok("trader shadow report", report)
                        .with_inputs(json!({ "rules_file": rules_file })),
                );
            } else {
                print!("{}", format_text(&report));
            }
            Ok(())
        }
    }
}
