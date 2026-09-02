use std::fs;
use std::io::{stdout, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::json;

use crate::agent::{
    analysis_report, compute_stats, load_agent_state, load_sim_agent_state, load_state, log_path,
    pid_path, reset_sim, run_agent_loop, save_state, sim_journal_path, sim_state_path,
    spawn_background, state_summary, stop_daemon,
};
use crate::cli::{AgentBacktestCommands, AgentCommands, AgentSimCommands};
use crate::config::RuntimeConfig;
use crate::flatten::{build_agent_options_flatten_plan, execute_agent_options_flatten};
use crate::output::{OutputFormat, ResponseEnvelope};
use crate::rules::{rules_json_schema, RulesConfig};
use crate::ui::context::DashboardContext;
use crate::ui::dashboard::render_dashboard;
use crate::ui::discover::resolve_rules_file;

use crate::agent::backtest::{
    build_backtest_report, prefetch_daily_bars, run_backtest, BacktestRunOptions,
};
use crate::agent::backtest::prefetch::{default_from_to, parse_ymd};
use crate::agent::paths::backtest_cache_path;

pub async fn run(runtime: &RuntimeConfig, command: AgentCommands) -> Result<()> {
    match command {
        AgentCommands::Schema => {
            runtime.emit(ResponseEnvelope::ok("agent schema", rules_json_schema()));
        }
        AgentCommands::Validate { file } => {
            let rules = RulesConfig::load(&file)?;
            runtime.emit(ResponseEnvelope::ok(
                "agent validate",
                json!({
                    "valid": true,
                    "agent_id": rules.agent_id,
                    "accounts": rules.accounts.len(),
                    "watchlist": rules.watchlist_items(),
                    "llm_enabled": rules.llm.enabled,
                    "telegram_enabled": rules.notify.telegram.enabled,
                    "simulation": rules.simulation,
                }),
            ));
        }
        AgentCommands::Status { rules_file } => {
            if runtime.output == OutputFormat::Json {
                if let Some(rules_path) = rules_file {
                    let ctx = DashboardContext::load_mode(&rules_path, runtime.simulate)?;
                    runtime.emit(ResponseEnvelope::ok("agent status", ctx.to_json()));
                } else {
                    emit_legacy_status(runtime)?;
                }
            } else if let Some(rules_path) = rules_file {
                let ctx = DashboardContext::load_mode(&rules_path, runtime.simulate)?;
                print!("{}", render_dashboard(&ctx));
                stdout().flush().ok();
            } else if let Ok(rules_path) = resolve_rules_file(None, runtime.is_interactive()) {
                let ctx = DashboardContext::load_mode(&rules_path, runtime.simulate)?;
                print!("{}", render_dashboard(&ctx));
                stdout().flush().ok();
            } else {
                emit_legacy_status(runtime)?;
            }
        }
        AgentCommands::Run {
            file,
            once,
            background,
        } => {
            if background {
                if once {
                    anyhow::bail!("--background cannot be combined with --once");
                }
                let mut extra = Vec::new();
                if runtime.dry_run {
                    extra.push("--dry-run".into());
                }
                if runtime.simulate {
                    extra.push("--simulate".into());
                }
                if runtime.trust {
                    extra.push("--trust".into());
                }
                if runtime.yes {
                    extra.push("--yes".into());
                }
                if runtime.output == OutputFormat::Json {
                    extra.push("--json".into());
                }
                let pid = spawn_background(&file, &extra)?;
                runtime.emit(ResponseEnvelope::ok(
                    "agent background",
                    json!({
                        "pid": pid,
                        "pid_file": pid_path(&file),
                        "log_file": log_path(&file),
                        "rules": file,
                        "simulate": runtime.simulate,
                    }),
                ));
                return Ok(());
            }
            run_agent_loop(runtime, &file, once, None).await?;
        }
        AgentCommands::Stop { file } => {
            stop_daemon(&file)?;
            runtime.emit(ResponseEnvelope::ok(
                "agent stop",
                json!({ "stopped": true, "rules": file }),
            ));
        }
        AgentCommands::CloseAll { file } => {
            let rules = RulesConfig::load(&file)?;
            let state = if runtime.simulate {
                load_sim_agent_state(&file, &rules.agent_id)
            } else {
                load_agent_state(&file, &rules.agent_id)
            };
            let api = runtime.build_api()?;
            let plan = build_agent_options_flatten_plan(&api, &rules, &state).await?;
            let data = execute_agent_options_flatten(runtime, &api, &file, &rules, &plan).await?;
            runtime.emit(ResponseEnvelope::ok("agent close-all", data));
        }
        AgentCommands::Sim { command } => run_sim(runtime, command).await?,
        AgentCommands::Backtest { command } => run_backtest_cmd(runtime, command).await?,
        AgentCommands::Scorecard {
            rules_file,
            simulate,
        } => {
            let rules = RulesConfig::load(&rules_file)?;
            let mut state = if simulate {
                load_sim_agent_state(&rules_file, &rules.agent_id)
            } else {
                load_agent_state(&rules_file, &rules.agent_id)
            };
            let from_journal =
                crate::agent::scorecard::aggregate_from_journal(&rules_file, simulate)?;
            // Prefer journal as authoritative; keep state in sync for TUI.
            state.llm_scorecard = from_journal.clone();
            let _ = save_state(
                &if simulate {
                    sim_state_path(&rules_file)
                } else {
                    crate::agent::paths::default_state_path(&rules_file)
                },
                &state,
            );
            runtime.emit(ResponseEnvelope::ok(
                "agent scorecard",
                json!({
                    "agent_id": rules.agent_id,
                    "simulate": simulate,
                    "scorecard": from_journal.to_json(),
                    "suggestions_path": crate::agent::paths::suggestions_path(&rules_file),
                    "last_decision": state.last_llm_entry_decision,
                }),
            ));
        }
    }
    Ok(())
}

async fn run_backtest_cmd(runtime: &RuntimeConfig, command: AgentBacktestCommands) -> Result<()> {
    match command {
        AgentBacktestCommands::Prefetch {
            rules_file,
            from,
            to,
            force,
        } => {
            let rules = RulesConfig::load(&rules_file)?;
            let from_d = from.as_deref().map(parse_ymd).transpose()?;
            let to_d = to.as_deref().map(parse_ymd).transpose()?;
            let (from, to) = default_from_to(from_d, to_d)?;
            let market = runtime.build_market_api()?;
            let cache =
                prefetch_daily_bars(&market, &rules, &rules_file, from, to, force).await?;
            runtime.emit(ResponseEnvelope::ok(
                "agent backtest prefetch",
                json!({
                    "cache_path": backtest_cache_path(&rules_file),
                    "from": from.to_string(),
                    "to": to.to_string(),
                    "symbols": cache.symbols.keys().cloned().collect::<Vec<_>>(),
                    "bars": cache.symbols.iter().map(|(k,v)| json!({k: v.len()})).collect::<Vec<_>>(),
                }),
            ));
        }
        AgentBacktestCommands::Run {
            rules_file,
            from,
            to,
            fresh,
        } => {
            let rules = RulesConfig::load(&rules_file)?;
            let from_d = from.as_deref().map(parse_ymd).transpose()?;
            let to_d = to.as_deref().map(parse_ymd).transpose()?;
            let (from, to) = default_from_to(from_d, to_d)?;
            let result = run_backtest(
                &rules_file,
                &rules,
                BacktestRunOptions { from, to, fresh },
            )?;
            runtime.emit(ResponseEnvelope::ok("agent backtest run", result));
        }
        AgentBacktestCommands::Report { rules_file, output } => {
            let rules = RulesConfig::load(&rules_file)?;
            let report = build_backtest_report(&rules_file, &rules)?;
            if let Some(path) = output {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            }
            runtime.emit(ResponseEnvelope::ok("agent backtest report", report));
        }
    }
    Ok(())
}

async fn run_sim(runtime: &RuntimeConfig, command: AgentSimCommands) -> Result<()> {
    match command {
        AgentSimCommands::Stats { file } => {
            let rules = RulesConfig::load(&file)?;
            let mut state = load_sim_agent_state(&file, &rules.agent_id);
            state.agent_id = rules.agent_id.clone();
            let stats = compute_stats(&state, &rules);
            runtime.emit(ResponseEnvelope::ok(
                "agent sim stats",
                json!({
                    "rules": file,
                    "state_path": sim_state_path(&file),
                    "stats": stats,
                }),
            ));
        }
        AgentSimCommands::Report { file, output } => {
            let rules = RulesConfig::load(&file)?;
            let mut state = load_sim_agent_state(&file, &rules.agent_id);
            state.agent_id = rules.agent_id.clone();
            let report = analysis_report(&state, &rules);
            if let Some(path) = output {
                let text = serde_json::to_string_pretty(&report)?;
                fs::write(&path, text).with_context(|| format!("write report {}", path.display()))?;
                runtime.emit(ResponseEnvelope::ok(
                    "agent sim report",
                    json!({ "rules": file, "output": path }),
                ));
            } else {
                runtime.emit(ResponseEnvelope::ok("agent sim report", report));
            }
        }
        AgentSimCommands::Reset { file } => {
            if !runtime.yes {
                anyhow::bail!("sim reset requires --yes (live agent-state is not modified)");
            }
            let rules = RulesConfig::load(&file)?;
            let path = sim_state_path(&file);
            let mut state = load_sim_agent_state(&file, &rules.agent_id);
            state.agent_id = rules.agent_id.clone();
            reset_sim(&mut state, &rules);
            save_state(&path, &state)?;
            let journal = sim_journal_path(&file);
            if journal.exists() {
                fs::remove_file(&journal)
                    .with_context(|| format!("remove journal {}", journal.display()))?;
            }
            runtime.emit(ResponseEnvelope::ok(
                "agent sim reset",
                json!({
                    "rules": file,
                    "state_path": path,
                    "starting_budget_usd": state
                        .sim
                        .as_ref()
                        .map(|s| s.starting_budget_usd),
                }),
            ));
        }
    }
    Ok(())
}

fn emit_legacy_status(runtime: &RuntimeConfig) -> Result<()> {
    let path = directories::ProjectDirs::from("com", "schwabinvestbot", "schwab")
        .map(|d| d.data_local_dir().join("agent-state.json"))
        .unwrap_or_else(|| PathBuf::from("agent-state.json"));
    let state = load_state(&path)?;
    runtime.emit(ResponseEnvelope::ok(
        "agent status",
        json!({ "state_path": path, "state": state_summary(&state) }),
    ));
    Ok(())
}
