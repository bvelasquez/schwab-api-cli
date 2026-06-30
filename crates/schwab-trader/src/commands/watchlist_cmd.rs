use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::agent::state::{load_state, TraderState};
use crate::cli::WatchlistCommands;
use crate::config::TraderRuntime;
use crate::market_ctx::MarketCtx;
use crate::rules::TraderRules;
use crate::watchlist::{
    build_watchlist, load_pool_file, validate_pool_quotes, write_rules_watchlists, BuildOptions,
    WriteTarget,
};
use crate::watchlist::cron::{
    build_watchlist_cron_plan, plan_to_json, write_refresh_script,
};

pub async fn run(runtime: &TraderRuntime, command: WatchlistCommands) -> Result<()> {
    match command {
        WatchlistCommands::Show { rules_file } => run_show(runtime, &rules_file).await,
        WatchlistCommands::Build {
            rules_file,
            write,
            target,
            top_n,
            min_score,
        } => run_build(runtime, &rules_file, write, &target, top_n, min_score).await,
        WatchlistCommands::Pool { command } => match command {
            WatchlistPoolCommands::Validate { pool } => run_pool_validate(runtime, &pool).await,
        },
        WatchlistCommands::Cron {
            rules_file,
            schedule,
            install,
        } => run_cron(runtime, &rules_file, schedule.as_deref(), install),
    }
}

use crate::cli::WatchlistPoolCommands;

async fn run_show(runtime: &TraderRuntime, rules_path: &Path) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let state = load_state(rules_path, &rules.trader_id).unwrap_or_else(|_| TraderState {
        trader_id: rules.trader_id.clone(),
        ..TraderState::default()
    });
    let pool = rules.candidate_pool_symbols(rules_path).unwrap_or_default();
    let screen = rules.symbols_for_screening(rules_path).unwrap_or_default();
    let tradable = rules.all_watchlist_symbols();

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "trader watchlist show",
            json!({
                "rules_file": rules_path,
                "core": rules.watchlists.core,
                "thematic": rules.watchlists.thematic,
                "tradable_count": tradable.len(),
                "tradable": tradable,
                "candidate_pool_count": pool.len(),
                "candidate_pool_file": rules.watchlists.candidate_pool_file,
                "screening_eligible_count": screen.len(),
                "dynamic_enabled": rules.watchlists.dynamic,
                "dynamic_watchlist": state.dynamic_watchlist,
                "screened": rules.watchlists.screened,
            }),
        )
        .with_inputs(json!({ "rules_file": rules_path })),
    );
    Ok(())
}

async fn run_build(
    runtime: &TraderRuntime,
    rules_path: &Path,
    write: bool,
    target: &str,
    top_n: Option<u32>,
    min_score: Option<f64>,
) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let market_api = runtime.build_market_api()?;
    let market = MarketCtx::for_rules(market_api, rules_path, &rules);

    let result = build_watchlist(
        &market,
        &rules,
        rules_path,
        &BuildOptions { top_n, min_score },
    )
    .await?;

    let write_target = WriteTarget::parse(target)?;
    let mut written = false;
    if write {
        write_rules_watchlists(
            rules_path,
            &result.proposed_thematic,
            &result.proposed_core_append,
            write_target,
        )?;
        written = true;
    }

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "trader watchlist build",
            json!({
                "rules_file": rules_path,
                "pool_size": result.pool_size,
                "qualified_count": result.qualified.len(),
                "rejected_count": result.rejected.len(),
                "qualified": result.qualified,
                "rejected": result.rejected,
                "proposed_thematic": result.proposed_thematic,
                "proposed_core_append": result.proposed_core_append,
                "write": write,
                "write_target": target,
                "written": written,
            }),
        )
        .with_inputs(json!({
            "rules_file": rules_path,
            "top_n": top_n,
            "min_score": min_score,
        })),
    );
    Ok(())
}

async fn run_pool_validate(runtime: &TraderRuntime, pool_path: &Path) -> Result<()> {
    let pool = load_pool_file(pool_path)?;
    let api = runtime.build_market_api()?;
    let data = validate_pool_quotes(&api, &pool.symbols).await?;

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok("trader watchlist pool validate", data)
            .with_inputs(json!({
                "pool": pool_path,
                "label": pool.label,
            })),
    );
    Ok(())
}

fn run_cron(
    runtime: &TraderRuntime,
    rules_path: &Path,
    schedule: Option<&str>,
    install: bool,
) -> Result<()> {
    let plan = build_watchlist_cron_plan(rules_path, schedule, None)?;
    write_refresh_script(&plan)?;
    let mut data = plan_to_json(&plan);
    data["installed"] = json!(false);

    if install {
        install_crontab_line(&plan.crontab_line)?;
        data["installed"] = json!(true);
    }

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok("trader watchlist cron", data)
            .with_inputs(json!({ "rules_file": rules_path })),
    );
    Ok(())
}

fn install_crontab_line(line: &str) -> Result<()> {
    use std::process::Command;
    let existing = Command::new("crontab").arg("-l").output();
    let current = match existing {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    };
    if current.lines().any(|l| l.trim() == line.trim()) {
        return Ok(());
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(line);
    next.push('\n');
    let mut child = Command::new("crontab").arg("-").spawn()?;
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), next.as_bytes())?;
    let status = child.wait()?;
    anyhow::ensure!(status.success(), "crontab install failed");
    Ok(())
}
