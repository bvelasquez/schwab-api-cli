use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::time::sleep;

use schwab_cli::order_status::order_status;
use schwab_cli::safety::{execute_trading_order, require_trading_approval};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcoStatus {
    Working,
    FilledExit,
    Canceled,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct BracketPlaceResult {
    pub order: Value,
    pub attempts: u32,
    pub elapsed_ms: u64,
}

pub fn build_exit_oco_order(
    symbol: &str,
    quantity: f64,
    profit_limit: f64,
    stop_price: f64,
    stop_limit: f64,
    duration: &str,
) -> Value {
    json!({
        "orderStrategyType": "OCO",
        "childOrderStrategies": [
            {
                "orderType": "LIMIT",
                "session": "NORMAL",
                "price": format!("{profit_limit:.2}"),
                "duration": duration,
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "SELL",
                    "quantity": quantity,
                    "instrument": {
                        "symbol": symbol.trim().to_uppercase(),
                        "assetType": "EQUITY"
                    }
                }]
            },
            {
                "orderType": "STOP_LIMIT",
                "session": "NORMAL",
                "stopPrice": format!("{stop_price:.2}"),
                "price": format!("{stop_limit:.2}"),
                "duration": duration,
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "SELL",
                    "quantity": quantity,
                    "instrument": {
                        "symbol": symbol.trim().to_uppercase(),
                        "assetType": "EQUITY"
                    }
                }]
            }
        ]
    })
}

pub async fn place_oco_bracket(
    runtime: &crate::config::TraderRuntime,
    api: &schwab_api::TraderApi,
    account_hash: &str,
    symbol: &str,
    quantity: f64,
    profit_limit: f64,
    stop_price: f64,
    stop_limit: f64,
    duration: &str,
) -> Result<Value> {
    let order = build_exit_oco_order(
        symbol, quantity, profit_limit, stop_price, stop_limit, duration,
    );
    let schwab_rt = runtime.as_schwab_runtime();
    require_trading_approval(
        &schwab_rt,
        "trade bracket",
        &format!("Place OCO exit for {quantity} {symbol}"),
    )?;
    runtime.safety.validate_order(&order, None, None)?;
    let result = execute_trading_order(&schwab_rt, api, account_hash, &order).await?;
    Ok(result)
}

pub async fn place_oco_bracket_with_retry(
    runtime: &crate::config::TraderRuntime,
    api: &schwab_api::TraderApi,
    account_hash: &str,
    symbol: &str,
    quantity: f64,
    profit_limit: f64,
    stop_price: f64,
    stop_limit: f64,
    duration: &str,
    max_seconds: u64,
    max_attempts: u32,
) -> Result<BracketPlaceResult> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(max_seconds.max(1));
    let mut attempts = 0u32;
    let mut last_err = None;

    while attempts < max_attempts.max(1) && Instant::now() < deadline {
        attempts += 1;
        match place_oco_bracket(
            runtime,
            api,
            account_hash,
            symbol,
            quantity,
            profit_limit,
            stop_price,
            stop_limit,
            duration,
        )
        .await
        {
            Ok(order) => {
                return Ok(BracketPlaceResult {
                    order,
                    attempts,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
            Err(err) => {
                last_err = Some(err);
                if Instant::now() < deadline && attempts < max_attempts {
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("OCO bracket placement failed")))
}

pub async fn cancel_order(
    runtime: &crate::config::TraderRuntime,
    api: &schwab_api::TraderApi,
    account_hash: &str,
    order_id: &str,
    label: &str,
) -> Result<Value> {
    let schwab_rt = runtime.as_schwab_runtime();
    require_trading_approval(
        &schwab_rt,
        "cancel order",
        &format!("Cancel order {order_id} ({label})"),
    )?;
    let result = api.orders().cancel(account_hash, order_id).await?;
    Ok(serde_json::to_value(result)?)
}

pub async fn poll_oco_status(
    api: &schwab_api::TraderApi,
    account_hash: &str,
    order_id: &str,
) -> Result<(OcoStatus, Value)> {
    let order = api.orders().get(account_hash, order_id).await?;
    let status = order_status(&order).unwrap_or_default();

    let oco_status = match status.as_str() {
        "FILLED" => OcoStatus::FilledExit,
        "CANCELED" | "CANCELLED" | "REJECTED" | "EXPIRED" => OcoStatus::Canceled,
        "WORKING" | "QUEUED" | "ACCEPTED" | "AWAITING_PARENT_ORDER" | "PENDING_ACTIVATION" => {
            OcoStatus::Working
        }
        _ => {
            // Check child legs for partial fill
            if order
                .get("childOrderStrategies")
                .and_then(|v| v.as_array())
                .is_some_and(|children| {
                    children.iter().any(|c| {
                        order_status(c)
                            .is_some_and(|s| s == "FILLED")
                    })
                })
            {
                OcoStatus::FilledExit
            } else {
                OcoStatus::Unknown
            }
        }
    };

    Ok((oco_status, order))
}

pub async fn replace_oco_bracket(
    runtime: &crate::config::TraderRuntime,
    api: &Arc<schwab_api::TraderApi>,
    account_hash: &str,
    old_order_id: &str,
    symbol: &str,
    quantity: f64,
    profit_limit: f64,
    stop_price: f64,
    stop_limit: f64,
    duration: &str,
) -> Result<BracketPlaceResult> {
    let _ = cancel_order(runtime, api, account_hash, old_order_id, "trailing stop replace")
        .await;
    place_oco_bracket_with_retry(
        runtime,
        api,
        account_hash,
        symbol,
        quantity,
        profit_limit,
        stop_price,
        stop_limit,
        duration,
        30,
        2,
    )
    .await
}
