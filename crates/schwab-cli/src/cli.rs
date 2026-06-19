use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::mode::CliMode;
use crate::output::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "schwab",
    version,
    about = "Agent-first CLI for Charles Schwab Trader API",
    long_about = "Schwab Trader API CLI (Accounts and Trading Production).\n\n\
        AGENTS: Prefer --mode agent (default). Discover commands with:\n\
          schwab --help --json\n\
          schwab capabilities --json\n\
          schwab env schema --json\n\
          schwab instructions --json\n\n\
        HUMANS: Use --mode human for guided prompts when arguments are omitted."
)]
pub struct Cli {
    /// Operating mode: agent (structured, default) or human (interactive prompts)
    #[arg(long, env = "SCHWAB_MODE", default_value = "agent")]
    pub mode: CliMode,

    /// Output format
    #[arg(long, env = "SCHWAB_OUTPUT", default_value = "pretty")]
    pub output: OutputFormat,

    /// Shorthand for --output json
    #[arg(long, short = 'j', global = true)]
    pub json: bool,

    /// Shorthand for --output md
    #[arg(long, global = true)]
    pub md: bool,

    /// Auto-confirm mutations (required in non-interactive agent mode)
    #[arg(long, global = true)]
    pub yes: bool,

    /// Validate mutation without executing
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Trusted agent mode: allow autonomous trading with --trust --yes (safety.json limits still enforced)
    #[arg(long, global = true)]
    pub trust: bool,

    /// Emit full command tree as JSON (agent discovery)
    #[arg(long, global = true)]
    pub help_json: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Machine-readable command catalog for agents
    Capabilities,

    /// Environment variable schema and precedence
    Env {
        #[command(subcommand)]
        command: EnvCommands,
    },

    /// Agent system prompt / tool-use instructions
    Instructions,

    /// OAuth authentication and token management
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// Account numbers, balances, and positions
    Accounts {
        #[command(subcommand)]
        command: AccountsCommands,
    },

    /// Order entry, preview, cancel, replace
    Orders {
        #[command(subcommand)]
        command: OrdersCommands,
    },

    /// Transaction history
    Transactions {
        #[command(subcommand)]
        command: TransactionsCommands,
    },

    /// User preferences and streamer metadata
    User {
        #[command(subcommand)]
        command: UserCommands,
    },

    /// Portfolio summary across linked accounts
    Portfolio {
        #[command(subcommand)]
        command: PortfolioCommands,
    },

    /// Buy or sell equities with safety guardrails
    Trade {
        #[command(subcommand)]
        command: TradeCommands,
    },

    /// Safety limits config (safety.json) for agent trading guardrails
    Safety {
        #[command(subcommand)]
        command: SafetyCommands,
    },

