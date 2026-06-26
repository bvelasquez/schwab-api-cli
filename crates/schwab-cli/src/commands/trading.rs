use anyhow::Result;
use serde_json::json;

use crate::cli::{PortfolioCommands, SafetyCommands, TradeCommands};
use crate::config::RuntimeConfig;
use crate::human;
use crate::order_builder::{
    build_equity_order, parse_duration, parse_session, parse_trade_order_type, TradeSide,
};
use crate::output::ResponseEnvelope;
use crate::portfolio::{
    account_buying_power, account_equity, ensure_sufficient_buying_power, estimate_equity_buy_cost,
    summarize_accounts, summary_to_json,
};
use crate::safety::{execute_trading_order, require_trading_approval};
use crate::safety_config::{config_path, SafetyConfig};

pub async fn run_portfolio(runtime: &RuntimeConfig, command: PortfolioCommands) -> Result<()> {
    let api = runtime.build_api()?;

    match command {
        PortfolioCommands::Summary => {
            let accounts = api.accounts().list(Some("positions")).await?;
            let summary = summarize_accounts(&accounts);
            runtime.emit(
                ResponseEnvelope::ok("portfolio summary", summary_to_json(&summary))
                    .with_next_actions(vec![
                        "schwab portfolio buying-power --account-number <hash> --json".into(),
                        "schwab trade buy --help".into(),
                    ]),
            );
        }
        PortfolioCommands::BuyingPower { account_number } => {
            let mut account_number = account_number;
            if runtime.is_interactive() && account_number.is_empty() {
                account_number = human::pick_account_hash(runtime, &api).await?;
            }
            let buying_power = account_buying_power(&api, &account_number).await?;
            runtime.emit(
                ResponseEnvelope::ok(
                    "portfolio buying-power",
                    json!({
                        "account_number": account_number,
                        "buying_power": buying_power,
                    }),
                )
                .with_next_actions(vec![
                    "schwab market quotes --symbols <SYMBOL> --fields quote --json".into(),
                    "schwab trade buy --account-number <hash> --symbol <SYMBOL> --quantity <N> --order-type limit --price <ASK> --dry-run --json".into(),
                ]),
            );
        }
    }

    Ok(())
}

pub async fn run_trade(runtime: &RuntimeConfig, command: TradeCommands) -> Result<()> {
    let api = runtime.build_api()?;

    match command {
        TradeCommands::Buy {
            mut account_number,
            symbol,
            quantity,
            order_type,
            price,
            duration,
            session,
        } => {
            run_side(
                runtime,
                &api,
                TradeRequest {
                    side: TradeSide::Buy,
                    account_number: &mut account_number,
                    symbol: &symbol,
                    quantity,
                    order_type_raw: &order_type,
                    limit_price: price,
                    duration_raw: duration.as_deref(),
                    session_raw: session.as_deref(),
                },
            )
            .await
        }
        TradeCommands::Sell {
            mut account_number,
            symbol,
            quantity,
            order_type,
            price,
            duration,
            session,
        } => {
            run_side(
                runtime,
                &api,
                TradeRequest {
                    side: TradeSide::Sell,
                    account_number: &mut account_number,
                    symbol: &symbol,
                    quantity,
                    order_type_raw: &order_type,
                    limit_price: price,
                    duration_raw: duration.as_deref(),
                    session_raw: session.as_deref(),
                },
            )
            .await
        }
    }
}

struct TradeRequest<'a> {
    side: TradeSide,
    account_number: &'a mut String,
    symbol: &'a str,
    quantity: f64,
    order_type_raw: &'a str,
    limit_price: Option<f64>,
    duration_raw: Option<&'a str>,
    session_raw: Option<&'a str>,
}

