use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use crate::agent::state::{load_state, save_state};
use crate::cli::SimCommands;
use crate::config::TraderRuntime;
use crate::journal;
use crate::rules::TraderRules;
use crate::sim::{compute_stats, reset_ledger, snapshot_equity};

pub async fn run(runtime: &TraderRuntime, command: SimCommands) -> Result<()> {
    match command {
        SimCommands::Stats { rules_file } => run_stats(runtime, &rules_file).await,
        SimCommands::Report { rules_file, output } => run_report(runtime, &rules_file, output.as_deref()).await,
        SimCommands::Reset { rules_file } => run_reset(runtime, &rules_file).await,
    }
}

async fn run_stats(runtime: &TraderRuntime, rules_path: &Path) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let mut state = load_state(rules_path, &rules.trader_id)?;
    snapshot_equity(&mut state, &rules);
    save_state(rules_path, &state)?;

    let stats = match compute_stats(&state) {
        Some(s) => serde_json::to_value(s)?,
        None => json!({
            "message": "No simulation ledger yet. Run with --simulate to start paper trading.",
        }),
    };

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok("trader sim stats", stats)
            .with_inputs(json!({ "rules_file": rules_path, "simulate": runtime.simulate })),
    );
    Ok(())
}

async fn run_report(
    runtime: &TraderRuntime,
    rules_path: &Path,
    output: Option<&Path>,
) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let report = journal::build_sim_analysis_report(rules_path, &rules)?;

    if let Some(path) = output {
        let pretty = serde_json::to_string_pretty(&report)?;
        fs::write(path, pretty).with_context(|| format!("write report {}", path.display()))?;
        runtime.emit(
            schwab_cli::output::ResponseEnvelope::ok(
                "trader sim report",
                json!({
                    "written": path,
                    "events_total": report.get("event_counts"),
                    "ledger_stats": report.get("ledger_stats"),
                }),
            )
            .with_inputs(json!({ "rules_file": rules_path, "output": path })),
        );
    } else {
        runtime.emit(
            schwab_cli::output::ResponseEnvelope::ok("trader sim report", report)
                .with_inputs(json!({ "rules_file": rules_path })),
        );
    }
    Ok(())
}

async fn run_reset(runtime: &TraderRuntime, rules_path: &Path) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let mut state = load_state(rules_path, &rules.trader_id)?;
    reset_ledger(&mut state, &rules);
    save_state(rules_path, &state).context("save state after sim reset")?;
    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "trader sim reset",
            json!({
                "reset": true,
                "starting_cash_usd": state.sim.as_ref().map(|s| s.starting_cash_usd),
            }),
        )
        .with_inputs(json!({ "rules_file": rules_path })),
    );
    Ok(())
}
