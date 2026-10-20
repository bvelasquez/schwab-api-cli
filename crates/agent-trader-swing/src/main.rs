use std::path::Path;

use anyhow::{Context, Result};
use broker_core::{
    parse_account_address, AccountRef, BrokerKind, DataSource, EquitySize, ExitBracket,
    OrderIntent, Session, Side, TimeInForce,
};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::{json, Value};

mod indicators;
mod instructions;
mod paper;
mod schedule;
mod workflow;
mod workflow_paper;

use agent_trader::{
    build_registry, legacy_account_busy, load_dotenv, require_trading_approval, AccountLock,
    SafetyGate, TradingFlags,
};

#[derive(Parser)]
#[command(
    name = "agent-trader-swing",
    version,
    about = "Equity swing book on Schwab or Public"
)]
struct Cli {
    #[arg(long, global = true)]
    yes: bool,
    #[arg(long, global = true)]
    trust: bool,
    #[arg(long, global = true)]
    dry_run: bool,
    #[arg(long, global = true)]
    simulate: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One pass: reconcile, thesis exits, then entries inside the capital sleeve.
    Tick {
        #[arg(long)]
        rules: String,
    },
    /// Record what a tick would do and do not send orders.
    Shadow {
        #[arg(long)]
        rules: String,
    },
    /// Simulated Public swing book. Never submits an order.
    Paper {
        #[arg(long, default_value = "rules/agent-trader-public-swing-paper.yaml")]
        rules: String,
        #[arg(long)]
        once: bool,
        #[arg(long, default_value_t = 120)]
        interval_seconds: u64,
    },
    /// The equity swing agent. Same loop and state files as `schwab-trader agent run`.
    /// Paper units pass `--simulate` and never `--trust` or `--yes`.
    Book {
        #[arg(long)]
        rules: String,
        #[arg(long)]
        once: bool,
    },
    /// Run cron workflows until stopped. `--dir` paper-runs every workflow file and records the dashboard book.
    Run {
        /// One live rules file. Requires a real account. Sends orders only with `--trust --yes`.
        #[arg(long)]
        rules: Option<String>,
        /// Directory of workflow files. Paper only: quotes, a local book, no orders.
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        once: bool,
        #[arg(long, default_value_t = 120)]
        interval_seconds: u64,
    },
    /// Schema, prompt, and validation for continuous workflows.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCmd,
    },
}

#[derive(Subcommand)]
enum WorkflowCmd {
    /// JSON Schema for a workflow file, including the fields each step binds.
    Schema {
        #[arg(long)]
        json: bool,
    },
    /// Instructions for an agent that wants to author a workflow.
    Prompt {
        #[arg(long)]
        json: bool,
    },
    /// Parse a workflow file and report unknown names.
    Validate {
        #[arg(long)]
        rules: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    schwab_cli::tls::install_crypto_provider();
    load_dotenv();
    let cli = Cli::parse();
    let flags = TradingFlags {
        yes: cli.yes,
        trust: cli.trust,
        dry_run: cli.dry_run,
        simulate: cli.simulate,
    };
    match cli.command {
        Command::Tick { rules } => {
            let report = tick(&rules, &flags, false).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Shadow { rules } => {
            let report = tick(&rules, &flags, true).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Paper {
            rules,
            once,
            interval_seconds,
        } => {
            if flags.trust || flags.yes {
                anyhow::bail!("swing paper ignores live approval flags and never places orders");
            }
            if workflow_paper::file_has_workflows(&rules) {
                workflow_paper::run(&rules, once, interval_seconds).await?;
            } else {
                paper::run_public_paper(&rules, once, interval_seconds).await?;
            }
        }
        Command::Book { rules, once } => {
            run_swing_book(Path::new(&rules), once, &flags).await?;
        }
        Command::Run {
            rules,
            dir,
            once,
            interval_seconds,
        } => {
            if dir.is_some() && (flags.trust || flags.yes) {
                anyhow::bail!("workflow directory run is paper only and never places orders");
            }
            match (rules, dir) {
                (Some(_), Some(_)) => anyhow::bail!("pass --rules or --dir, not both"),
                (None, Some(dir)) => workflow_paper::run_dir(&dir, once, interval_seconds).await?,
                (Some(rules), None) => run_workflows(&rules, &flags, once).await?,
                (None, None) => anyhow::bail!("pass --rules <file> or --dir rules/workflows"),
            }
        }
        Command::Workflow { command } => match command {
            WorkflowCmd::Schema { .. } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&instructions::workflow_schema())?
                );
            }
            WorkflowCmd::Prompt { .. } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&instructions::workflow_prompt())?
                );
            }
            WorkflowCmd::Validate { rules } => {
                let report = workflow::validate_report(Path::new(&rules))?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        },
    }
    Ok(())
}

