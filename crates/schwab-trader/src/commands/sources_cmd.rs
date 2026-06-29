use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::SourcesCommands;
use crate::config::TraderRuntime;
use crate::rules::TraderRules;
use crate::sources::{catalog_json, fetch_all_enabled, fetch_feeds_for_phase};

pub async fn run(runtime: &TraderRuntime, command: SourcesCommands) -> Result<()> {
    match command {
        SourcesCommands::List { rules_file } => run_list(runtime, &rules_file).await,
        SourcesCommands::Test { rules_file, phase } => {
            run_test(runtime, &rules_file, phase).await
        }
    }
}

pub async fn run_list(runtime: &TraderRuntime, rules_path: &Path) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "trader sources list",
            json!({
                "feeds": catalog_json(&rules, None),
                "web": rules.sources.web,
            }),
        )
        .with_inputs(json!({ "rules_file": rules_path })),
    );
    Ok(())
}

pub async fn run_test(runtime: &TraderRuntime, rules_path: &Path, phase: Option<String>) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let results = if let Some(phase) = phase {
        fetch_feeds_for_phase(&rules, &phase).await
    } else {
        fetch_all_enabled(&rules).await
    };

    let ok_count = results.iter().filter(|r| r.ok).count();
    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "trader sources test",
            json!({
                "tested": results.len(),
                "ok": ok_count,
                "failed": results.len().saturating_sub(ok_count),
                "results": results,
            }),
        )
        .with_inputs(json!({ "rules_file": rules_path })),
    );
    Ok(())
}
