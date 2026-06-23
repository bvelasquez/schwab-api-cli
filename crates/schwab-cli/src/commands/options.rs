use anyhow::Result;
use schwab_market_data::endpoints::chains::ChainQuery;
use serde_json::json;

use crate::cli::OptionsCommands;
use crate::config::RuntimeConfig;
use crate::human;
use crate::options::{
    build_close_order_for_group, build_order_for_strategy, ensure_option_buying_power,
    estimate_order_margin, find_position_group, group_option_legs, list_option_positions,
    options_schema, params_from_value, validate_account_for_strategy,
    StrategyKind,
};
use crate::output::ResponseEnvelope;
use crate::portfolio::account_equity;
use crate::rules::AccountType;
use crate::safety::{execute_trading_order, require_trading_approval};

pub async fn run(runtime: &RuntimeConfig, command: OptionsCommands) -> Result<()> {
    let trader = runtime.build_api()?;
    let market = runtime.build_market_api()?;

    match command {
        OptionsCommands::Chain {
            symbol,
            contract_type,
            strike_count,
            from_date,
            to_date,
        } => {
            let ct = contract_type.as_deref().unwrap_or("ALL");
            let data = market
                .chains()
                .get(&ChainQuery {
                    symbol: &symbol,
                    contract_type: Some(ct),
                    strike_count,
                    include_underlying_quote: Some(true),
                    from_date: from_date.as_deref(),
                    to_date: to_date.as_deref(),
                    ..Default::default()
                })
                .await?;
            runtime.emit(ResponseEnvelope::ok("options chain", json!(data)).with_inputs(json!({
                "symbol": symbol,
                "contract_type": ct,
            })));
        }
        OptionsCommands::Positions { account_number } => {
            let hash = account_number.as_deref();
            let legs = list_option_positions(&trader, hash).await?;
            let groups = group_option_legs(&legs);
            runtime.emit(ResponseEnvelope::ok(
                "options positions",
                json!({ "legs": legs, "groups": groups }),
            ));
        }
        OptionsCommands::Schema => {
            runtime.emit(ResponseEnvelope::ok("options schema", options_schema()));
        }
        OptionsCommands::Validate {
            strategy,
            params,
            account_number,
            account_type,
        } => {
            let kind = StrategyKind::parse(&strategy)?;
            let raw = human::parse_order_input(&params)?;
            let normalized = params_from_value(kind, &raw)?;
            let order = build_order_for_strategy(kind, &normalized)?;
            let equity = if let Some(hash) = account_number.as_deref() {
                account_equity(&trader, hash).await.ok().flatten()
            } else {
                None
            };
            runtime.safety.validate_order(&order, None, equity)?;
            let acct_type = parse_account_type(account_type.as_deref())?;
            validate_account_for_strategy(acct_type, kind)?;
            let margin = estimate_order_margin(&order, kind, &normalized)?;
            runtime.emit(ResponseEnvelope::ok(
                "options validate",
                json!({
                    "valid": true,
                    "strategy": kind.as_str(),
                    "estimated_margin_usd": margin,
                    "order": order,
                }),
            ));
        }
        OptionsCommands::Preview {
            account_number,
            strategy,
            params,
        } => {
            let kind = StrategyKind::parse(&strategy)?;
            let raw = human::parse_order_input(&params)?;
            let normalized = params_from_value(kind, &raw)?;
            let order = build_order_for_strategy(kind, &normalized)?;
            let equity = account_equity(&trader, &account_number).await.ok().flatten();
            runtime.safety.validate_order(&order, None, equity)?;
            let margin = estimate_order_margin(&order, kind, &normalized)?;
            ensure_option_buying_power(&trader, &account_number, margin).await?;
            let preview = trader.orders().preview(&account_number, &order).await?;
            runtime.emit(ResponseEnvelope::ok(
                "options preview",
                json!({ "preview": preview, "order": order, "estimated_margin_usd": margin }),
            ));
        }
        OptionsCommands::Open {
            account_number,
            strategy,
            params,
        } => {
            let kind = StrategyKind::parse(&strategy)?;
            let raw = human::parse_order_input(&params)?;
            let normalized = params_from_value(kind, &raw)?;
            let order = build_order_for_strategy(kind, &normalized)?;
            let margin = estimate_order_margin(&order, kind, &normalized)?;
            require_trading_approval(
                runtime,
                "options open",
                &format!("Open {} {} on {}", kind.as_str(), margin, account_number),
            )?;
            ensure_option_buying_power(&trader, &account_number, margin).await?;
            let data = if runtime.dry_run {
                json!({
                    "dry_run": true,
                    "order": order,
                    "estimated_margin_usd": margin,
                })
            } else {
                execute_trading_order(runtime, &trader, &account_number, &order).await?
            };
            runtime.emit(ResponseEnvelope::ok("options open", data));
        }
        OptionsCommands::Close {
            account_number,
            position_id,
        } => {
            let legs = list_option_positions(&trader, Some(&account_number)).await?;
            let groups = group_option_legs(&legs);
            let group = find_position_group(&groups, &position_id)
                .ok_or_else(|| anyhow::anyhow!("position not found: {position_id}"))?;
            let order = build_close_order_for_group(group)?;
            require_trading_approval(
                runtime,
                "options close",
                &format!("Close position {position_id}"),
            )?;
            let data = if runtime.dry_run {
                json!({ "dry_run": true, "order": order, "position": group })
            } else {
                execute_trading_order(runtime, &trader, &account_number, &order).await?
            };
            runtime.emit(ResponseEnvelope::ok("options close", data));
        }
    }
    Ok(())
}

fn parse_account_type(raw: Option<&str>) -> Result<AccountType> {
    match raw.unwrap_or("margin").to_ascii_lowercase().as_str() {
        "margin" => Ok(AccountType::Margin),
        "ira" => Ok(AccountType::Ira),
        "cash" => Ok(AccountType::Cash),
        other => anyhow::bail!("unknown account type `{other}`"),
    }
}
