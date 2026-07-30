use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use crate::agent::state::{load_state, TraderState};
use crate::cli::WatchlistCommands;
use crate::config::TraderRuntime;
use crate::market_ctx::MarketCtx;
use crate::rules::TraderRules;
use crate::fmp::{write_discovered_pool, DiscoverOptions, FmpClient};
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
        WatchlistCommands::Discover {
            rules_file,
            limit,
            write_pool,
            pool_out,
            set_candidate_pool,
            movers_only,
        } => {
            run_discover(
                runtime,
                &rules_file,
                limit,
                write_pool,
                pool_out.as_deref(),
                set_candidate_pool,
                movers_only,
            )
            .await
        }
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

async fn run_discover(
    runtime: &TraderRuntime,
    rules_path: &Path,
    limit: Option<usize>,
    write_pool: bool,
    pool_out: Option<&Path>,
    set_candidate_pool: bool,
    movers_only: bool,
) -> Result<()> {
    let rules = TraderRules::load(rules_path)?;
    let client = FmpClient::from_env()?;
    let opts = DiscoverOptions {
        min_price: rules.playbook.entry.min_price_usd,
        limit: limit.unwrap_or(40),
        prefer_screener: !movers_only,
        ..DiscoverOptions::default()
    };
    let result = client.discover(&opts).await?;

    let default_out = rules_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("universe")
        .join("fmp-discovered.yaml");
    let out_path = pool_out
        .map(Path::to_path_buf)
        .unwrap_or(default_out);

    let mut written_pool = false;
    if write_pool {
        write_discovered_pool(&out_path, &result.symbols, "fmp-discovered")?;
        written_pool = true;
    }

    let mut updated_rules = false;
    if set_candidate_pool {
        if !write_pool {
            write_discovered_pool(&out_path, &result.symbols, "fmp-discovered")?;
            written_pool = true;
        }
        let rel = relative_pool_path(rules_path, &out_path);
        patch_candidate_pool_file(rules_path, &rel)?;
        updated_rules = true;
    }

    runtime.emit(
        schwab_cli::output::ResponseEnvelope::ok(
            "trader watchlist discover",
            json!({
                "mode": result.mode,
                "symbol_count": result.symbols.len(),
                "symbols": result.symbols,
                "candidates": result.candidates,
                "warnings": result.warnings,
                "pool_out": out_path,
                "written_pool": written_pool,
                "updated_rules_candidate_pool_file": updated_rules,
                "next": "schwab-trader watchlist build --rules-file <rules> --write --json",
            }),
        )
        .with_inputs(json!({
            "rules_file": rules_path,
            "limit": opts.limit,
            "movers_only": movers_only,
        })),
    );
    Ok(())
}

fn relative_pool_path(rules_path: &Path, pool_path: &Path) -> String {
    let rules_dir = rules_path.parent().unwrap_or_else(|| Path::new("."));
    if let Ok(rel) = pool_path.strip_prefix(rules_dir) {
        return rel.to_string_lossy().replace('\\', "/");
    }
    pool_path.to_string_lossy().replace('\\', "/")
}

fn patch_candidate_pool_file(rules_path: &Path, relative_pool: &str) -> Result<()> {
    let raw = std::fs::read_to_string(rules_path)
        .with_context(|| format!("read {}", rules_path.display()))?;
    let updated = if raw.contains("candidate_pool_file:") {
        let mut out = String::new();
        for line in raw.lines() {
            if line.trim_start().starts_with("candidate_pool_file:") {
                let indent = &line[..line.len() - line.trim_start().len()];
                out.push_str(&format!("{indent}candidate_pool_file: {relative_pool}\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    } else {
        // Insert under watchlists: if present.
        let mut out = String::new();
        let mut inserted = false;
        for line in raw.lines() {
            out.push_str(line);
            out.push('\n');
            if !inserted && line.trim() == "watchlists:" {
                out.push_str(&format!("  candidate_pool_file: {relative_pool}\n"));
                inserted = true;
            }
        }
        if !inserted {
            anyhow::bail!("could not find watchlists: to set candidate_pool_file");
        }
        out
    };
    std::fs::write(rules_path, updated)
        .with_context(|| format!("write {}", rules_path.display()))?;
    Ok(())
}
