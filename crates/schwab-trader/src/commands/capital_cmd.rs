use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::agent::state::load_state;
use crate::capital::{capital_check_to_json, compute_capital_check};
use crate::config::TraderRuntime;
use crate::rules::TraderRules;

pub async fn run_show(runtime: &TraderRuntime, rules_path: &Path) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let account = rules.primary_account()?.hash.clone();
    let api = runtime.build_api()?;
    let state = load_state(rules_path, &rules.trader_id)?;
    let check = compute_capital_check(
        &api,
        &rules,
        &state,
        &account,
        None,
        None,
        runtime.simulate,
        Some(rules_path),
    )
    .await?;
    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "trader capital show",
            json!({
                "capital_check": capital_check_to_json(&check),
                "state_summary": state.summary(),
            }),
        )
        .with_inputs(json!({ "rules_file": rules_path })),
    );
    Ok(())
}
