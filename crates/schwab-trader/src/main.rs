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
        Some(Commands::Dashboard {
            rules_file,
            options_rules,
            bind,
        }) => {
            let paths = schwab_trader::dashboard::DashboardPaths {
                swing: resolve_rules(rules_file, "rules/trader-swing-9947.yaml"),
                options: resolve_rules(options_rules, "rules/options-pilot-8709.yaml"),
            };
            if paths.swing.is_empty() && paths.options.is_empty() {
                anyhow::bail!("no rules files found; pass --rules-file and/or --options-rules");
            }
            schwab_trader::dashboard::serve(&bind, paths)
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
        Some(Commands::Shadow { command }) => {
            schwab_trader::commands::shadow_cmd::run(&runtime, command).await
        }
        Some(Commands::LlmSignal { rules_file }) => {
            let summary = schwab_trader::agent::llm_signal::run_signal_batch(&rules_file).await?;
            println!("{summary}");
            Ok(())
        }
        Some(Commands::LlmAgent { rules_file }) => {
            let summary = schwab_trader::agent::llm_signal::run_agent_batch(&rules_file).await?;
            println!("{summary}");
            Ok(())
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

fn resolve_rules(given: Vec<std::path::PathBuf>, default_rel: &str) -> Vec<std::path::PathBuf> {
    if !given.is_empty() {
        return given;
    }
    let path = std::path::PathBuf::from(default_rel);
    if path.is_file() {
        vec![path]
    } else {
        Vec::new()
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
