use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::time::sleep;

use crate::agent::llm::{candidate_approved, OpenRouterClient, TraderLlmReview};
use crate::agent::paths::state_path;
use crate::agent::schedule::{
    self, should_run_monitor_review, should_run_overnight_digest, should_run_premarket_digest,
    should_use_web_research, AgentSession,
};
use crate::agent::state::{load_state, save_state, TraderState};
use crate::capital::{capital_check_to_json, compute_capital_check};
use crate::closure::process_closure_exits;
use crate::commands::scan_cmd::run_scan_inner;
use crate::config::TraderRuntime;
use crate::entry::{attempt_entry, EntryStatus};
use crate::journal;
use crate::learn::{
    adaptation_allowed, apply_rule_patches, build_learn_context, should_run_learn,
};
use crate::reconcile::reconcile_tick;
use crate::risk::{monitoring_metrics, update_drawdown};
use crate::rules::TraderRules;
use crate::sim::{compute_stats, snapshot_equity};
use crate::sources::{attach_feeds_to_context, fetch_feeds_for_phase};

pub struct AgentRunOptions {
    pub once: bool,
}

struct TickOutcome {
    body: Value,
    next_sleep_seconds: u64,
    session: String,
}

pub async fn run_agent_loop(
    runtime: &TraderRuntime,
    rules_path: &Path,
    options: AgentRunOptions,
) -> Result<()> {
    let mut rules = TraderRules::load(rules_path)?;
    rules.log_validation_hints();
    let account = rules.primary_account()?.hash.clone();
    let api = runtime.build_api()?;
    let market = runtime.build_market_api()?;

    let mut state = load_state(rules_path, &rules.trader_id)?;

    let llm_client = if rules.llm.enabled {
        OpenRouterClient::from_env().ok()
    } else {
        None
    };

    loop {
        state.tick_count += 1;
        state.last_tick = Some(Utc::now());
        let outcome = run_tick(
            runtime,
            rules_path,
            &mut rules,
            &account,
            &api,
            &market,
            &mut state,
            llm_client.as_ref(),
        )
        .await?;
        state.last_tick_result = Some(outcome.body.clone());
        save_state(rules_path, &state)?;
        let log_line = format!(
            "tick={} session={} open={} trades_today={} style={} dry_run={} simulate={}",
            state.tick_count,
            outcome.session,
            state.open_positions.len(),
            state.trades_today,
            rules.playbook.style,
            runtime.dry_run,
            runtime.simulate
        );
        let _ = crate::agent::paths::append_trader_log(rules_path, &log_line);
        runtime.emit(schwab_cli::output::ResponseEnvelope::ok(
            "trader agent tick",
            json!({
                "tick": state.tick_count,
                "session": outcome.session,
                "next_sleep_seconds": outcome.next_sleep_seconds,
                "summary": state.summary(),
                "tick_result": outcome.body,
            }),
        ));

        if options.once {
            break;
        }
        sleep(Duration::from_secs(outcome.next_sleep_seconds)).await;
    }

    Ok(())
}

async fn run_tick(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &mut TraderRules,
    account_hash: &str,
    api: &Arc<schwab_api::TraderApi>,
    market: &Arc<schwab_market_data::MarketDataApi>,
    state: &mut TraderState,
    llm_client: Option<&OpenRouterClient>,
) -> Result<TickOutcome> {
    state.reset_trades_day(&rules.schedule.timezone);

    let transition =
        schedule::resolve_session(rules, state.last_session.as_deref());
    let session = transition.session;
    state.last_session = Some(session.as_str().to_string());

    let outcome = match session {
        AgentSession::Idle => {
            tick_idle(runtime, rules_path, rules, state, api, account_hash, &transition).await?
        }
        AgentSession::Overnight => {
            tick_overnight(
                runtime,
                rules_path,
                rules,
                state,
                api,
                account_hash,
                market,
                llm_client,
                &transition,
            )
            .await?
        }
        AgentSession::Premarket => {
            tick_premarket(
                runtime,
                rules_path,
                rules,
                state,
                api,
                account_hash,
                market,
                llm_client,
                &transition,
            )
            .await?
        }
        AgentSession::RegularHours => {
            tick_regular(
                runtime,
                rules_path,
                rules,
                state,
                api,
                account_hash,
                market,
                llm_client,
                &transition,
            )
            .await?
        }
    };

    Ok(TickOutcome {
        body: outcome,
        next_sleep_seconds: transition.sleep_seconds,
        session: session.as_str().to_string(),
    })
}

