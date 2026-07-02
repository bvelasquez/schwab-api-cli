mod agent;
mod auth_callback;
mod auth_reminder;
mod capabilities;
mod cli;
mod commands;
mod config;
mod disclaimer;
mod env_schema;
mod human;
mod instructions;
mod market_conditions;
mod flatten;
mod market_hours;
mod market_info;
mod mode;
mod notify;
mod options;
mod order_builder;
mod order_schema;
mod order_status;
mod output;
mod plan;
mod portfolio;
mod rules;
mod safety;
mod safety_config;
mod tls;
mod ui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use tracing_subscriber::EnvFilter;

use crate::config::RuntimeConfig;
use crate::output::ResponseEnvelope;

#[tokio::main]
async fn main() {
    tls::install_crypto_provider();
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
    let runtime = RuntimeConfig::from_cli(&cli)?;

    if cli.help_json {
        return print_help_json(&runtime);
    }

    if !should_skip_first_run_banner(&cli.command) {
        disclaimer::maybe_show_first_run()?;
    }

    match cli.command {
        Some(Commands::Capabilities) => commands::meta::capabilities(&runtime).await,
        Some(Commands::Env { command }) => commands::meta::env(&runtime, command).await,
        Some(Commands::Instructions) => commands::meta::instructions(&runtime).await,
        Some(Commands::Disclaimer { command }) => {
            commands::disclaimer::run(&runtime, command).await
        }
        Some(Commands::Auth { command }) => commands::auth::run(&runtime, command).await,
        Some(Commands::Accounts { command }) => commands::accounts::run(&runtime, command).await,
        Some(Commands::Orders { command }) => commands::orders::run(&runtime, command).await,
        Some(Commands::Transactions { command }) => {
            commands::transactions::run(&runtime, command).await
        }
        Some(Commands::User { command }) => commands::user::run(&runtime, command).await,
        Some(Commands::Portfolio { command }) => {
            commands::trading::run_portfolio(&runtime, command).await
        }
        Some(Commands::Trade { command }) => commands::trading::run_trade(&runtime, command).await,
        Some(Commands::Safety { command }) => {
            commands::trading::run_safety(&runtime, command).await
        }
        Some(Commands::Plan { command }) => commands::plan::run(&runtime, command).await,
        Some(Commands::Market { command }) => commands::market::run(&runtime, command).await,
        Some(Commands::Options { command }) => commands::options::run(&runtime, command).await,
        Some(Commands::Agent { command }) => commands::agent::run(&runtime, command).await,
        Some(Commands::Dashboard { file }) => commands::ui_cmd::run_dashboard(&runtime, file).await,
        Some(Commands::Watch { file, monitor_only }) => {
            commands::ui_cmd::run_watch(&runtime, file, monitor_only).await
        }
        Some(Commands::Rules { command }) => commands::ui_cmd::run_rules(&runtime, command).await,
        None => {
            if runtime.mode.is_human()
                && runtime.output == crate::output::OutputFormat::Pretty
                && runtime.is_tty()
            {
                ui::menu::run_interactive_menu(&runtime).await
            } else {
                print_top_level_help(&runtime);
                Ok(())
            }
        }
    }
}

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(mut path) = dir {
        let candidate = path.join(".env");
        if candidate.is_file() {
            // Project .env is source of truth (overrides stale keys from shell profile).
            let _ = dotenvy::from_path_override(&candidate);
            return;
        }
        if !path.pop() {
            break;
        }
        dir = Some(path);
    }

    if let Some(home) = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()) {
        let fallback = home.join(".config/schwabinvestbot/.env");
        if fallback.is_file() {
            let _ = dotenvy::from_path_override(&fallback);
        }
    }
}

fn should_skip_first_run_banner(command: &Option<Commands>) -> bool {
    matches!(command, Some(Commands::Disclaimer { .. }))
}

fn print_top_level_help(runtime: &RuntimeConfig) {
    let envelope = ResponseEnvelope::ok(
        "help",
        serde_json::json!({
            "message": "Schwab Trader API CLI — agent-first. Run subcommand --help for details.",
            "discovery": [
                "schwab --help --json",
                "schwab capabilities --json",
                "schwab env schema --json",
                "schwab instructions --json",
                "schwab dashboard",
                "schwab watch"
            ],
            "mode": runtime.mode.as_str(),
        }),
    )
    .with_next_actions(vec![
        "schwab auth login".into(),
        "schwab accounts numbers --json".into(),
    ]);
    runtime.emit(envelope);
}

fn print_help_json(runtime: &RuntimeConfig) -> Result<()> {
    let tree = capabilities::command_tree();
    let envelope = ResponseEnvelope::ok(
        "help-json",
        serde_json::json!({
            "name": "schwab",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Agent-first CLI for Charles Schwab Trader API (Accounts and Trading Production)",
            "experimental": true,
            "disclaimer": crate::disclaimer::HELP_DISCLAIMER,
            "disclaimer_commands": [
                "schwab disclaimer show",
                "schwab disclaimer accept --yes",
                "schwab disclaimer status --json"
            ],
            "base_url": schwab_api::TRADER_BASE_URL,
            "modes": ["agent", "human"],
            "output_formats": ["pretty", "json", "md"],
            "commands": tree,
            "env_schema_hint": "schwab env schema --json",
            "capabilities_hint": "schwab capabilities --json",
        }),
    );
    runtime.emit(envelope);
    Ok(())
}
