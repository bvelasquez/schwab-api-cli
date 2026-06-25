use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::agent::{daemon_status, run_agent_loop};
use crate::cli::RulesCommands;
use crate::config::RuntimeConfig;
use crate::rules::RulesConfig;
use crate::safety::require_trading_approval;
use crate::ui::discover::{list_rules_files_display, resolve_rules_file};
use crate::ui::menu::{list_rules_files, show_dashboard, show_rules};
use crate::ui::watch::{run_watch_tui, WatchAgentMode, WatchConfig};

pub async fn run_dashboard(runtime: &RuntimeConfig, file: Option<PathBuf>) -> Result<()> {
    show_dashboard(runtime, file).await
}

pub async fn run_watch(
    runtime: &RuntimeConfig,
    file: Option<PathBuf>,
    monitor_only: bool,
) -> Result<()> {
    if runtime.output == crate::output::OutputFormat::Json {
        anyhow::bail!("watch is interactive only — omit --json");
    }

    let rules_path = resolve_rules_file(file, runtime.is_interactive())?;
    let daemon = daemon_status(&rules_path);

    let agent_mode = if daemon.running {
        WatchAgentMode::External
    } else if monitor_only {
        WatchAgentMode::MonitorOnly
    } else {
        WatchAgentMode::Embedded
    };

    let will_spawn_agent = matches!(agent_mode, WatchAgentMode::Embedded);

    if will_spawn_agent && !runtime.dry_run {
        let agent_id = RulesConfig::load(&rules_path)
            .map(|r| r.agent_id)
            .unwrap_or_else(|_| "agent".into());
        require_trading_approval(
            runtime,
            "watch",
            &format!("Run and watch options agent `{agent_id}`"),
        )?;
    }

    let mut agent_runtime = runtime.clone();
    agent_runtime.suppress_tick_output = true;

    let agent_handle = if will_spawn_agent {
        let path = rules_path.clone();
        Some(tokio::spawn(async move {
            run_agent_loop(&agent_runtime, &path, false).await
        }))
    } else {
        None
    };

    // Brief pause so the first tick can start before the TUI loads.
    if will_spawn_agent {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    let watch_config = WatchConfig {
        rules_path: rules_path.clone(),
        agent_mode,
    };

    let watch_result = tokio::task::spawn_blocking(move || run_watch_tui(&watch_config))
        .await
        .context("watch UI thread panicked")?;

    if let Some(handle) = agent_handle {
        handle.abort();
        let _ = handle.await;
    }

    watch_result
}

pub async fn run_rules(runtime: &RuntimeConfig, command: RulesCommands) -> Result<()> {
    match command {
        RulesCommands::Show { file } => show_rules(runtime, file).await,
        RulesCommands::List => {
            if runtime.output == crate::output::OutputFormat::Json {
                list_rules_files(runtime);
            } else {
                println!("\n{}", list_rules_files_display());
                println!();
            }
            Ok(())
        }
    }
}