async fn run_workflows(path: &str, flags: &TradingFlags, once: bool) -> Result<()> {
    let path = Path::new(path);
    let registry = build_registry()?;
    loop {
        let file = workflow::load_workflow(path)?;
        let account = file.account_ref()?;
        if workflow::is_placeholder(&account) {
            anyhow::bail!("replace account with a real public:ID or schwab:HASH before run");
        }
        let handle = registry.resolve(&account, file.data_source()?)?;
        let safety = SafetyGate::load()?;
        let state_path = workflow::state_path_for(path)?;
        let now = chrono::Utc::now();
        let report = workflow::run_due(
            &file,
            handle.trading.as_ref(),
            handle.data.as_ref(),
            &account,
            &safety,
            flags,
            &state_path,
            path,
            now,
            true,
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        if once {
            return Ok(());
        }
        let wait = workflow::seconds_until_next(&file, &state_path, chrono::Utc::now());
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
    }
}

#[derive(Debug, Deserialize)]
struct SwingRules {
    #[serde(default)]
    accounts: Vec<SwingAccount>,
    #[serde(default)]
    broker: Option<String>,
    #[serde(default)]
    data_source: Option<String>,
    #[serde(default)]
    capital: Capital,
    #[serde(default)]
    watchlists: Value,
    #[serde(default)]
    exits: SwingExits,
}

#[derive(Debug, Deserialize)]
struct SwingAccount {
    #[serde(default)]
    hash: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    broker: Option<String>,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct Capital {
    #[serde(default = "sleeve")]
    fixed_sleeve_cap_usd: f64,
    #[serde(default = "floor")]
    min_cash_floor_usd: f64,
}

impl Default for Capital {
    fn default() -> Self {
        Self {
            fixed_sleeve_cap_usd: 4_000.0,
            min_cash_floor_usd: 0.0,
        }
    }
}

fn sleeve() -> f64 {
    4_000.0
}
fn floor() -> f64 {
    0.0
}

#[derive(Debug, Deserialize)]
struct SwingExits {
    #[serde(default = "profit")]
    profit_pct: f64,
    #[serde(default = "stop")]
    stop_pct: f64,
    #[serde(default = "thesis")]
    invalidation_pct: f64,
}

impl Default for SwingExits {
    fn default() -> Self {
        Self {
            profit_pct: 8.0,
            stop_pct: 4.0,
            invalidation_pct: 6.0,
        }
    }
}

fn profit() -> f64 {
    8.0
}
fn stop() -> f64 {
    4.0
}
fn thesis() -> f64 {
    6.0
}

async fn tick(path: &str, flags: &TradingFlags, shadow: bool) -> Result<Value> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    let rules: SwingRules = serde_yaml::from_str(&raw)?;
    let registry = build_registry()?;
    let safety = SafetyGate::load()?;
    let symbols = symbols_in(&rules.watchlists);
    let mut rows = Vec::new();
    for account in accounts(&rules)? {
        let handle = registry.resolve(
            &account,
            DataSource::parse(rules.data_source.as_deref(), account.broker)?,
        )?;
        let portfolio = handle.trading.portfolio(&account).await?;
        let held: Vec<_> = portfolio
            .positions
            .iter()
            .filter(|position| position.quantity.abs() > 0.0)
            .cloned()
            .collect();
        rows.push(json!({
            "account": account.address(),
            "reconcile": {
                "positions": held.len(),
                "cash": portfolio.balances.cash_buying_power,
            }
        }));
        for position in &held {
            let quotes = handle
                .data
                .quotes(&account, &[position.symbol.clone()])
                .await?;
            let last = quotes.first().and_then(|quote| quote.last);
            let average = position.average_price.unwrap_or(0.0);
            if let (Some(last), true) = (last, average > 0.0) {
                let drop_pct = (average - last) / average * 100.0;
                if drop_pct >= rules.exits.invalidation_pct {
                    let intent = sell_intent(&position.symbol, position.quantity, last);
                    rows.push(
                        decide(
                            &account,
                            &handle,
                            &safety,
                            &intent,
                            portfolio.balances.equity,
                            flags,
                            shadow,
                            path,
                            "thesis_exit",
                        )
                        .await?,
                    );
                }
            }
        }
        let spendable = (portfolio.balances.cash_buying_power - rules.capital.min_cash_floor_usd)
            .max(0.0)
            .min(rules.capital.fixed_sleeve_cap_usd);
        if held.is_empty() {
            for symbol in &symbols {
                let quotes = handle.data.quotes(&account, &[symbol.clone()]).await?;
                let Some(quote) = quotes.first() else {
                    continue;
                };
                let price = quote.bid.or(quote.last).unwrap_or(0.0);
                if price <= 0.0 || spendable < price {
                    continue;
                }
                let qty = (spendable / price).floor();
                if qty < 1.0 {
                    continue;
                }
                let stop = price * (1.0 - rules.exits.stop_pct / 100.0);
                let target = price * (1.0 + rules.exits.profit_pct / 100.0);
                let risk = (price - stop) * qty;
                let caps = handle.trading.capabilities();
                let bracket = if caps.native_bracket {
                    Some(ExitBracket {
                        symbol: symbol.clone(),
                        quantity: qty,
                        take_profit: target,
                        stop,
                        stop_limit: stop * 0.995,
                        tif: TimeInForce::Gtc,
                    })
                } else {
                    None
                };
                let intent = OrderIntent::Equity {
                    side: Side::Buy,
                    symbol: symbol.clone(),
                    size: EquitySize::Shares(qty),
                    limit: Some(price),
                    tif: TimeInForce::Day,
                    session: Session::Core,
                    bracket,
                };
                let order = decide(
                    &account,
                    &handle,
                    &safety,
                    &intent,
                    portfolio.balances.equity,
                    flags,
                    shadow,
                    path,
                    "entry",
                )
                .await?;
                rows.push(json!({"risk_usd": risk, "order": order}));
                if !caps.native_bracket && !shadow && !flags.dry_run && !flags.simulate {
                    let exit = OrderIntent::ExitBracket(ExitBracket {
                        symbol: symbol.clone(),
                        quantity: qty,
                        take_profit: target,
                        stop,
                        stop_limit: stop * 0.995,
                        tif: TimeInForce::Gtc,
                    });
                    rows.push(
                        decide(
                            &account,
                            &handle,
                            &safety,
                            &exit,
                            portfolio.balances.equity,
                            flags,
                            shadow,
                            path,
                            "post_fill_oco",
                        )
                        .await?,
                    );
                }
                break;
            }
        }
    }
    Ok(json!({"shadow": shadow, "rows": rows}))
}

async fn decide(
    account: &AccountRef,
    handle: &broker_core::BrokerHandle,
    safety: &SafetyGate,
    intent: &OrderIntent,
    equity: f64,
    flags: &TradingFlags,
    shadow: bool,
    rules_path: &str,
    action: &str,
) -> Result<Value> {
    safety.check(account, intent, equity)?;
    if shadow || flags.dry_run || flags.simulate {
        return Ok(json!({
            "account": account.address(),
            "action": action,
            "status": if shadow { "shadow" } else if flags.simulate { "simulated" } else { "dry_run" },
        }));
    }
    require_trading_approval(flags, "swing tick")?;
    let rules_dir = Path::new(rules_path).parent().unwrap_or(Path::new("rules"));
    if let Some(reason) = legacy_account_busy(rules_dir, &account.id)? {
        anyhow::bail!(reason);
    }
    let _lock = AccountLock::acquire(
        &agent_trader::state_dir()?.join("locks"),
        &account.address(),
    )?;
    let preflight = handle.trading.preflight(account, intent).await?;
    if !preflight.accepted {
        return Ok(json!({"action": action, "status": "rejected", "message": preflight.message}));
    }
    let placed = handle
        .trading
        .place(account, intent, &uuid::Uuid::new_v4().to_string())
        .await?;
    Ok(json!({"action": action, "status": "submitted", "order_id": placed.order_id}))
}

fn sell_intent(symbol: &str, quantity: f64, limit: f64) -> OrderIntent {
    OrderIntent::Equity {
        side: Side::Sell,
        symbol: symbol.to_string(),
        size: EquitySize::Shares(quantity.abs()),
        limit: Some(limit),
        tif: TimeInForce::Day,
        session: Session::Core,
        bracket: None,
    }
}

fn accounts(rules: &SwingRules) -> Result<Vec<AccountRef>> {
    let mut out = Vec::new();
    for row in &rules.accounts {
        if !row.enabled {
            continue;
        }
        let id = row
            .id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| row.hash.clone());
        if id.is_empty() || id.starts_with('<') {
            continue;
        }
        let mut account = parse_account_address(&id)?;
        if let Some(broker) = row.broker.as_deref().or(rules.broker.as_deref()) {
            account.broker = BrokerKind::parse(broker)?;
            if !id.contains(':') {
                account.id = id;
            }
        }
        out.push(account);
    }
    Ok(out)
}

