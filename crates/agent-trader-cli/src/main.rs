use anyhow::Result;
use clap::{Parser, Subcommand};

use agent_trader::load_dotenv;

#[derive(Parser)]
#[command(name = "agent-trader", version, about = "Broker-agnostic trading CLI")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    yes: bool,
    #[arg(long, global = true)]
    trust: bool,
    #[arg(long, global = true)]
    dry_run: bool,
    #[arg(long, global = true)]
    simulate: bool,
    #[arg(long, global = true)]
    broker: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List accounts for one backend, or every configured backend.
    Accounts,
    /// Positions, strategies, and buying power.
    Portfolio {
        account: String,
        #[arg(long)]
        data_source: Option<String>,
    },
    /// Quotes, chains, expirations, and bars.
    Market {
        #[command(subcommand)]
        command: MarketCmd,
    },
    /// Validate or run an equity plan. The plan's account picks the backend.
    Plan {
        #[command(subcommand)]
        command: PlanCmd,
    },
    /// Mechanical options book: entries, exits, rolls, and flatten.
    Options {
        #[command(subcommand)]
        command: OptionsCmd,
    },
    /// Copy safety.json into the agent-trader config directory.
    ImportSchwab,
    /// Show the safety config the new stack will enforce.
    Safety,
    /// Capability flags for each configured backend.
    Capabilities,
    /// Show whether a legacy Schwab agent already owns an account.
    Guard {
        account: String,
        #[arg(long)]
        rules_dir: Option<String>,
    },
    /// Read-only readiness check. Does not place an order.
    Canary {
        account: String,
    },
}

#[derive(Subcommand)]
enum MarketCmd {
    Quotes {
        symbols: Vec<String>,
        #[arg(long)]
        account: String,
    },
    Expirations {
        underlying: String,
        #[arg(long)]
        account: String,
    },
    Chain {
        underlying: String,
        expiration: String,
        #[arg(long)]
        account: String,
    },
    Bars {
        symbol: String,
        #[arg(long, default_value = "year")]
        period: String,
        #[arg(long)]
        account: String,
    },
}

#[derive(Subcommand)]
enum PlanCmd {
    Validate { path: String },
    Run { path: String },
}