async fn tick_idle(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<schwab_api::TraderApi>,
    account_hash: &str,
    transition: &schedule::SessionTransition,
) -> Result<Value> {
    let reconcile_report = reconcile_tick(
        runtime, rules_path, rules, state, api, account_hash,
    )
    .await?;

    Ok(json!({
        "session": "idle",
        "at_open": false,
        "market_clock": crate::market_session::market_clock_json(rules),
        "next_sleep_seconds": transition.sleep_seconds,
        "skipped": ["market closed — no LLM, no scan, no entries"],
        "reconcile_report": reconcile_report,
        "monitoring": monitoring_metrics(state, rules),
    }))
}

async fn tick_overnight(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<schwab_api::TraderApi>,
    account_hash: &str,
    market: &Arc<schwab_market_data::MarketDataApi>,
    llm_client: Option<&OpenRouterClient>,
    transition: &schedule::SessionTransition,
) -> Result<Value> {
    let mut skipped = Vec::<String>::new();
    let reconcile_report = reconcile_tick(
        runtime, rules_path, rules, state, api, account_hash,
    )
    .await?;

    if rules.is_intraday() && !state.open_positions.is_empty() {
        let _ = process_closure_exits(
            runtime, rules_path, rules, state, api, market, account_hash,
        )
        .await?;
        skipped.push("intraday overnight flatten check".into());
    } else if state.open_positions.is_empty() {
        skipped.push("overnight — flat, no positions".into());
    } else {
        skipped.push(format!(
            "overnight — {} open position(s) (reconcile only)",
            state.open_positions.len()
        ));
    }

    let mut llm_summary = None;
    let digest_due =
        should_run_overnight_digest(state, &rules.schedule.overnight, Utc::now());

    if !digest_due {
        skipped.push(format!(
            "overnight digest next in ~{} min",
            rules.schedule.overnight.tick_interval_seconds / 60
        ));
    } else if let Some(client) = llm_client {
        let context = llm_context_with_feeds(
            rules,
            "overnight_digest",
            json!({
                "phase": "overnight_digest",
                "playbook_style": rules.playbook.style,
                "open_positions": state.open_positions,
                "open_playbook": state.open_playbook,
                "entries_blocked": "market closed",
            }),
        )
        .await;
        match client
            .review(
                &rules.llm,
                "overnight_digest",
                &rules.llm.web_model,
                &context,
                true,
            )
            .await
        {
            Ok(review) => {
                state.last_overnight_digest_at = Some(Utc::now());
                state.open_playbook = Some(json!({
                    "market_commentary": review.market_commentary,
                    "web_insights": review.web_insights,
                    "candidates": review.candidates,
                    "at": Utc::now(),
                }));
                llm_summary = Some(serde_json::to_value(&review)?);
                state.last_llm_summary = llm_summary.clone();
            }
            Err(err) => {
                skipped.push(format!("overnight digest failed: {err}"));
            }
        }
    } else if rules.llm.enabled {
        skipped.push("overnight digest skipped (no LLM client)".into());
    }

    Ok(json!({
        "session": "overnight",
        "at_open": false,
        "market_clock": crate::market_session::market_clock_json(rules),
        "next_sleep_seconds": transition.sleep_seconds,
        "skipped": skipped,
        "reconcile_report": reconcile_report,
        "llm": llm_summary,
        "monitoring": monitoring_metrics(state, rules),
    }))
}

