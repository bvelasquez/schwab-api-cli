use std::sync::Arc;

use anyhow::{Context, Result};
use schwab_api::{ClientConfig, SchwabClient, TraderApi};
use schwab_market_data::MarketDataApi;

use crate::cli::Cli;
use crate::mode::CliMode;
use crate::output::{OutputFormat, OutputSink};
use crate::safety::SafetyContext;
use crate::safety_config::SafetyConfig;

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub mode: CliMode,
    pub output: OutputFormat,
    pub yes: bool,
    pub dry_run: bool,
    /// Paper options trading — separate state file, no broker orders.
    pub simulate: bool,
    /// Explicit trusted agent mode — required with --yes for autonomous trading.
    pub trust: bool,
    /// When true, agent ticks do not print to stdout (watch TUI mode).
    pub suppress_tick_output: bool,
    pub safety: SafetyContext,
    pub sink: OutputSink,
}

impl RuntimeConfig {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        let safety_cfg = SafetyConfig::load().context("Failed to load safety.json")?;
        anyhow::ensure!(
            !(cli.dry_run && cli.simulate),
            "--dry-run and --simulate are mutually exclusive"
        );
        Ok(Self {
            mode: cli.mode,
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

    pub fn emit(&self, envelope: crate::output::ResponseEnvelope) {
        self.sink.write(&envelope, self.output);
    }

    pub fn is_tty(&self) -> bool {
        use std::io::{stdin, stdout, IsTerminal};
        stdin().is_terminal() && stdout().is_terminal()
    }

    /// Human-mode guided prompts (account pickers, etc.).
    pub fn is_interactive(&self) -> bool {
        use std::io::stdout;
        use std::io::IsTerminal;
        self.mode.is_human() && stdout().is_terminal()
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

    /// Agent-mode runtime for order execution (used by `schwab-trader`).
    pub fn for_agent_trading(
        output: OutputFormat,
        yes: bool,
        dry_run: bool,
        trust: bool,
        suppress_tick_output: bool,
        safety: SafetyContext,
        sink: OutputSink,
    ) -> Self {
        Self {
            mode: CliMode::Agent,
            output,
            yes,
            dry_run,
            simulate: false,
            trust,
            suppress_tick_output,
            safety,
            sink,
        }
    }
}
