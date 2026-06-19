use anyhow::Result;
use serde_json::json;

use crate::cli::TransactionsCommands;
use crate::config::RuntimeConfig;
use crate::human;
use crate::output::ResponseEnvelope;

pub async fn run(runtime: &RuntimeConfig, command: TransactionsCommands) -> Result<()> {
    let api = runtime.build_api()?;

    match command {
        TransactionsCommands::List {
            mut account_number,
            start_date,
            end_date,
            types,
            symbol,
        } => {
            if runtime.is_interactive() && account_number.is_empty() {
                account_number = human::pick_account_hash(runtime, &api).await?;
            }
            let data = api
                .transactions()
                .list(
                    &account_number,
                    start_date.as_deref(),
                    end_date.as_deref(),
                    types.as_deref(),
                    symbol.as_deref(),
                )
                .await?;
            runtime.emit(ResponseEnvelope::ok("transactions list", json!(data)));
        }
        TransactionsCommands::Get {
            account_number,
            transaction_id,
        } => {
            let data = api
                .transactions()
                .get(&account_number, &transaction_id)
                .await?;
            runtime.emit(ResponseEnvelope::ok("transactions get", json!(data)));
        }
    }

    Ok(())
}
