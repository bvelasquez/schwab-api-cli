use anyhow::Result;

use crate::capabilities;
use crate::cli::EnvCommands;
use crate::config::RuntimeConfig;
use crate::env_schema;
use crate::instructions;
use crate::output::ResponseEnvelope;

pub async fn capabilities(runtime: &RuntimeConfig) -> Result<()> {
    let envelope = ResponseEnvelope::ok("capabilities", capabilities::capabilities_json())
        .with_inputs(serde_json::json!({ "source": "schwab-cli" }));
    runtime.emit(envelope);
    Ok(())
}

pub async fn env(runtime: &RuntimeConfig, command: EnvCommands) -> Result<()> {
    match command {
        EnvCommands::Schema => {
            let envelope = ResponseEnvelope::ok("env schema", env_schema::env_schema_json());
            runtime.emit(envelope);
        }
    }
    Ok(())
}

pub async fn instructions(runtime: &RuntimeConfig) -> Result<()> {
    let envelope = ResponseEnvelope::ok(
        "instructions",
        instructions::instructions_json(&runtime.safety),
    );
    runtime.emit(envelope);
    Ok(())
}