async fn run_side(
    runtime: &RuntimeConfig,
    api: &schwab_api::TraderApi,
    req: TradeRequest<'_>,
) -> Result<()> {
    let TradeRequest {
        side,
        account_number,
        symbol,
        quantity,
        order_type_raw,
        limit_price,
        duration_raw,
        session_raw,
    } = req;
    let side_label = if side == TradeSide::Buy {
        "buy"
    } else {
        "sell"
    };
    require_trading_approval(
        runtime,
        &format!("trade {side_label}"),
        &format!("Place a live {side_label} order for {quantity} shares of {symbol}."),
    )?;

    if runtime.is_interactive() && account_number.is_empty() {
        *account_number = human::pick_account_hash(runtime, api).await?;
    }

    let order_type = parse_trade_order_type(order_type_raw)?;
    let duration = parse_duration(duration_raw)?;
    let session = parse_session(session_raw)?;
    let order = build_equity_order(
        side,
        symbol,
        quantity,
        order_type,
        limit_price,
        duration,
        session,
    )?;

    let mut buying_power_check = None;
    if side == TradeSide::Buy {
        let buying_power = account_buying_power(api, account_number).await?;
        let market_ask = if order_type == crate::order_builder::TradeOrderType::Market {
            Some(fetch_equity_ask(runtime, symbol).await?)
        } else {
            None
        };
        let estimated_cost =
            estimate_equity_buy_cost(quantity, order_type_raw, limit_price, market_ask)?;
        ensure_sufficient_buying_power(&buying_power, estimated_cost)?;
        buying_power_check = Some(json!({
            "cash_available_for_trading": buying_power.cash_available_for_trading,
            "estimated_cost": estimated_cost,
            "remaining_after_trade": buying_power.cash_available_for_trading - estimated_cost,
        }));
    }

    let inputs = json!({
        "account_number": account_number,
        "symbol": symbol.to_uppercase(),
        "quantity": quantity,
        "order_type": order_type_raw,
        "price": limit_price,
        "side": side_label,
        "buying_power_check": buying_power_check,
    });

    if runtime.dry_run {
        let equity = account_equity(api, account_number).await.ok().flatten();
        runtime.safety.validate_order(&order, None, equity)?;
        runtime.emit(
            ResponseEnvelope::ok(
                format!("trade {side_label}"),
                json!({ "dry_run": true, "order": order, "buying_power_check": buying_power_check }),
            )
            .with_inputs(inputs),
        );
        return Ok(());
    }

    let result = execute_trading_order(runtime, api, account_number, &order).await?;
    runtime.emit(ResponseEnvelope::ok(format!("trade {side_label}"), result).with_inputs(inputs));
    Ok(())
}

async fn fetch_equity_ask(runtime: &RuntimeConfig, symbol: &str) -> Result<f64> {
    use anyhow::Context;

    let market_api = runtime.build_market_api()?;
    let symbol = symbol.trim().to_uppercase();
    let quotes = market_api
        .quotes()
        .get_quotes(&symbol, Some("quote"), None)
        .await
        .context("Failed to fetch market quote for buying-power estimate")?;
    let quote = quotes
        .get(&symbol)
        .or_else(|| quotes.as_object().and_then(|m| m.values().next()))
        .and_then(|entry| entry.get("quote"))
        .with_context(|| format!("Quote payload missing for symbol {symbol}"))?;
    quote
        .get("askPrice")
        .and_then(|v| v.as_f64())
        .context("Quote missing askPrice for buying-power estimate")
}

pub async fn run_safety(runtime: &RuntimeConfig, command: SafetyCommands) -> Result<()> {
    match command {
        SafetyCommands::Show => {
            let path = config_path();
            let loaded_from_file = path.is_file();
            runtime.emit(ResponseEnvelope::ok(
                "safety show",
                json!({
                    "path": path,
                    "loaded_from_file": loaded_from_file,
                    "config": &*runtime.safety,
                    "trust_mode": runtime.trust,
                }),
            ));
        }
        SafetyCommands::Init => {
            crate::safety::require_mutation_approval(
                runtime,
                "safety init",
                "Write default safety.json to the config directory.",
            )?;
            let path = SafetyConfig::init_at_default_path()?;
            let cfg = SafetyConfig::load()?;
            runtime.emit(
                ResponseEnvelope::ok(
                    "safety init",
                    json!({
                        "path": path,
                        "created": true,
                        "config": cfg,
                    }),
                )
                .with_next_actions(vec![
                    "Edit safety.json limits before enabling --trust".into(),
                ]),
            );
        }
        SafetyCommands::Path => {
            runtime.emit(ResponseEnvelope::ok(
                "safety path",
                json!({ "path": config_path() }),
            ));
        }
    }
    Ok(())
}
