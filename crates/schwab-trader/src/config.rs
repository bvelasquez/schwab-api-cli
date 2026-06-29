use anyhow::{Context, Result};
use schwab_api::{ClientConfig, SchwabClient, TraderApi};
use schwab_cli::config::RuntimeConfig;
use schwab_cli::mode::CliMode;
use schwab_cli::output::{OutputFormat, OutputSink, ResponseEnvelope};
use schwab_cli::safety::SafetyContext;
use schwab_cli::safety_config::SafetyConfig;
use schwab_market_data::MarketDataApi;
use std::sync::Arc;

use crate::cli::Cli;

#[derive(Debug, Clone)]
pub struct TraderRuntime {
    pub output: OutputFormat,
    pub yes: bool,
    pub dry_run: bool,
    pub simulate: bool,
    pub trust: bool,
    /// When true, agent ticks do not print JSON to stdout (watch TUI mode).
    pub suppress_tick_output: bool,
    pub safety: SafetyContext,
    pub sink: OutputSink,
}

impl TraderRuntime {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        let safety_cfg = SafetyConfig::load().context("Failed to load safety.json")?;
        anyhow::ensure!(
            !(cli.dry_run && cli.simulate),
            "--dry-run and --simulate are mutually exclusive (use --simulate for paper trading)"
        );
        Ok(Self {
            output: cli.effective_output(),
            yes: cli.yes,
            dry_run: cli.dry_run,
            simulate: cli.simulate,
            trust: cli.trust,
            suppress_tick_output: false,
            safety: SafetyContext::new(safety_cfg),
            sink: OutputSink::stdout(),
        })
    }

    pub fn emit(&self, envelope: ResponseEnvelope) {
        if self.suppress_tick_output {
            return;
        }
        self.sink.write(&envelope, self.output);
    }

    pub fn is_tty(&self) -> bool {
        use std::io::{stdin, stdout, IsTerminal};
        stdin().is_terminal() && stdout().is_terminal()
    }

    pub fn as_schwab_runtime(&self) -> RuntimeConfig {
        RuntimeConfig {
            mode: CliMode::Agent,
            output: self.output,
            yes: self.yes,
            dry_run: self.dry_run,
            trust: self.trust,
            suppress_tick_output: self.suppress_tick_output,
            safety: self.safety.clone(),
            sink: self.sink.clone(),
        }
    }

    pub fn build_api(&self) -> Result<Arc<TraderApi>> {
        let config = ClientConfig::from_env().context("Failed to load Schwab client config")?;
        let client = SchwabClient::new(config);
        Ok(Arc::new(TraderApi::new(client)))
    }

    pub fn build_market_api(&self) -> Result<Arc<MarketDataApi>> {
        let config = ClientConfig::from_env().context("Failed to load Schwab client config")?;
        let client = SchwabClient::new(config);
        Ok(Arc::new(MarketDataApi::new(client)))
    }
}
