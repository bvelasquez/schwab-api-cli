use anyhow::Result;

use crate::agent::state::load_state;
use crate::cli::TradeCommands;
use crate::config::TraderRuntime;
use crate::entry::attempt_entry;
use crate::rules::TraderRules;
use schwab_cli::output::ResponseEnvelope;
use serde_json::json;

pub async fn run(runtime: &TraderRuntime, command: TradeCommands) -> Result<()> {
    match command {
        TradeCommands::Buy {
            rules_file,
            symbol,
            quantity,
            price,
            bracket,
        } => run_buy(runtime, &rules_file, &symbol, quantity, price, bracket).await,
    }
}

async fn run_buy(
    runtime: &TraderRuntime,
    rules_path: &std::path::Path,
    symbol: &str,
    quantity: f64,
    price: Option<f64>,
    bracket: bool,
) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let account = rules.primary_account()?.hash.clone();
    let api = runtime.build_api()?;
    let market = runtime.build_market_api()?;
    let mut state = load_state(rules_path, &rules.trader_id)?;

    let result = attempt_entry(
        runtime,
        rules_path,
        &rules,
        &mut state,
        &api,
        &market,
        &account,
        symbol,
        price,
        Some(quantity),
        bracket,
        "manual",
    )
    .await?;

    runtime.emit(ResponseEnvelope::ok("trader trade buy", json!(result)));
    Ok(())
}
