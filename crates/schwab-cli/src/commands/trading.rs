use anyhow::Result;
use serde_json::json;

use crate::cli::{PortfolioCommands, SafetyCommands, TradeCommands};
use crate::config::RuntimeConfig;
use crate::human;
use crate::order_builder::{
    build_equity_order, parse_duration, parse_session, parse_trade_order_type,
    TradeSide,
};
use crate::output::ResponseEnvelope;
use crate::portfolio::{account_equity, summarize_accounts, summary_to_json};
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
                    .with_next_actions(vec!["schwab trade buy --help".into()]),
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

async fn run_side(runtime: &RuntimeConfig, api: &schwab_api::TraderApi, req: TradeRequest<'_>) -> Result<()> {
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
    let side_label = if side == TradeSide::Buy { "buy" } else { "sell" };
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

    let inputs = json!({
        "account_number": account_number,
        "symbol": symbol.to_uppercase(),
        "quantity": quantity,
        "order_type": order_type_raw,
        "price": limit_price,
        "side": side_label,
    });

    if runtime.dry_run {
        let equity = account_equity(api, account_number).await.ok().flatten();
        runtime.safety.validate_order(&order, None, equity)?;
        runtime.emit(
            ResponseEnvelope::ok(
                format!("trade {side_label}"),
                json!({ "dry_run": true, "order": order }),
            )
            .with_inputs(inputs),
        );
        return Ok(());
    }

    let result = execute_trading_order(runtime, api, account_number, &order).await?;
    runtime.emit(
        ResponseEnvelope::ok(format!("trade {side_label}"), result).with_inputs(inputs),
    );
    Ok(())
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
                .with_next_actions(vec!["Edit safety.json limits before enabling --trust".into()]),
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
