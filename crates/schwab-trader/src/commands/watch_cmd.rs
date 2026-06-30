use std::path::Path;

use anyhow::{Context, Result};

use crate::agent::runner::{run_agent_loop, AgentRunOptions};
use crate::config::TraderRuntime;
use crate::rules::TraderRules;
use crate::ui::health::new_shared_health;
use crate::ui::live_feed::{new_live_snapshot, spawn_live_quote_feed};
use crate::ui::{run_watch_tui, WatchAgentMode, WatchConfig};
use schwab_cli::safety::require_trading_approval;

pub async fn run(
    runtime: &TraderRuntime,
    rules_path: &Path,
    monitor_only: bool,
) -> Result<()> {
    if runtime.output == schwab_cli::output::OutputFormat::Json {
        anyhow::bail!("watch is interactive only — omit --json");
    }

    let rules = TraderRules::load(rules_path)?;
    let agent_mode = if monitor_only {
        WatchAgentMode::MonitorOnly
    } else {
        WatchAgentMode::Embedded
    };

    let will_spawn = matches!(agent_mode, WatchAgentMode::Embedded);

    if will_spawn && !runtime.dry_run && !runtime.simulate {
        require_trading_approval(
            &runtime.as_schwab_runtime(),
            "trader watch",
            &format!("Run and watch swing trader `{}`", rules.trader_id),
        )?;
    }

    let mut agent_runtime = runtime.clone();
    agent_runtime.suppress_tick_output = true;

    let agent_health = if will_spawn {
        Some(new_shared_health())
    } else {
        None
    };

    let agent_handle = if will_spawn {
        let path = rules_path.to_path_buf();
        let health = agent_health.clone().expect("health");
        let rt = agent_runtime.clone();
        Some(tokio::spawn(async move {
            let result = run_agent_loop(&rt, &path, AgentRunOptions { once: false }).await;
            if let Err(e) = result {
                let msg = format!("agent exited: {e:#}");
                let _ = crate::agent::paths::append_trader_log(&path, &msg);
                if let Ok(mut g) = health.lock() {
                    g.loop_running = false;
                    g.last_error = Some(msg);
                }
            }
        }))
    } else {
        None
    };

    if will_spawn {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    let live = new_live_snapshot();
    let _quote_feed = match runtime.build_market_api() {
        Ok(market) => Some(spawn_live_quote_feed(
            rules_path.to_path_buf(),
            live.clone(),
            market,
        )),
        Err(err) => {
            if let Ok(mut g) = live.write() {
                g.last_error = Some(format!("market API: {err:#}"));
            }
            None
        }
    };

    let watch_config = WatchConfig {
        rules_path: rules_path.to_path_buf(),
        agent_mode,
        dry_run: runtime.dry_run,
        simulate: runtime.simulate,
        agent_health,
        live,
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
