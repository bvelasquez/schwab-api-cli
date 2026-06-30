use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use serde_json::json;

use crate::backtest::cache::BacktestCache;
use crate::backtest::cron::{build_prefetch_cron_plan, install_prefetch_cron, write_prefetch_script};
use crate::backtest::prefetch::prefetch_daily_bars;
use crate::backtest::report::build_backtest_analysis_report;
use crate::backtest::runner::{run_backtest, BacktestRunOptions, EntryFillMode};
use crate::cli::BacktestCommands;
use crate::config::TraderRuntime;
use crate::rules::TraderRules;

pub async fn run(runtime: &TraderRuntime, command: BacktestCommands) -> Result<()> {
    match command {
        BacktestCommands::Prefetch {
            rules_file,
            from,
            to,
            force,
        } => run_prefetch(runtime, &rules_file, from, to, force).await,
        BacktestCommands::Run {
            rules_file,
            from,
            to,
            fresh,
            learn,
            no_learn,
            fill_at,
        } => {
            let rules = TraderRules::load(&rules_file)?;
            let (from, to) = resolve_date_range(from, to, &rules)?;
            let learn_enabled = if no_learn {
                false
            } else {
                learn
            };
            run_backtest(
                runtime,
                &rules_file,
                BacktestRunOptions {
                    from,
                    to,
                    fresh,
                    learn: learn_enabled,
                    fill_at: EntryFillMode::parse(&fill_at)?,
                },
            )
            .await
        }
        BacktestCommands::Report {
            rules_file,
            from,
            to,
            output,
        } => run_report(runtime, &rules_file, from, to, output.as_deref()).await,
        BacktestCommands::Cron {
            rules_file,
            schedule,
            write_script,
            install,
        } => run_cron(runtime, &rules_file, schedule.as_deref(), write_script, install).await,
    }
}

async fn run_prefetch(
    runtime: &TraderRuntime,
    rules_path: &Path,
    from: Option<String>,
    to: Option<String>,
    force: bool,
) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let (from, to) = resolve_date_range(from, to, &rules)?;
    let market = runtime.build_market_api()?;
    let cache = prefetch_daily_bars(&market, &rules, rules_path, from, to, force).await?;

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "backtest prefetch complete",
            json!({
                "from": cache.from.to_string(),
                "to": cache.to.to_string(),
                "symbols": cache.symbols.len(),
                "fetched_at": cache.fetched_at.to_rfc3339(),
                "cache_path": crate::agent::paths::backtest_cache_path(rules_path),
            }),
        )
        .with_inputs(json!({
            "rules_file": rules_path,
            "from": from.to_string(),
            "to": to.to_string(),
            "force": force,
        })),
    );
    Ok(())
}

async fn run_report(
    runtime: &TraderRuntime,
    rules_path: &Path,
    from: Option<String>,
    to: Option<String>,
    output: Option<&Path>,
) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let (from_date, to_date) = if from.is_some() || to.is_some() {
        let (f, t) = resolve_date_range(from, to, &rules)?;
        (Some(f), Some(t))
    } else {
        (None, None)
    };
    let cache_path = crate::agent::paths::backtest_cache_path(rules_path);
    let cache = BacktestCache::load(&cache_path).ok();
    let report = build_backtest_analysis_report(
        rules_path,
        &rules,
        cache.as_ref(),
        from_date,
        to_date,
    )?;

    if let Some(path) = output {
        let pretty = serde_json::to_string_pretty(&report)?;
        fs::write(path, pretty).with_context(|| format!("write report {}", path.display()))?;
        runtime.emit(
            schwab_cli::output::ResponseEnvelope::ok(
                "backtest report",
                json!({
                    "written": path,
                    "ledger_stats": report.get("ledger_stats"),
                    "benchmark_comparison": report.get("benchmark_comparison"),
                    "exposure_adjusted_benchmark": report.get("exposure_adjusted_benchmark"),
                }),
            )
            .with_inputs(json!({ "rules_file": rules_path, "output": path })),
        );
    } else {
        runtime.emit(
            schwab_cli::output::ResponseEnvelope::ok("backtest report", report)
                .with_inputs(json!({ "rules_file": rules_path })),
        );
    }
    Ok(())
}

async fn run_cron(
    runtime: &TraderRuntime,
    rules_path: &Path,
    schedule: Option<&str>,
    write_script: bool,
    install: bool,
) -> Result<()> {
    let _ = TraderRules::load(rules_path)?;
    let plan = build_prefetch_cron_plan(rules_path, schedule, None)?;
    let mut result = json!({
        "schedule": plan.schedule,
        "crontab_line": plan.crontab_line,
        "script_path": plan.script_path,
        "log_path": plan.log_path,
        "rules_file": plan.rules_file,
        "hint": "Set CRON_TZ=America/New_York (or your TZ) so 06:15 runs before the US open.",
    });

    if install {
        let install_result = install_prefetch_cron(&plan)?;
        result["install"] = install_result;
    } else if write_script || !install {
        write_prefetch_script(&plan)?;
        result["script_written"] = json!(plan.script_path);
    }

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok("backtest cron prefetch", result)
            .with_inputs(json!({
                "rules_file": rules_path,
                "install": install,
                "write_script": write_script,
            })),
    );
    Ok(())
}

fn resolve_date_range(
    from: Option<String>,
    to: Option<String>,
    rules: &TraderRules,
) -> Result<(NaiveDate, NaiveDate)> {
    let to = match to {
        Some(s) => parse_date(&s)?,
        None => Utc::now().date_naive() - chrono::Duration::days(1),
    };
    let from = match from {
        Some(s) => parse_date(&s)?,
        None => to - chrono::Duration::days(730),
    };
    anyhow::ensure!(from <= to, "from date must be on or before to date");
    let _ = rules;
    Ok((from, to))
}

fn parse_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .with_context(|| format!("invalid date {s:?} (use YYYY-MM-DD)"))
}
