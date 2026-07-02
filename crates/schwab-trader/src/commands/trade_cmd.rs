use anyhow::Result;

use crate::agent::state::load_state;
use crate::cli::TradeCommands;
use crate::closure::flatten_all_live_positions;
use crate::config::TraderRuntime;
use crate::entry::attempt_entry;
use crate::rules::TraderRules;
use schwab_cli::output::ResponseEnvelope;
use serde_json::json;
use std::sync::Arc;

pub async fn run(runtime: &TraderRuntime, command: TradeCommands) -> Result<()> {
    match command {
        TradeCommands::Buy {
            rules_file,
            symbol,
            quantity,
            price,
            bracket,
        } => run_buy(runtime, &rules_file, &symbol, quantity, price, bracket).await,
        TradeCommands::CloseAll { rules_file } => {
            run_close_all(runtime, &rules_file).await
        }
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
    let market = crate::market_ctx::MarketCtx::for_rules(market, rules_path, &rules);
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
        None,
    )
    .await?;

    runtime.emit(ResponseEnvelope::ok("trader trade buy", json!(result)));
    Ok(())
}

async fn run_close_all(runtime: &TraderRuntime, rules_path: &std::path::Path) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let account = rules.primary_account()?.hash.clone();
    let api = Arc::new(runtime.build_api()?);
    let market_api = runtime.build_market_api()?;
    let market = crate::market_ctx::MarketCtx::for_rules(market_api, rules_path, &rules);
    let mut state = load_state(rules_path, &rules.trader_id)?;

    let result = flatten_all_live_positions(
        runtime,
        rules_path,
        &rules,
        &mut state,
        &api,
        &market,
        &account,
    )
    .await?;

    runtime.emit(ResponseEnvelope::ok("trader trade close-all", result));
    Ok(())
}