async fn run_swing_book(rules: &Path, once: bool, flags: &TradingFlags) -> Result<()> {
    let safety = schwab_cli::safety_config::SafetyConfig::load()?;
    let runtime = schwab_trader::config::TraderRuntime {
        output: schwab_cli::output::OutputFormat::Pretty,
        yes: flags.yes,
        dry_run: flags.dry_run,
        simulate: flags.simulate,
        trust: flags.trust,
        suppress_tick_output: false,
        no_audio: false,
        safety: schwab_cli::safety::SafetyContext::new(safety),
        sink: schwab_cli::output::OutputSink::stdout(),
    };
    schwab_trader::agent::run_agent_loop(
        &runtime,
        rules,
        schwab_trader::agent::runner::AgentRunOptions { once, health: None },
    )
    .await
}

pub(crate) fn symbols_in(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(value, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(symbol) => {
            if !symbol.is_empty() {
                out.push(symbol.to_uppercase());
            }
        }
        Value::Array(rows) => {
            for row in rows {
                walk(row, out);
            }
        }
        Value::Object(map) => {
            for (key, row) in map {
                if key == "symbol" {
                    if let Some(symbol) = row.as_str() {
                        out.push(symbol.to_uppercase());
                    }
                } else {
                    walk(row, out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_watchlists() {
        let value = serde_json::json!({"core": ["spy", "AAPL"], "extra": [{"symbol": "msft"}]});
        let symbols = symbols_in(&value);
        assert_eq!(symbols, vec!["AAPL", "MSFT", "SPY"]);
    }
}
