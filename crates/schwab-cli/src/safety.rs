use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::config::RuntimeConfig;
use crate::portfolio::{
    account_equity, ensure_preview_accepted, ensure_preview_buying_power,
    validate_buying_power_after_preview, validate_buying_power_for_order,
};
use crate::safety_config::{validate_order, SafetyConfig};

pub fn require_mutation_approval(
    runtime: &RuntimeConfig,
    command: &str,
    summary: &str,
) -> Result<()> {
    if runtime.dry_run {
        return Ok(());
    }

    if runtime.yes {
        return Ok(());
    }

    if runtime.is_interactive() {
        let confirm = inquire::Confirm::new(&format!("{summary} Proceed?"))
            .with_default(false)
            .prompt()?;
        if confirm {
            return Ok(());
        }
        bail!("Mutation cancelled by user");
    }

    bail!(
        "Mutation `{command}` blocked in non-interactive mode. Pass --yes to confirm or --dry-run to validate."
    );
}

/// Trading mutations (orders, trade buy/sell) require explicit trust mode for autonomous agents.
pub fn require_trading_approval(
    runtime: &RuntimeConfig,
    command: &str,
    summary: &str,
) -> Result<()> {
    if runtime.dry_run {
        return Ok(());
    }

    if runtime.is_interactive() {
        let confirm = inquire::Confirm::new(&format!("{summary} Proceed?"))
            .with_default(false)
            .prompt()?;
        if confirm {
            return Ok(());
        }
        bail!("Trade cancelled by user");
    }

    // Agent / non-interactive: need both --trust and --yes
    if runtime.trust && runtime.yes {
        return Ok(());
    }

    if runtime.yes && !runtime.trust {
        bail!(
            "Trading mutation `{command}` blocked: --yes requires --trust for autonomous agent execution. \
             Safety limits in safety.json still apply."
        );
    }

    bail!(
        "Trading mutation `{command}` blocked in non-interactive mode. \
         Pass --trust --yes to execute autonomously, or --dry-run to validate."
    );
}

pub async fn execute_trading_order(
    runtime: &RuntimeConfig,
    api: &schwab_api::TraderApi,
    account_number: &str,
    order: &Value,
) -> Result<Value> {
    let equity = account_equity(api, account_number).await.ok().flatten();

    // Static validation before preview (symbol, qty, types)
    runtime.safety.validate_order(order, None, equity)?;

    validate_buying_power_for_order(api, account_number, order, None).await?;

    let preview = if runtime.safety.require_preview_before_place {
        Some(api.orders().preview(account_number, order).await?)
    } else {
        None
    };

    if let Some(ref preview_data) = preview {
        ensure_preview_accepted(preview_data)?;
        ensure_preview_buying_power(preview_data)?;
        runtime
            .safety
            .validate_order(order, Some(preview_data), equity)?;
        validate_buying_power_after_preview(api, account_number, order, preview_data).await?;
    }

    let place = api.orders().place(account_number, order).await?;
    let order_id = place
        .location
        .as_deref()
        .and_then(crate::order_status::parse_order_id_from_location);

    Ok(json!({
        "status": place.status,
        "location": place.location,
        "order_id": order_id,
        "previewed": preview.is_some(),
        "message": "Order accepted; order_id present when Location header is returned",
    }))
}

#[derive(Clone, Debug)]
pub struct SafetyContext {
    inner: Arc<SafetyConfig>,
}

impl SafetyContext {
    pub fn new(config: SafetyConfig) -> Self {
        Self {
            inner: Arc::new(config),
        }
    }

    pub fn validate_order(
        &self,
        order: &Value,
        preview: Option<&Value>,
        account_equity: Option<f64>,
    ) -> Result<()> {
        validate_order(&self.inner, order, preview, account_equity)
    }
}

impl std::ops::Deref for SafetyContext {
    type Target = SafetyConfig;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeConfig;
    use crate::mode::CliMode;
    use crate::output::{OutputFormat, OutputSink};

    fn runtime(trust: bool, yes: bool) -> RuntimeConfig {
        RuntimeConfig {
            mode: CliMode::Agent,
            output: OutputFormat::Json,
            yes,
            dry_run: false,
            trust,
            safety: SafetyContext::new(SafetyConfig::default()),
            sink: OutputSink::stdout(),
        }
    }

    #[test]
    fn trading_requires_trust_and_yes() {
        assert!(require_trading_approval(&runtime(false, false), "trade buy", "x").is_err());
        assert!(require_trading_approval(&runtime(false, true), "trade buy", "x").is_err());
        assert!(require_trading_approval(&runtime(true, false), "trade buy", "x").is_err());
        assert!(require_trading_approval(&runtime(true, true), "trade buy", "x").is_ok());
    }

    #[test]
    fn dry_run_skips_trading_approval() {
        let mut rt = runtime(false, false);
        rt.dry_run = true;
        assert!(require_trading_approval(&rt, "trade buy", "x").is_ok());
    }
}
