use anyhow::Result;
use serde_json::json;

use crate::cli::UserCommands;
use crate::config::RuntimeConfig;
use crate::output::ResponseEnvelope;

pub async fn run(runtime: &RuntimeConfig, command: UserCommands) -> Result<()> {
    let api = runtime.build_api()?;

    match command {
        UserCommands::Preference => {
            let data = api.user().preference().await?;
            runtime.emit(ResponseEnvelope::ok("user preference", json!(data)));
        }
    }

    Ok(())
}