async fn tick_premarket(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<schwab_api::TraderApi>,
    account_hash: &str,
    market: &Arc<schwab_market_data::MarketDataApi>,
    llm_client: Option<&OpenRouterClient>,
    transition: &schedule::SessionTransition,
) -> Result<Value> {
    let mut skipped = Vec::<String>::new();
    let reconcile_report = reconcile_tick(
        runtime, rules_path, rules, state, api, account_hash,
    )
    .await?;

    let scan = run_scan_inner(market, rules, state).await?;
    skipped.push("premarket — no entries until regular session".into());

    let mut llm_summary = None;
    let digest_due = should_run_premarket_digest(state, &rules.schedule, Utc::now());
    let mins_to_open =
        crate::market_session::minutes_until_equity_open(Utc::now(), &rules.schedule.timezone);

    if !digest_due {
        skipped.push(format!(
            "premarket digest next soon ({} min to open)",
            mins_to_open
        ));
    } else if let Some(client) = llm_client {
        let context = llm_context_with_feeds(
            rules,
            "premarket_digest",
            json!({
                "phase": "premarket_digest",
                "minutes_to_open": mins_to_open,
                "playbook_style": rules.playbook.style,
                "scan": scan,
                "open_positions": state.open_positions,
                "open_playbook": state.open_playbook,
                "entries_blocked": crate::closure::entry_block_reason(rules),
            }),
        )
        .await;
        match client
            .review(
                &rules.llm,
                "premarket_digest",
                &rules.llm.web_model,
                &context,
                true,
            )
            .await
        {
            Ok(review) => {
                state.last_premarket_digest_at = Some(Utc::now());
                apply_web_picks(state, rules, &review);
                state.open_playbook = Some(json!({
                    "market_commentary": review.market_commentary,
                    "web_insights": review.web_insights,
                    "candidates": review.candidates,
                    "at": Utc::now(),
                    "minutes_to_open": mins_to_open,
                }));
                llm_summary = Some(serde_json::to_value(&review)?);
                state.last_llm_summary = llm_summary.clone();
            }
            Err(err) => {
                skipped.push(format!("premarket digest failed: {err}"));
            }
        }
    } else if rules.llm.enabled {
        skipped.push("premarket digest skipped (no LLM client)".into());
    }

    Ok(json!({
        "session": "premarket",
        "at_open": false,
        "minutes_to_open": mins_to_open,
        "market_clock": crate::market_session::market_clock_json(rules),
        "next_sleep_seconds": transition.sleep_seconds,
        "skipped": skipped,
        "reconcile_report": reconcile_report,
        "scan": scan,
        "llm": llm_summary,
        "monitoring": monitoring_metrics(state, rules),
    }))
}

