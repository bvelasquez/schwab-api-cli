use anyhow::Result;
use serde_json::json;

use crate::cli::OrdersCommands;
use crate::config::RuntimeConfig;
use crate::human;
use crate::order_schema::{
    order_examples, order_request_schema, order_schema_meta, validate_order_shape,
};
use crate::output::ResponseEnvelope;
use crate::safety::{execute_trading_order, require_trading_approval};

pub async fn run(runtime: &RuntimeConfig, command: OrdersCommands) -> Result<()> {
    let api = runtime.build_api()?;

    match command {
        OrdersCommands::Schema => {
            runtime.emit(ResponseEnvelope::ok(
                "orders schema",
                json!({
                    "schema": order_request_schema(),
                    "meta": order_schema_meta(),
                    "examples": order_examples(),
                }),
            ));
        }
        OrdersCommands::Validate {
            order,
            account_number,
        } => {
            let order_json = human::parse_order_input(&order)?;
            validate_order_shape(&order_json)?;

            let equity = if let Some(hash) = account_number.as_deref() {
                crate::portfolio::account_equity(&api, hash)
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };

            runtime.safety.validate_order(&order_json, None, equity)?;

            runtime.emit(
                ResponseEnvelope::ok("orders validate", json!({
                    "valid": true,
                    "order": order_json,
                    "account_equity": equity,
                }))
                .with_next_actions(vec![
                    "schwab orders preview --account-number <hash> --order '<json>' --json".into(),
                    "schwab orders place --account-number <hash> --order '<json>' --trust --yes --json".into(),
                ]),
            );
        }
        OrdersCommands::List {
            mut account_number,
            from_entered_time,
            to_entered_time,
            status,
            max_results,
        } => {
            if runtime.is_interactive() && account_number.is_empty() {
                account_number = human::pick_account_hash(runtime, &api).await?;
            }
            let data = api
                .orders()
                .list_for_account(
                    &account_number,
                    from_entered_time.as_deref(),
                    to_entered_time.as_deref(),
                    status.as_deref(),
                    max_results.as_deref(),
                )
                .await?;
            runtime.emit(ResponseEnvelope::ok("orders list", json!(data)));
        }
        OrdersCommands::All {
            from_entered_time,
            to_entered_time,
            status,
            max_results,
        } => {
            let data = api
                .orders()
                .list_all(
                    from_entered_time.as_deref(),
                    to_entered_time.as_deref(),
                    status.as_deref(),
                    max_results.as_deref(),
                )
                .await?;
            runtime.emit(ResponseEnvelope::ok("orders all", json!(data)));
        }
        OrdersCommands::Get {
            account_number,
            order_id,
        } => {
            let data = api.orders().get(&account_number, &order_id).await?;
            runtime.emit(ResponseEnvelope::ok("orders get", json!(data)));
        }
        OrdersCommands::Wait {
            account_number,
            order_id,
            until,
            timeout_seconds,
            interval_seconds,
            proceed_on_partial_fill,
        } => {
            use crate::order_status::{
                wait_for_order, wait_result_json, WaitCondition, WaitOptions,
            };
            use std::time::Duration;

            let condition = WaitCondition::parse(&until)?;
            let result = wait_for_order(
                &api,
                &account_number,
                &order_id,
                WaitOptions {
                    condition,
                    timeout: Duration::from_secs(timeout_seconds),
                    interval: Duration::from_secs(interval_seconds),
                    proceed_on_partial_fill,
                    requested_quantity: None,
                },
            )
            .await?;

            let envelope = if result.met {
                ResponseEnvelope::ok("orders wait", wait_result_json(&result))
            } else {
                let mut e = ResponseEnvelope::err(
                    "orders wait",
                    result
                        .error
                        .clone()
                        .unwrap_or_else(|| "Wait condition not met".into()),
                );
                e.data = wait_result_json(&result);
                e
            };
            runtime.emit(envelope.with_inputs(json!({
                "account_number": account_number,
                "order_id": order_id,
                "until": until,
            })));
        }
        OrdersCommands::Place {
            mut account_number,
            order,
        } => {
            require_trading_approval(runtime, "orders place", "Place a live brokerage order.")?;

            let order_json = if runtime.is_interactive() && order.is_empty() {
                human::read_order_json("Order JSON (file path or inline)")?
            } else {
                human::parse_order_input(&order)?
            };

            validate_order_shape(&order_json)?;

            if runtime.dry_run {
                let equity = crate::portfolio::account_equity(&api, &account_number)
                    .await
                    .ok()
                    .flatten();
                runtime.safety.validate_order(&order_json, None, equity)?;
                runtime.emit(ResponseEnvelope::ok(
                    "orders place",
                    json!({ "dry_run": true, "order": order_json }),
                ));
                return Ok(());
            }

            if runtime.is_interactive() && account_number.is_empty() {
                account_number = human::pick_account_hash(runtime, &api).await?;
            }

            let data = execute_trading_order(runtime, &api, &account_number, &order_json).await?;
            let mut envelope = ResponseEnvelope::ok("orders place", data);
            if runtime.safety.require_preview_before_place {
                envelope = envelope.with_warnings(vec!["Order previewed before submission".into()]);
            }
            runtime.emit(envelope);
        }
        OrdersCommands::Preview {
            mut account_number,
            order,
        } => {
            let order_json = if runtime.is_interactive() && order.is_empty() {
                human::read_order_json("Order JSON to preview")?
            } else {
                human::parse_order_input(&order)?
            };

            validate_order_shape(&order_json)?;

            if runtime.is_interactive() && account_number.is_empty() {
                account_number = human::pick_account_hash(runtime, &api).await?;
            }

            let equity = crate::portfolio::account_equity(&api, &account_number)
                .await
                .ok()
                .flatten();
            runtime.safety.validate_order(&order_json, None, equity)?;

            let data = api.orders().preview(&account_number, &order_json).await?;
            runtime.emit(
                ResponseEnvelope::ok("orders preview", json!(data)).with_next_actions(vec![
                    format!(
                        "schwab orders place --account-number {account_number} --order '<json>' --trust --yes"
                    ),
                ]),
            );
        }
        OrdersCommands::Cancel {
            account_number,
            order_id,
        } => {
            require_trading_approval(runtime, "orders cancel", "Cancel open order.")?;
            if runtime.dry_run {
                runtime.emit(ResponseEnvelope::ok(
                    "orders cancel",
                    json!({ "dry_run": true, "account_number": account_number, "order_id": order_id }),
                ));
                return Ok(());
            }
            let data = api.orders().cancel(&account_number, &order_id).await?;
            runtime.emit(ResponseEnvelope::ok(
                "orders cancel",
                json!({ "status": data.status, "canceled": true }),
            ));
        }
        OrdersCommands::Replace {
            account_number,
            order_id,
            order,
        } => {
            require_trading_approval(runtime, "orders replace", "Replace an existing order.")?;
            let order_json = human::parse_order_input(&order)?;
            validate_order_shape(&order_json)?;
            if runtime.dry_run {
                let equity = crate::portfolio::account_equity(&api, &account_number)
                    .await
                    .ok()
                    .flatten();
                runtime.safety.validate_order(&order_json, None, equity)?;
                runtime.emit(ResponseEnvelope::ok(
                    "orders replace",
                    json!({ "dry_run": true, "order": order_json }),
                ));
                return Ok(());
            }
            let data = api
                .orders()
                .replace(&account_number, &order_id, &order_json)
                .await?;
            runtime.emit(ResponseEnvelope::ok(
                "orders replace",
                json!({
                    "status": data.status,
                    "location": data.location,
                }),
            ));
        }
    }

    Ok(())
}
