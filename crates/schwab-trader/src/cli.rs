use std::path::PathBuf;

use clap::{Parser, Subcommand};

use schwab_cli::output::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "schwab-trader",
    version,
    about = "Equity swing trading agent for Charles Schwab (experimental)"
)]
pub struct Cli {
    #[arg(long, env = "SCHWAB_OUTPUT", default_value = "pretty")]
    pub output: OutputFormat,

    #[arg(long, short = 'j', global = true)]
    pub json: bool,

    #[arg(long, global = true)]
    pub yes: bool,

    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Paper trading: track simulated fills, exits, and ROI (no Schwab orders)
    #[arg(long, global = true)]
    pub simulate: bool,

    #[arg(long, global = true)]
    pub trust: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

impl Cli {
    pub fn effective_output(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.output
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Validate trader rules YAML
    Rules {
        #[command(subcommand)]
        command: RulesCommands,
    },
    /// Show capital ledger and tradable budget
    Capital {
        #[arg(long)]
        rules_file: PathBuf,
    },
    /// Scan watchlist for candidates (no orders)
    Scan {
        #[arg(long)]
        rules_file: PathBuf,
    },
    /// Place trades (buy with optional post-fill OCO bracket)
    Trade {
        #[command(subcommand)]
        command: TradeCommands,
    },
    /// Trade journal and stats
    Journal {
        #[command(subcommand)]
        command: JournalCommands,
    },
    /// Run swing trading agent
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// Live TUI + embedded agent (q to quit)
    Watch {
        #[arg(long)]
        rules_file: PathBuf,
        /// Do not start agent; view state only
        #[arg(long)]
        monitor_only: bool,
    },
    /// Simulation stats and paper portfolio
    Sim {
        #[command(subcommand)]
        command: SimCommands,
    },
    /// Configured URL/API/RSS feeds for LLM context
    Sources {
        #[command(subcommand)]
        command: SourcesCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum RulesCommands {
    Validate {
        rules_file: PathBuf,
    },
    Show {
        rules_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum TradeCommands {
    /// Limit buy with capital check and optional post-fill OCO
    Buy {
        #[arg(long)]
        rules_file: PathBuf,
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        quantity: f64,
        #[arg(long)]
        price: Option<f64>,
        #[arg(long, default_value = "true")]
        bracket: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum JournalCommands {
    List {
        #[arg(long)]
        rules_file: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Stats {
        #[arg(long)]
        rules_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentCommands {
    Run {
        rules_file: PathBuf,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        background: bool,
    },
    Status {
        rules_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum SimCommands {
    /// ROI, win rate, equity curve summary
    Stats {
        #[arg(long)]
        rules_file: PathBuf,
    },
    /// Reset paper portfolio and trade history
    Reset {
        #[arg(long)]
        rules_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum SourcesCommands {
    /// List configured feeds from rules YAML
    List {
        #[arg(long)]
        rules_file: PathBuf,
    },
    /// Fetch all enabled feeds (optional --phase filter)
    Test {
        #[arg(long)]
        rules_file: PathBuf,
        #[arg(long)]
        phase: Option<String>,
    },
}
