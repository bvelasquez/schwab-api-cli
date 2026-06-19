use anyhow::Result;
use serde_json::json;

use crate::cli::AccountsCommands;
use crate::config::RuntimeConfig;
use crate::human;
use crate::output::ResponseEnvelope;

pub async fn run(runtime: &RuntimeConfig, command: AccountsCommands) -> Result<()> {
    let api = runtime.build_api()?;

    match command {
        AccountsCommands::Numbers => {
            let data = match api.accounts().account_numbers().await {
                Ok(data) => data,
                Err(e) => {
                    runtime.emit(ResponseEnvelope::err("accounts numbers", e.to_string()));
                    return Ok(());
                }
            };
            runtime.emit(
                ResponseEnvelope::ok("accounts numbers", json!(data))
                    .with_next_actions(vec!["schwab accounts list --json".into()]),
            );
        }
        AccountsCommands::List { fields } => {
            // Positions are omitted unless requested; default to portfolio-friendly output.
            let fields = fields.or_else(|| Some("positions".to_string()));
            let data = api.accounts().list(fields.as_deref()).await?;
            runtime.emit(
                ResponseEnvelope::ok("accounts list", json!(data))
                    .with_inputs(json!({ "fields": fields })),
            );
        }
        AccountsCommands::Get {
            mut account_number,
            fields,
        } => {
            if runtime.is_interactive() && account_number.is_empty() {
                account_number = human::pick_account_hash(runtime, &api).await?;
            }
            let data = api
                .accounts()
                .get(&account_number, fields.as_deref())
                .await?;
            runtime.emit(ResponseEnvelope::ok("accounts get", json!(data)));
        }
    }

    Ok(())
}
