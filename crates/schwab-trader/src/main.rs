use anyhow::Result;
use clap::Parser;
use schwab_trader::cli::{Cli, Commands};
use schwab_trader::config::TraderRuntime;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    schwab_cli::tls::install_crypto_provider();
    if let Err(err) = run().await {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    load_dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let runtime = TraderRuntime::from_cli(&cli)?;

    match cli.command {
        Some(Commands::Rules { command }) => {
            schwab_trader::commands::rules_cmd::run(&runtime, command).await
        }
        Some(Commands::Capital { rules_file }) => {
            schwab_trader::commands::capital_cmd::run_show(&runtime, &rules_file).await
        }
        Some(Commands::Scan { rules_file }) => {
            schwab_trader::commands::scan_cmd::run(&runtime, &rules_file).await
        }
        Some(Commands::Trade { command }) => {
            schwab_trader::commands::trade_cmd::run(&runtime, command).await
        }
        Some(Commands::Journal { command }) => {
            schwab_trader::commands::journal_cmd::run(&runtime, command).await
        }
        Some(Commands::Agent { command }) => {
            schwab_trader::commands::agent_cmd::run(&runtime, command).await
        }
        Some(Commands::Watch {
            rules_file,
            monitor_only,
        }) => {
            schwab_trader::commands::watch_cmd::run(&runtime, &rules_file, monitor_only).await
        }
        Some(Commands::Sim { command }) => {
            schwab_trader::commands::sim_cmd::run(&runtime, command).await
        }
        Some(Commands::Backtest { command }) => {
            schwab_trader::commands::backtest_cmd::run(&runtime, command).await
        }
        Some(Commands::Sources { command }) => {
            schwab_trader::commands::sources_cmd::run(&runtime, command).await
        }
        Some(Commands::Watchlist { command }) => {
            schwab_trader::commands::watchlist_cmd::run(&runtime, command).await
        }
        None => {
            eprintln!("schwab-trader — equity swing agent. Run with --help.");
            Ok(())
        }
    }
}

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(mut path) = dir {
        let candidate = path.join(".env");
        if candidate.is_file() {
            let _ = dotenvy::from_path_override(&candidate);
            return;
        }
        if !path.pop() {
            break;
        }
        dir = Some(path);
    }
}