    /// Multi-step trade plans (YAML/JSON) for LLM-generated rebalances
    Plan {
        #[command(subcommand)]
        command: PlanCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum EnvCommands {
    /// JSON schema of supported environment variables
    Schema,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommands {
    /// Start OAuth login (opens browser, captures redirect code)
    Login {
        /// Authorization code if already obtained (skips browser)
        #[arg(long)]
        code: Option<String>,
    },
    /// Show token status
    Status,
    /// Refresh access token using refresh token
    Refresh,
    /// Remove stored tokens
    Logout,
}

#[derive(Debug, Subcommand)]
pub enum AccountsCommands {
    /// GET /accounts/accountNumbers
    Numbers,
    /// GET /accounts
    List {
        #[arg(long)]
        fields: Option<String>,
    },
    /// GET /accounts/{accountNumber}
    Get {
        account_number: String,
        #[arg(long)]
        fields: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum OrdersCommands {
    /// GET /accounts/{accountNumber}/orders
    List {
        account_number: String,
        #[arg(long)]
        from_entered_time: Option<String>,
        #[arg(long)]
        to_entered_time: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        max_results: Option<String>,
    },
    /// GET /orders (all linked accounts)
    All {
        #[arg(long)]
        from_entered_time: Option<String>,
        #[arg(long)]
        to_entered_time: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        max_results: Option<String>,
    },
    /// GET /accounts/{accountNumber}/orders/{orderId}
    Get {
        account_number: String,
        order_id: String,
    },
    /// Poll order status until filled, terminal, or timeout
    Wait {
        account_number: String,
        order_id: String,
        /// Wait condition: accepted | filled | terminal
        #[arg(long, default_value = "filled")]
        until: String,
        /// Max seconds to poll before giving up
        #[arg(long, default_value = "3600")]
        timeout_seconds: u64,
        /// Seconds between status polls
        #[arg(long, default_value = "5")]
        interval_seconds: u64,
        /// Treat partial fill as success when waiting for filled
        #[arg(long, default_value = "false")]
        proceed_on_partial_fill: bool,
    },
    /// POST /accounts/{accountNumber}/orders
    Place {
        account_number: String,
        /// Path to order JSON file or inline JSON string
        #[arg(long)]
        order: String,
    },
    /// POST /accounts/{accountNumber}/previewOrder
    Preview {
        account_number: String,
        #[arg(long)]
        order: String,
    },
    /// DELETE /accounts/{accountNumber}/orders/{orderId}
    Cancel {
        account_number: String,
        order_id: String,
    },
    /// PUT /accounts/{accountNumber}/orders/{orderId}
    Replace {
        account_number: String,
        order_id: String,
        #[arg(long)]
        order: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TransactionsCommands {
    /// GET /accounts/{accountNumber}/transactions
    List {
        account_number: String,
        #[arg(long)]
        start_date: Option<String>,
        #[arg(long)]
        end_date: Option<String>,
        #[arg(long)]
        types: Option<String>,
        #[arg(long)]
        symbol: Option<String>,
    },
    /// GET /accounts/{accountNumber}/transactions/{transactionId}
    Get {
        account_number: String,
        transaction_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum UserCommands {
    /// GET /userPreference
    Preference,
}

#[derive(Debug, Subcommand)]
pub enum PortfolioCommands {
    /// Aggregate positions and equity across all linked accounts
    Summary,
}

#[derive(Debug, Subcommand)]
pub enum TradeCommands {
    /// Buy shares (equity, single-leg)
    Buy {
        /// Account hash from `schwab accounts numbers`
        #[arg(long)]
        account_number: String,
        /// Ticker symbol
        #[arg(long)]
        symbol: String,
        /// Share quantity
        #[arg(long)]
        quantity: f64,
        /// Order type: market | limit
        #[arg(long, default_value = "market")]
        order_type: String,
        /// Limit price (required for limit orders)
        #[arg(long)]
        price: Option<f64>,
        /// Duration: day | gtc | fok
        #[arg(long)]
        duration: Option<String>,
        /// Session: normal | am | pm | seamless
        #[arg(long)]
        session: Option<String>,
    },
    /// Sell shares (equity, single-leg)
    Sell {
        #[arg(long)]
        account_number: String,
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        quantity: f64,
        #[arg(long, default_value = "market")]
        order_type: String,
        #[arg(long)]
        price: Option<f64>,
        #[arg(long)]
        duration: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SafetyCommands {
    /// Show active safety.json limits and config path
    Show,
    /// Write default safety.json to the config directory
    Init,
    /// Print safety.json path only
    Path,
}

#[derive(Debug, Subcommand)]
pub enum PlanCommands {
    /// JSON Schema for trade plan files
    Schema,
    /// LLM prompt and workflow for generating trade plans
    Prompt,
    /// Validate plan structure and safety limits
    Validate {
        /// Path to .yaml, .yml, or .json trade plan
        file: PathBuf,
    },
    /// Show parsed plan contents
    Show {
        file: PathBuf,
    },
    /// Execute plan steps (requires --trust --yes in agent mode, or --dry-run)
    Run {
        file: PathBuf,
        /// Run only this step id
        #[arg(long)]
        step: Option<String>,
        /// Run from this step id through the end
        #[arg(long)]
        from_step: Option<String>,
    },
}

/// Resolved output path for clap parse result.
impl Cli {
    pub fn effective_output(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else if self.md {
            OutputFormat::Md
        } else {
            self.output
        }
    }
}