async fn tick_regular(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &mut TraderRules,
    state: &mut TraderState,
    api: &Arc<schwab_api::TraderApi>,
    account_hash: &str,
    market: &Arc<schwab_market_data::MarketDataApi>,
    llm_client: Option<&OpenRouterClient>,
    transition: &schedule::SessionTransition,
) -> Result<Value> {
    let at_open = transition.just_opened;
    state.regular_tick_count += 1;

    let reconcile_report = reconcile_tick(
        runtime, rules_path, rules, state, api, account_hash,
    )
    .await?;

    let drawdown = update_drawdown(state, rules);

    let closure_exits = process_closure_exits(
        runtime, rules_path, rules, state, api, market, account_hash,
    )
    .await?;
    if runtime.simulate {
        snapshot_equity(state, rules);
    }

    let scan = run_scan_inner(market, rules, state).await?;
    let capital = compute_capital_check(
        api,
        rules,
        state,
        account_hash,
        None,
        None,
        runtime.simulate,
        Some(rules_path),
    )
    .await?;

    let has_candidates = scan
        .get("candidates")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let has_positions = !state.open_positions.is_empty();

    let mut llm_review: Option<TraderLlmReview> = None;
    let mut llm_summary = None;

    if let Some(client) = llm_client {
        if let Some((phase, model, use_web)) =
            resolve_regular_llm_phase(rules, state, has_candidates, has_positions)
        {
            let context = llm_context_with_feeds(
                rules,
                phase,
                json!({
                    "phase": phase,
                    "regular_tick": state.regular_tick_count,
                    "at_open": at_open,
                    "playbook_style": rules.playbook.style,
                    "adaptable_playbook": crate::learn::adaptable_playbook_snapshot(rules),
                    "open_playbook": state.open_playbook,
                    "capital_check": capital_check_to_json(&capital),
                    "scan": scan,
                    "open_positions": state.open_positions,
                    "sim_stats": compute_stats(state),
                    "closure_exits_this_tick": closure_exits,
                    "entries_blocked": crate::closure::entry_block_reason(rules),
                }),
            )
            .await;
            match client.review(&rules.llm, phase, model, &context, use_web).await {
                Ok(review) => {
                    apply_web_picks(state, rules, &review);
                    llm_summary = Some(serde_json::to_value(&review)?);
                    state.last_llm_summary = llm_summary.clone();
                    state.last_llm_review_tick = Some(state.regular_tick_count);
                    state.llm_review_count += 1;
                    llm_review = Some(review);
                }
                Err(err) => {
                    llm_summary = Some(json!({ "error": err.to_string() }));
                }
            }
        }
    }

    let mut entry_attempts = Vec::new();
    if capital.passed && state.entry_block_reason(rules).is_none() {
        if let Some(candidates) = scan.get("candidates").and_then(|v| v.as_array()) {
            for candidate in candidates {
                let symbol = candidate
                    .get("symbol")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if symbol.is_empty() {
                    continue;
                }

                let llm_ok = match &llm_review {
                    Some(review) => candidate_approved(review, symbol, rules.llm.veto_entries),
                    None if rules.llm.enabled && !runtime.dry_run && !runtime.simulate => false,
                    None if rules.llm.enabled && runtime.dry_run => true,
                    None => true,
                };
                if !llm_ok {
                    entry_attempts.push(json!({
                        "status": "skipped",
                        "symbol": symbol,
                        "reason": "llm_veto_or_missing_review",
                    }));
                    continue;
                }

                let attempt = attempt_entry(
                    runtime,
                    rules_path,
                    rules,
                    state,
                    api,
                    market,
                    account_hash,
                    symbol,
                    None,
                    None,
                    true,
                    "agent",
                )
                .await?;

                let status = match attempt.status {
                    EntryStatus::DryRun => "dry_run",
                    EntryStatus::Simulated => "simulated",
                    EntryStatus::Filled => "filled",
                    EntryStatus::Submitted => "submitted",
                    EntryStatus::Skipped => "skipped",
                };
                let done = matches!(
                    attempt.status,
                    EntryStatus::DryRun
                        | EntryStatus::Simulated
                        | EntryStatus::Filled
                        | EntryStatus::Submitted
                );
                entry_attempts.push(json!({
                    "status": status,
                    "attempt": attempt,
                }));

                if done {
                    break;
                }
            }
        }
    }

    let mut learn_result = None;
    if rules.llm.enabled && should_run_learn(rules, state) {
        if let Some(client) = llm_client {
            let learn_ctx = build_learn_context(rules, state, rules_path)?;
            let learn_ctx = llm_context_with_feeds(rules, "learn", learn_ctx).await;
            match client
                .review(
                    &rules.llm,
                    "learn",
                    &rules.llm.learn_model,
                    &learn_ctx,
                    false,
                )
                .await
            {
                Ok(review) => {
                    let mut applied = Vec::new();
                    if !review.rule_patches.is_empty() {
                        if adaptation_allowed(runtime.dry_run, runtime.simulate, rules) {
                            match apply_rule_patches(rules, &review.rule_patches) {
                                Ok(p) => {
                                    applied = p;
                                    rules.save(rules_path)?;
                                }
                                Err(err) => {
                                    learn_result = Some(json!({
                                        "error": err.to_string(),
                                        "patches_rejected": review.rule_patches,
                                    }));
                                }
                            }
                        }
                        let _ = journal::append_event(
                            rules_path,
                            if applied.is_empty() {
                                "rule_patch_proposed"
                            } else {
                                "rule_auto_applied"
                            },
                            json!({
                                "patches": review.rule_patches,
                                "applied": applied,
                                "dry_run": runtime.dry_run,
                            }),
                        );
                        if !applied.is_empty() {
                            notify_rule_adaptation(rules, applied.len()).await;
                            state.closed_trades_since_learn = 0;
                            state.last_learn_tick = Some(state.tick_count);
                        }
                    }
                    if learn_result.is_none() {
                        learn_result = Some(json!({
                            "review": review,
                            "applied": applied,
                        }));
                    }
                    state.last_learn_tick = Some(state.tick_count);
                }
                Err(err) => {
                    learn_result = Some(json!({ "error": err.to_string() }));
                }
            }
        }
    }

    Ok(json!({
        "session": "regular",
        "at_open": at_open,
        "regular_tick": state.regular_tick_count,
        "market_clock": crate::market_session::market_clock_json(rules),
        "next_sleep_seconds": transition.sleep_seconds,
        "reconcile_report": reconcile_report,
        "drawdown": drawdown,
        "monitoring": monitoring_metrics(state, rules),
        "scan": scan,
        "capital_check": capital_check_to_json(&capital),
        "llm": llm_summary,
        "learn": learn_result,
        "entry_attempts": entry_attempts,
        "closure_exits": closure_exits,
        "sim_stats": compute_stats(state),
        "dry_run": runtime.dry_run,
        "simulate": runtime.simulate,
        "playbook_style": rules.playbook.style,
        "state_path": state_path(rules_path),
    }))
}

