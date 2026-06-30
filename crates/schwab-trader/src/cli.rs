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
    /// Historical swing backtest (Schwab daily bars)
    Backtest {
        #[command(subcommand)]
        command: BacktestCommands,
    },
    /// Configured URL/API/RSS feeds for LLM context
    Sources {
        #[command(subcommand)]
        command: SourcesCommands,
    },
    /// Build and maintain rules watchlists from a candidate pool
    Watchlist {
        #[command(subcommand)]
        command: WatchlistCommands,
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
    /// Full analysis report from journal + ledger (for post-simulation review)
    Report {
        #[arg(long)]
        rules_file: PathBuf,
        /// Write report JSON to this path (stdout via --json if omitted)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Reset paper portfolio and trade history
    Reset {
        #[arg(long)]
        rules_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum BacktestCommands {
    /// Download daily OHLCV from Schwab into a local cache
    Prefetch {
        #[arg(long)]
        rules_file: PathBuf,
        /// Start date YYYY-MM-DD (default: ~2 years before --to)
        #[arg(long)]
        from: Option<String>,
        /// End date YYYY-MM-DD (default: yesterday)
        #[arg(long)]
        to: Option<String>,
        /// Re-download even if cache covers the range
        #[arg(long)]
        force: bool,
    },
    /// Replay trading days through scan/entry/exit logic
    Run {
        #[arg(long)]
        rules_file: PathBuf,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        /// Reset backtest state and journal before running
        #[arg(long)]
        fresh: bool,
        /// Run LLM learn loop (in-memory rule patches; requires OPENROUTER_API_KEY)
        #[arg(long)]
        learn: bool,
        /// Disable LLM learn even when llm.enabled (default: learn on when llm + adaptation enabled)
        #[arg(long)]
        no_learn: bool,
        /// Entry fill: close (same day) or next_open
        #[arg(long, default_value = "close")]
        fill_at: String,
    },
    /// Full analysis report from backtest journal + ledger
    Report {
        #[arg(long)]
        rules_file: PathBuf,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Generate cron job + wrapper script for nightly cache prefetch
    Cron {
        #[arg(long)]
        rules_file: PathBuf,
        /// Crontab schedule (default: 06:15 Mon–Fri server local time)
        #[arg(long)]
        schedule: Option<String>,
        /// Write wrapper script only (default: true with --install)
        #[arg(long)]
        write_script: bool,
        /// Append line to user crontab (also writes script)
        #[arg(long)]
        install: bool,
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

#[derive(Debug, Subcommand)]
pub enum WatchlistCommands {
    /// Show core, thematic, pool, and dynamic watchlist state
    Show {
        #[arg(long)]
        rules_file: PathBuf,
    },
    /// Screen candidate pool with playbook filters; optionally write results
    Build {
        #[arg(long)]
        rules_file: PathBuf,
        /// Write qualified symbols into rules YAML
        #[arg(long)]
        write: bool,
        /// thematic (default): replace thematic; core: append to core; both
        #[arg(long, default_value = "thematic")]
        target: String,
        #[arg(long)]
        top_n: Option<u32>,
        #[arg(long)]
        min_score: Option<f64>,
    },
    /// Candidate pool file utilities
    Pool {
        #[command(subcommand)]
        command: WatchlistPoolCommands,
    },
    /// Generate weekly refresh cron + wrapper script
    Cron {
        #[arg(long)]
        rules_file: PathBuf,
        #[arg(long)]
        schedule: Option<String>,
        #[arg(long)]
        install: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum WatchlistPoolCommands {
    /// Quote-check every symbol in a pool file
    Validate {
        pool: PathBuf,
    },
}
