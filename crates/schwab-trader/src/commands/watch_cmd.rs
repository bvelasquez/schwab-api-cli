use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::agent::runner::{run_agent_loop, AgentRunOptions};
use crate::config::TraderRuntime;
use crate::rules::TraderRules;
use crate::ui::health::{new_shared_health, update_health, SharedAgentHealth};
use crate::ui::live_feed::{new_live_snapshot, spawn_live_quote_feed};
use crate::ui::{run_watch_tui, WatchAgentMode, WatchConfig};
use schwab_cli::market_conditions::{spawn_market_conditions_feed, MarketConditionsSnapshot};
use schwab_cli::safety::require_trading_approval;
use std::sync::{Arc, Mutex};

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
            supervise_agent_loop(rt, path, health).await;
        }))
    } else {
        None
    };

    if will_spawn {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let live = new_live_snapshot();
    let market_conditions = Arc::new(Mutex::new(MarketConditionsSnapshot::default()));
    let _quote_feed = match runtime.build_market_api() {
        Ok(market) => {
            if let Ok(mut guard) = market_conditions.lock() {
                schwab_cli::market_conditions::refresh_market_conditions(&market, &mut guard).await;
            }
            let _conditions_feed =
                spawn_market_conditions_feed(market.clone(), market_conditions.clone());
            Some(spawn_live_quote_feed(
                rules_path.to_path_buf(),
                live.clone(),
                market,
            ))
        }
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
        market_conditions,
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

/// Belt-and-suspenders: if `run_agent_loop` ever returns, restart with backoff.
async fn supervise_agent_loop(rt: TraderRuntime, path: PathBuf, health: SharedAgentHealth) {
    let mut restart_backoff_secs: u64 = 5;
    loop {
        update_health(&health, |g| {
            g.loop_running = true;
        });
        let result = run_agent_loop(
            &rt,
            &path,
            AgentRunOptions {
                once: false,
                health: Some(health.clone()),
            },
        )
        .await;

        match result {
            Ok(()) => {
                // Clean exit (should not happen for once:false) — stop supervising.
                update_health(&health, |g| g.record_loop_stopped(None));
                let _ = crate::agent::paths::append_trader_log(
                    &path,
                    "agent supervisor: loop returned Ok — stopping",
                );
                break;
            }
            Err(e) => {
                let msg = format!("agent loop exited: {e:#}");
                let _ = crate::agent::paths::append_trader_log(&path, &msg);
                update_health(&health, |g| {
                    g.record_supervisor_restart();
                    g.record_loop_stopped(Some(msg.clone()));
                });
                let _ = crate::agent::paths::append_trader_log(
                    &path,
                    &format!(
                        "agent supervisor: restarting in {restart_backoff_secs}s (restart #{})",
                        health
                            .lock()
                            .map(|g| g.restart_count)
                            .unwrap_or(0)
                    ),
                );
                tokio::time::sleep(Duration::from_secs(restart_backoff_secs)).await;
                restart_backoff_secs = (restart_backoff_secs.saturating_mul(2)).min(300);
                update_health(&health, |g| {
                    g.loop_running = true;
                    g.healthy = false;
                });
            }
        }
    }
}