async fn llm_context_with_feeds(rules: &TraderRules, phase: &str, context: Value) -> Value {
    let feeds = fetch_feeds_for_phase(rules, phase).await;
    attach_feeds_to_context(context, rules, phase, &feeds)
}

/// Returns (phase, model, use_web) when an LLM call is warranted during regular hours.
fn resolve_regular_llm_phase<'a>(
    rules: &'a TraderRules,
    state: &TraderState,
    has_candidates: bool,
    has_positions: bool,
) -> Option<(&'static str, &'a str, bool)> {
    if !rules.llm.enabled {
        return None;
    }
    if has_candidates {
        let use_web = should_use_web_research(
            state.llm_review_count,
            rules.llm.web_research_every_reviews,
        ) && rules.sources.web.enabled
            && !rules.is_intraday();
        let model = if use_web {
            &rules.llm.web_model
        } else {
            &rules.llm.selection_model
        };
        let phase = if use_web { "web" } else { "selection" };
        return Some((phase, model, use_web));
    }
    if has_positions
        && should_run_monitor_review(
            state.regular_tick_count,
            state.last_llm_review_tick,
            rules.llm.review_every_ticks,
        )
    {
        return Some(("monitor", &rules.llm.monitor_model, false));
    }
    None
}

fn apply_web_picks(state: &mut TraderState, rules: &TraderRules, review: &TraderLlmReview) {
    if !rules.watchlists.dynamic || !rules.sources.web.enabled {
        return;
    }
    state.reset_web_picks_day(&rules.schedule.timezone);
    let budget = rules.sources.web.pick_budget_per_day;
    let max_dynamic = rules.watchlists.max_dynamic_symbols as usize;

    for candidate in &review.candidates {
        if state.web_picks_today >= budget {
            break;
        }
        if !candidate.recommendation.eq_ignore_ascii_case("proceed") {
            continue;
        }
        let sym = candidate.symbol.trim().to_uppercase();
        if sym.is_empty()
            || rules.is_core_holding(&sym)
            || rules.is_blocked_symbol(&sym)
            || state.dynamic_watchlist.iter().any(|s| s == &sym)
        {
            continue;
        }
        if state.dynamic_watchlist.len() >= max_dynamic {
            break;
        }
        state.dynamic_watchlist.push(sym);
        state.web_picks_today += 1;
    }

    if !review.candidates.is_empty() {
        state.last_web_picks = Some(json!({
            "candidates": review.candidates,
            "web_insights": review.web_insights,
        }));
    }
}

async fn notify_rule_adaptation(rules: &TraderRules, patch_count: usize) {
    if !rules.notify.telegram.notify_on_rule_adaptation {
        return;
    }
    if std::env::var("TELEGRAM_BOT_TOKEN").is_err() || std::env::var("TELEGRAM_CHAT_ID").is_err()
    {
        return;
    }
    let url = format!(
        "https://api.telegram.org/bot{}/sendMessage",
        std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default()
    );
    let _ = reqwest::Client::new()
        .post(&url)
        .json(&json!({
            "chat_id": std::env::var("TELEGRAM_CHAT_ID").unwrap_or_default(),
            "text": format!(
                "schwab-trader [{}]: {} rule patch(es) applied",
                rules.trader_id, patch_count
            ),
        }))
        .send()
        .await;
}