#[derive(Subcommand)]
enum OptionsCmd {
    /// One decision pass. Live orders require --trust --yes.
    Tick {
        #[arg(long)]
        rules: String,
    },
    /// Close option strategies on the targeted accounts.
    Flatten {
        #[arg(long)]
        rules: String,
    },
    /// Replay entry selection against a saved chain JSON file.
    Replay {
        #[arg(long)]
        chain: String,
        #[arg(long, default_value = "2026-10-13")]
        today: String,
        #[arg(long)]
        rules: Option<String>,
    },
    /// Simulated Public options book. Never submits an order.
    Paper {
        #[arg(long, default_value = "rules/agent-trader-public-paper.yaml")]
        rules: String,
        #[arg(long)]
        once: bool,
        #[arg(long, default_value_t = 120)]
        interval_seconds: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    load_dotenv();
    let cli = Cli::parse();
    if cli.dry_run && cli.simulate {
        anyhow::bail!("--dry-run and --simulate are mutually exclusive");
    }
    let flags = agent_trader::TradingFlags {
        yes: cli.yes,
        trust: cli.trust,
        dry_run: cli.dry_run,
        simulate: cli.simulate,
    };
    match cli.command {
        Command::Accounts => accounts(cli.broker.as_deref()).await?,
        Command::Portfolio { account, data_source } => {
            portfolio(&account, data_source.as_deref()).await?
        }
        Command::Market { command } => market(command).await?,
        Command::Plan { command } => plan_cmd(command, &flags).await?,
        Command::Options { command } => options_cmd(command, &flags).await?,
        Command::ImportSchwab => import_schwab()?,
        Command::Safety => safety_show()?,
        Command::Capabilities => capabilities(cli.broker.as_deref()).await?,
        Command::Guard { account, rules_dir } => guard(&account, rules_dir.as_deref())?,
        Command::Canary { account } => canary(&account).await?,
    }
    Ok(())
}

fn print_json(value: serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
}

async fn accounts(broker: Option<&str>) -> Result<()> {
    let registry = agent_trader::build_registry()?;
    let kinds = selected_kinds(&registry, broker)?;
    let mut out = Vec::new();
    for kind in kinds {
        let handle = registry.get(kind)?;
        match handle.trading.accounts().await {
            Ok(rows) => out.extend(rows),
            Err(err) => eprintln!("{kind} accounts: {err}"),
        }
    }
    print_json(serde_json::json!({"accounts": out}));
    Ok(())
}

async fn portfolio(account: &str, data_source: Option<&str>) -> Result<()> {
    let registry = agent_trader::build_registry()?;
    let account = broker_core::parse_account_address(account)?;
    let source = broker_core::DataSource::parse(data_source, account.broker)?;
    let handle = registry.resolve(&account, source)?;
    let portfolio = handle.trading.portfolio(&account).await?;
    print_json(serde_json::json!(portfolio));
    Ok(())
}

async fn market(command: MarketCmd) -> Result<()> {
    let registry = agent_trader::build_registry()?;
    match command {
        MarketCmd::Quotes { symbols, account } => {
            let account = broker_core::parse_account_address(&account)?;
            let handle = registry.resolve(&account, broker_core::DataSource::AccountBroker)?;
            let quotes = handle.data.quotes(&account, &symbols).await?;
            print_json(serde_json::json!({
                "quotes": quotes,
                "source": handle.data.broker_name()
            }));
        }
        MarketCmd::Expirations { underlying, account } => {
            let account = broker_core::parse_account_address(&account)?;
            let handle = registry.resolve(&account, broker_core::DataSource::AccountBroker)?;
            let rows = handle.data.option_expirations(&account, &underlying).await?;
            print_json(serde_json::json!({"expirations": rows}));
        }
        MarketCmd::Chain { underlying, expiration, account } => {
            let account = broker_core::parse_account_address(&account)?;
            let handle = registry.resolve(&account, broker_core::DataSource::AccountBroker)?;
            let chain = handle.data.option_chain(&account, &underlying, &expiration).await?;
            print_json(serde_json::json!(chain));
        }
        MarketCmd::Bars { symbol, period, account } => {
            let account = broker_core::parse_account_address(&account)?;
            let handle = registry.resolve(&account, broker_core::DataSource::AccountBroker)?;
            let bars = handle.data.bars(&account, &symbol, &period).await?;
            print_json(serde_json::json!({"bars": bars}));
        }
    }
    Ok(())
}

async fn plan_cmd(command: PlanCmd, flags: &agent_trader::TradingFlags) -> Result<()> {
    match command {
        PlanCmd::Validate { path } => {
            let plan = agent_trader::TradePlan::load(&path)?;
            print_json(serde_json::json!({"ok": true, "plan_id": plan.plan_id, "steps": plan.steps.len()}));
        }
        PlanCmd::Run { path } => {
            let plan = agent_trader::TradePlan::load(&path)?;
            let registry = agent_trader::build_registry()?;
            let report = agent_trader::run_plan(&registry, &plan, flags).await?;
            print_json(report);
        }
    }
    Ok(())
}

async fn options_cmd(command: OptionsCmd, flags: &agent_trader::TradingFlags) -> Result<()> {
    match command {
        OptionsCmd::Tick { rules } => {
            let registry = agent_trader::build_registry()?;
            let report = agent_trader::options::tick(&registry, &rules, flags).await?;
            print_json(report);
        }
        OptionsCmd::Flatten { rules } => {
            let registry = agent_trader::build_registry()?;
            let report = agent_trader::options::flatten(&registry, &rules, flags).await?;
            print_json(report);
        }
        OptionsCmd::Replay { chain, today, rules } => {
            let report = agent_trader::options::replay_file(&chain, &today, rules.as_deref())?;
            print_json(report);
        }
        OptionsCmd::Paper {
            rules,
            once,
            interval_seconds,
        } => {
            if flags.trust || flags.yes {
                anyhow::bail!("options paper ignores live approval flags and never places orders");
            }
            agent_trader::paper::run_public_paper(&rules, once, interval_seconds).await?;
        }
    }
    Ok(())
}

fn import_schwab() -> Result<()> {
    let dest_dir = agent_trader::config_dir()?;
    std::fs::create_dir_all(&dest_dir)?;
    let dest = agent_trader::safety_path()?;
    if dest.is_file() {
        println!("safety config already exists at {}", dest.display());
        return Ok(());
    }
    let source = directories::ProjectDirs::from("", "", "schwabinvestbot")
        .map(|dirs| dirs.config_dir().join("safety.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("safety.json"));
    if source.is_file() {
        std::fs::copy(&source, &dest)?;
        println!("copied {} to {}", source.display(), dest.display());
    } else {
        let gate = agent_trader::SafetyGate::default();
        std::fs::write(&dest, serde_json::to_string_pretty(&gate)?)?;
        println!("wrote default safety config to {}", dest.display());
    }
    println!("Schwab tokens stay in the schwabinvestbot token directory. PUBLIC_API_KEY stays in .env.");
    Ok(())
}

fn safety_show() -> Result<()> {
    let gate = agent_trader::SafetyGate::load()?;
    print_json(serde_json::json!(gate));
    Ok(())
}

async fn capabilities(broker: Option<&str>) -> Result<()> {
    let registry = agent_trader::build_registry()?;
    let kinds = selected_kinds(&registry, broker)?;
    let mut rows = Vec::new();
    for kind in kinds {
        let handle = registry.get(kind)?;
        rows.push(handle.trading.capabilities());
    }
    print_json(serde_json::json!({"backends": rows}));
    Ok(())
}

fn guard(account: &str, rules_dir: Option<&str>) -> Result<()> {
    let account = broker_core::parse_account_address(account)?;
    let dir = rules_dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("rules"));
    let busy = agent_trader::legacy_account_busy(&dir, &account.id)?;
    print_json(serde_json::json!({
        "account": account.address(),
        "legacy_busy": busy.is_some(),
        "reason": busy,
    }));
    Ok(())
}

async fn canary(account: &str) -> Result<()> {
    let registry = agent_trader::build_registry()?;
    let account = broker_core::parse_account_address(account)?;
    let handle = registry.resolve(&account, broker_core::DataSource::AccountBroker)?;
    let portfolio = handle.trading.portfolio(&account).await?;
    let funded = portfolio.balances.cash_buying_power > 0.0 || portfolio.balances.equity > 0.0;
    let quotes = handle.data.quotes(&account, &["AAPL".into()]).await?;
    let limit = quotes
        .first()
        .and_then(|quote| quote.ask.or(quote.last))
        .filter(|price| *price > 0.0)
        .unwrap_or(1.0);
    let probe = broker_core::OrderIntent::Equity {
        side: broker_core::Side::Buy,
        symbol: "AAPL".into(),
        size: broker_core::EquitySize::Shares(1.0),
        limit: Some(limit),
        tif: broker_core::TimeInForce::Day,
        session: broker_core::Session::Core,
        bracket: None,
    };
    let preflight = handle.trading.preflight(&account, &probe).await?;
    let message = preflight.message.replace(&account.id, "<account>");
    print_json(serde_json::json!({
        "account": account.address().replace(&account.id, "<account>"),
        "read_ok": true,
        "funded": funded,
        "cash_buying_power": portfolio.balances.cash_buying_power,
        "equity": portfolio.balances.equity,
        "preflight_limit": limit,
        "preflight_accepted": preflight.accepted,
        "preflight_message": message,
        "order_placed": false,
        "note": "Canary check never submits an order. Fund the account before a live order."
    }));
    Ok(())
}

fn selected_kinds(
    registry: &broker_core::BrokerRegistry,
    broker: Option<&str>,
) -> Result<Vec<broker_core::BrokerKind>> {
    if let Some(broker) = broker {
        return Ok(vec![broker_core::BrokerKind::parse(broker)?]);
    }
    let mut kinds = Vec::new();
    for kind in [broker_core::BrokerKind::Schwab, broker_core::BrokerKind::Public] {
        if registry.get(kind).is_ok() {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        anyhow::bail!("no backends configured; set SCHWAB_APP_KEY and/or PUBLIC_API_KEY");
    }
    Ok(kinds)
}
