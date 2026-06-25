use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, Utc};
use schwab_api::TraderApi;
use schwab_market_data::endpoints::chains::ChainQuery;
use schwab_market_data::MarketDataApi;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::config::RuntimeConfig;
use crate::notify::TelegramNotifier;
use crate::options::{
    build_order_for_strategy, ensure_option_buying_power, group_option_legs,
    list_option_positions, parse_expiry, days_to_expiry, IronCondorParams, StrategyKind,
    VerticalParams,
};
use crate::options::positions::build_close_order_for_group;
use crate::order_status::{
    is_failure_status, wait_for_order, wait_result_json, WaitCondition, WaitOptions,
};
use crate::rules::{LlmPhase, RulesConfig};
use crate::safety::{execute_trading_order, require_trading_approval};

use super::exits::{
    evaluate_position_monitor, exit_signal_json, find_tracked_position, position_key,
    reconcile_open_positions,
};
use super::llm::OpenRouterClient;
use super::market_context::{market_context_summary_for_llm, vertical_entry_market_context};
use super::paths::{default_state_path, load_agent_state};
use super::schedule::{self, AgentSession};
use super::state::{save_state, AgentState, TrackedPosition};

#[derive(Debug, Clone, serde::Serialize)]
pub struct TickResult {
    pub session: String,
    pub at_open: bool,
    pub next_sleep_seconds: u64,
    pub signals: Vec<Value>,
    pub actions: Vec<Value>,
    pub skipped: Vec<String>,
    pub monitored_positions: Vec<Value>,
    pub llm_review: Option<Value>,
}

pub async fn run_agent_loop(
    runtime: &RuntimeConfig,
    rules_path: &std::path::Path,
    once: bool,
) -> Result<()> {
    let rules = RulesConfig::load(rules_path)?;
    let state_path = default_state_path(rules_path);
    let mut state = load_agent_state(rules_path, &rules.agent_id);
    state.agent_id = rules.agent_id.clone();

    if !runtime.dry_run {
        crate::safety::require_trading_approval(
            runtime,
            "agent run",
            &format!("Run options agent `{}`", rules.agent_id),
        )?;
    }

    let trader = runtime.build_api()?;
    let market = runtime.build_market_api()?;
    let telegram = TelegramNotifier::from_env(&rules.notify.telegram).ok().flatten();
    let llm_client = if rules.llm.enabled {
        Some(OpenRouterClient::from_env()?)
    } else {
        None
    };

    loop {
        let result = tick_once(
            runtime,
            &rules,
            &trader,
            &market,
            &mut state,
            llm_client.as_ref(),
            telegram.as_ref(),
        )
        .await?;
        state.last_tick = Some(Utc::now());
        save_state(&state_path, &state)?;

        runtime.emit(crate::output::ResponseEnvelope::ok(
            if once { "agent run once" } else { "agent tick" },
            json!({
                "agent_id": rules.agent_id,
                "session": result.session,
                "at_open": result.at_open,
                "next_sleep_seconds": result.next_sleep_seconds,
                "signals": result.signals,
                "actions": result.actions,
                "skipped": result.skipped,
                "monitored_positions": result.monitored_positions,
                "llm_review": result.llm_review,
                "dry_run": runtime.dry_run,
            }),
        ));

        notify_tick(telegram.as_ref(), &rules, &result, runtime.dry_run).await;

        if once {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(result.next_sleep_seconds))
            .await;
    }

    Ok(())
}

pub async fn tick_once(
    runtime: &RuntimeConfig,
    rules: &RulesConfig,
    trader: &Arc<TraderApi>,
    market: &Arc<MarketDataApi>,
    state: &mut AgentState,
    llm_client: Option<&OpenRouterClient>,
    telegram: Option<&TelegramNotifier>,
) -> Result<TickResult> {
    let today = Local::now().date_naive();
    state.reset_daily_if_needed(today);
    state.tick_count += 1;

    let mut result = TickResult {
        session: "unknown".into(),
        at_open: false,
        next_sleep_seconds: rules.schedule.tick_interval_seconds.max(5),
        signals: vec![],
        actions: vec![],
        skipped: vec![],
        monitored_positions: vec![],
        llm_review: None,
    };

    reconcile_open_positions(trader, state, rules).await?;

    let market_open = market_is_open(market).await?;
    let transition = schedule::resolve_session(
        market_open,
        &rules.schedule,
        state.last_session.as_deref(),
    );
    result.session = transition.session.as_str().to_string();
    result.next_sleep_seconds = transition.sleep_seconds;
    state.last_session = Some(result.session.clone());

    match transition.session {
        AgentSession::Idle => {
            result
                .skipped
                .push("market closed (option hours)".into());
            return Ok(result);
        }
        AgentSession::Overnight => {
            return tick_overnight(
                runtime,
                rules,
                state,
                llm_client,
                telegram,
                &mut result,
            )
            .await;
        }
        AgentSession::RegularHours => {
            if transition.just_opened {
                result.at_open = true;
                result
                    .skipped
                    .push("market open — full evaluation (mechanical exits + live marks)".into());
                notify_at_open(telegram, state.open_playbook.as_ref()).await;
            }
            state.regular_tick_count += 1;
        }
    }

    let entries_paused = state.trades_today >= rules.risk.max_trades_per_day;

    // Exit evaluation and position monitoring (always runs when market is open)
    for account in rules.enabled_accounts() {
        let legs = list_option_positions(trader, Some(&account.hash)).await?;
        let groups = group_option_legs(&legs);
        for group in &groups {
            let tracked = find_tracked_position(state, &account.hash, group);
            let monitor = evaluate_position_monitor(market, group, rules, today, tracked).await?;

            if !group.legs.is_empty() {
                result.monitored_positions.push(monitor.snapshot);
            }

            if let Some(eval) = monitor.exit {
                let exit = exit_signal_json(group, &eval);
                result.signals.push(exit.clone());
                if !runtime.dry_run {
                    if let Ok(action) = execute_exit(
                        runtime,
                        trader,
                        &account.hash,
                        rules,
                        group,
                        &exit,
                        state,
                    )
                    .await
                    {
                        result.actions.push(action);
                        notify_action(telegram, "EXIT", &exit).await;
                    }
                }
            }
        }
    }

    // Entry scan (signals collected; execution after LLM review)
    let mut pending_entries: Vec<(String, StrategyKind, Value)> = Vec::new();
    if entries_paused {
        result.skipped.push(format!(
            "new entries paused — max_trades_per_day ({}) reached",
            rules.risk.max_trades_per_day
        ));
    } else {
        for account in rules.enabled_accounts() {
            for underlying in &rules.watchlist {
                let sym = underlying.to_uppercase();
                if !rules.risk.allowed_underlyings.is_empty()
                    && !rules
                        .risk
                        .allowed_underlyings
                        .iter()
                        .any(|u| u.eq_ignore_ascii_case(&sym))
                {
                    continue;
                }

                if rules.strategies.vertical.enabled {
                    match evaluate_vertical_entry(
                        market,
                        rules,
                        &sym,
                        today,
                        state,
                        &account.hash,
                    )
                    .await
                    {
                        Ok(Some(signal)) => {
                            pending_entries.push((
                                account.hash.clone(),
                                StrategyKind::Vertical,
                                signal,
                            ));
                        }
                        Ok(None) => {}
                        Err(e) => result
                            .skipped
                            .push(format!("{sym} vertical: {e:#}")),
                    }
                }

                if rules.strategies.iron_condor.enabled {
                    match evaluate_condor_entry(
                        market,
                        rules,
                        &sym,
                        today,
                        state,
                        &account.hash,
                    )
                    .await
                    {
                        Ok(Some(signal)) => {
                            pending_entries.push((
                                account.hash.clone(),
                                StrategyKind::IronCondor,
                                signal,
                            ));
                        }
                        Ok(None) => {}
                        Err(e) => result
                            .skipped
                            .push(format!("{sym} iron_condor: {e:#}")),
                    }
                }
            }
        }
    }

    for (_, _, signal) in &pending_entries {
        result.signals.push(signal.clone());
    }

    // LLM review — selection when candidates exist; monitor on schedule when positions open
    let mut llm_veto_entries = false;
    let mut llm_close_ids: Vec<String> = Vec::new();
    let has_candidates = !pending_entries.is_empty();
    let has_positions = !result.monitored_positions.is_empty();

    if let Some(client) = llm_client {
        if let Some(phase) = resolve_llm_phase(rules, state, has_candidates, has_positions) {
            let use_web =
                should_use_web_research(rules, state) && matches!(phase, LlmPhase::Selection);

            let context = json!({
                "agent_id": rules.agent_id,
                "tick": state.regular_tick_count,
                "date": today.to_string(),
                "phase": match phase {
                    LlmPhase::Selection => "selection",
                    LlmPhase::Monitor => "monitor",
                    LlmPhase::OvernightDigest => "overnight_digest",
                },
                "market": market_context_summary_for_llm(),
                "exit_rules": super::exits::exit_rules_summary(&rules.exit_rules),
                "open_positions": result.monitored_positions,
                "open_playbook": state.open_playbook,
                "candidate_entries": pending_entries.iter().map(|(_, _, s)| s).collect::<Vec<_>>(),
                "recent_signals": result.signals,
                "watchlist": rules.watchlist,
                "risk": {
                    "max_trades_per_day": rules.risk.max_trades_per_day,
                    "trades_today": state.trades_today,
                    "max_risk_per_trade_usd": rules.risk.max_risk_per_trade_usd,
                    "max_portfolio_risk_usd": rules.risk.max_portfolio_risk_usd,
                },
            });

            match client.review(&rules.llm, phase, &context, use_web).await {
                Ok(review) => {
                    let review_json = review.to_json();
                    result.llm_review = Some(review_json.clone());
                    state.last_llm_review_tick = Some(state.regular_tick_count);
                    state.llm_review_count += 1;
                    state.last_llm_summary = Some(review_json.clone());
                    state.record_action("llm_review", review_json.clone());

                    if rules.llm.veto_entries && review.should_veto_entries() {
                        llm_veto_entries = true;
                        result.skipped.push(format!(
                            "LLM veto entries: {}",
                            review.entry_reasoning
                        ));
                    }

                    if rules.llm.allow_llm_exits {
                        for pos in review.urgent_close_positions() {
                            llm_close_ids.push(pos.position_id.clone());
                        }
                    }

                    if !review.risk_alerts.is_empty() || !review.market_commentary.is_empty() {
                        notify_llm(telegram, &review.market_commentary, &review.risk_alerts).await;
                    }
                }
                Err(e) => result.skipped.push(format!("LLM review failed: {e:#}")),
            }
        }
    }

    // LLM-requested exits (high urgency only, when enabled)
    if !llm_close_ids.is_empty() && !runtime.dry_run {
        for account in rules.enabled_accounts() {
            let legs = list_option_positions(trader, Some(&account.hash)).await?;
            let groups = group_option_legs(&legs);
            for group in &groups {
                if !llm_close_ids.iter().any(|id| id == &group.id) {
                    continue;
                }
                let exit = json!({
                    "type": "exit",
                    "reason": "llm_recommendation",
                    "position_id": group.id,
                    "underlying": group.underlying,
                    "expiry": group.expiry,
                });
                result.signals.push(exit.clone());
                if let Ok(action) =
                    execute_exit(runtime, trader, &account.hash, rules, group, &exit, state).await
                {
                    result.actions.push(action);
                    notify_action(telegram, "LLM EXIT", &exit).await;
                }
            }
        }
    }

    // Execute pending entries unless LLM vetoed (dry-run never executes)
    if !llm_veto_entries && !runtime.dry_run {
        for (account_hash, kind, signal) in pending_entries {
            if let Ok(action) = maybe_execute_entry(
                runtime,
                trader,
                &account_hash,
                kind,
                &signal,
                rules,
                state,
            )
            .await
            {
                if let Some(a) = action {
                    result.actions.push(a.clone());
                    let label = a
                        .pointer("/fill_status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("UNKNOWN");
                    match label {
                        "FILLED" => notify_action(telegram, "ENTRY FILLED", &a).await,
                        "WORKING" | "ACCEPTED" | "PENDING_ACTIVATION" | "QUEUED" => {
                            notify_action(telegram, "ORDER WORKING (limit)", &a).await
                        }
                        other if is_failure_status(other) => {
                            notify_action(telegram, "ORDER REJECTED", &a).await
                        }
                        _ => notify_action(telegram, "ORDER", &a).await,
                    }
                }
            }
        }
    }

    Ok(result)
}

fn should_run_monitor_review(rules: &RulesConfig, state: &AgentState) -> bool {
    schedule::should_run_monitor_review(
        state.regular_tick_count,
        state.last_llm_review_tick,
        rules.llm.review_every_ticks,
    )
}

async fn tick_overnight(
    _runtime: &RuntimeConfig,
    rules: &RulesConfig,
    state: &mut AgentState,
    llm_client: Option<&OpenRouterClient>,
    telegram: Option<&TelegramNotifier>,
    result: &mut TickResult,
) -> Result<TickResult> {
    let today = Local::now().date_naive();
    result.monitored_positions = overnight_position_snapshots(state);

    if result.monitored_positions.is_empty() {
        result.skipped.push("overnight — no open positions".into());
    } else {
        result.skipped.push(format!(
            "overnight — monitoring {} open position(s) (no live marks)",
            result.monitored_positions.len()
        ));
    }

    let digest_due = schedule::should_run_overnight_digest(
        state,
        &rules.schedule.overnight,
        Utc::now(),
    );

    if !digest_due {
        result.skipped.push(format!(
            "overnight digest next in ~{} min",
            rules.schedule.overnight.tick_interval_seconds / 60
        ));
        return Ok(result.clone());
    }

    if !rules.llm.enabled {
        result.skipped.push("overnight digest skipped (llm.enabled false)".into());
        return Ok(result.clone());
    }

    let Some(client) = llm_client else {
        result.skipped.push("overnight digest skipped (no LLM client)".into());
        return Ok(result.clone());
    };

    let context = json!({
        "agent_id": rules.agent_id,
        "date": today.to_string(),
        "phase": "overnight_digest",
        "market_closed": true,
        "open_positions": result.monitored_positions,
        "prior_open_playbook": state.open_playbook,
        "watchlist": rules.watchlist,
        "exit_rules": super::exits::exit_rules_summary(&rules.exit_rules),
        "note": "Build open playbook for next session. No chain data. new_entries must be skip.",
    });

    match client
        .review(
            &rules.llm,
            LlmPhase::OvernightDigest,
            &context,
            true,
        )
        .await
    {
        Ok(review) => {
            let review_json = review.to_json();
            result.llm_review = Some(review_json.clone());
            state.last_overnight_digest_at = Some(Utc::now());
            state.open_playbook = Some(json!({
                "updated_at": Utc::now(),
                "market_commentary": review.market_commentary,
                "positions": review.position_reviews,
                "risk_alerts": review.risk_alerts,
                "open_actions": review.entry_reasoning,
            }));
            state.last_llm_summary = Some(review_json.clone());
            state.record_action("overnight_digest", review_json);

            let should_notify = if rules.schedule.overnight.alert_on_risk_only {
                !review.risk_alerts.is_empty()
            } else {
                !review.risk_alerts.is_empty() || !review.market_commentary.is_empty()
            };
            if should_notify {
                notify_overnight_alert(telegram, &review).await;
            }
        }
        Err(e) => result.skipped.push(format!("overnight digest failed: {e:#}")),
    }

    Ok(result.clone())
}

fn overnight_position_snapshots(state: &AgentState) -> Vec<Value> {
    state
        .open_positions
        .values()
        .map(|p| {
            json!({
                "position_id": p.position_id,
                "underlying": p.underlying,
                "expiry": p.expiry,
                "strategy": p.strategy,
                "entry_credit": p.entry_credit,
                "max_loss_usd": p.max_loss_usd,
                "status": "overnight (reconciled, no live marks)",
            })
        })
        .collect()
}

async fn notify_at_open(telegram: Option<&TelegramNotifier>, playbook: Option<&Value>) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let body = if let Some(pb) = playbook {
        let commentary = pb
            .get("market_commentary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no overnight playbook)");
        format!("Market open — running full evaluation.\n\nOvernight playbook:\n{commentary}")
    } else {
        "Market open — running full evaluation.".into()
    };
    let _ = tg.send(&body).await;
}

async fn notify_overnight_alert(
    telegram: Option<&TelegramNotifier>,
    review: &super::llm::LlmReview,
) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let alerts = if review.risk_alerts.is_empty() {
        String::new()
    } else {
        format!("\nAlerts: {}", review.risk_alerts.join("; "))
    };
    let _ = tg
        .send(&format!(
            "OVERNIGHT DIGEST\n{}{}",
            review.market_commentary, alerts
        ))
        .await;
}

async fn market_is_open(market: &MarketDataApi) -> Result<bool> {
    let hours = market.markets().hours("option", None).await?;
    let open = hours
        .pointer("/option/EQO/isOpen")
        .or_else(|| hours.pointer("/option/option/isOpen"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    Ok(open)
}

fn resolve_llm_phase(
    rules: &RulesConfig,
    state: &AgentState,
    has_candidates: bool,
    has_positions: bool,
) -> Option<LlmPhase> {
    if !rules.llm.enabled {
        return None;
    }
    if has_candidates {
        return Some(LlmPhase::Selection);
    }
    if has_positions && should_run_monitor_review(rules, state) {
        return Some(LlmPhase::Monitor);
    }
    None
}

/// Every Nth LLM review uses web_model during selection phase.
fn should_use_web_research(rules: &RulesConfig, state: &AgentState) -> bool {
    if rules.llm.web_research_every_reviews == 0 {
        return false;
    }
    let next_review = state.llm_review_count + 1;
    next_review % rules.llm.web_research_every_reviews.max(1) == 0
}

async fn notify_tick(
    telegram: Option<&TelegramNotifier>,
    rules: &RulesConfig,
    result: &TickResult,
    dry_run: bool,
) {
    let Some(tg) = telegram else { return };
    if !tg.wants_tick_summary() {
        return;
    }
    let prefix = if dry_run { "[DRY RUN] " } else { "" };
    let msg = format!(
        "{prefix}Agent `{}` tick\nsignals: {}\nactions: {}\nskipped: {}",
        rules.agent_id,
        result.signals.len(),
        result.actions.len(),
        result.skipped.len()
    );
    let _ = tg.send(&msg).await;
}

async fn notify_action(telegram: Option<&TelegramNotifier>, kind: &str, detail: &Value) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let msg = format!("{kind}\n```\n{}\n```", serde_json::to_string_pretty(detail).unwrap_or_default());
    let _ = tg.send(&msg).await;
}

async fn notify_llm(telegram: Option<&TelegramNotifier>, commentary: &str, alerts: &[String]) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let alerts_text = if alerts.is_empty() {
        String::new()
    } else {
        format!("\nAlerts: {}", alerts.join("; "))
    };
    let _ = tg.send(&format!("LLM review\n{commentary}{alerts_text}")).await;
}

async fn evaluate_vertical_entry(
    market: &MarketDataApi,
    rules: &RulesConfig,
    underlying: &str,
    today: NaiveDate,
    state: &AgentState,
    account_hash: &str,
) -> Result<Option<Value>> {
    let entry = &rules.entry_rules.vertical;
    let open_count = state.count_open_for_strategy(account_hash, StrategyKind::Vertical);
    if open_count >= entry.max_open_positions {
        return Ok(None);
    }

    let chain = market
        .chains()
        .get(&ChainQuery {
            symbol: underlying,
            contract_type: Some("PUT"),
            strike_count: Some(50),
            include_underlying_quote: Some(true),
            ..Default::default()
        })
        .await?;

    let (expiry, put_map) = pick_expiry_map(&chain, "putExpDateMap", entry.dte_min, entry.dte_max, today)?;
    let underlying_price = chain
        .pointer("/underlying/last")
        .or_else(|| chain.pointer("/underlyingPrice"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    if underlying_price <= 0.0 {
        return Ok(None);
    }

    let short_strike = pick_strike_by_delta(
        &put_map,
        entry.short_delta_min,
        entry.short_delta_max,
        true,
    )
    .or_else(|| pick_otm_strike(&put_map, underlying_price, 0.10, true).ok())
    .context("no suitable short strike")?;
    let long_strike = pick_wing_strike(&put_map, short_strike, entry.max_width, true)?;
    let credit = estimate_spread_credit(&put_map, short_strike, long_strike)?;
    if credit < entry.min_credit {
        return Ok(None);
    }

    let params = VerticalParams {
        underlying: underlying.to_string(),
        expiry: expiry.to_string(),
        spread_type: entry.r#type.clone(),
        short_strike,
        long_strike,
        contracts: entry.max_contracts_per_trade as f64,
        limit_credit: Some(credit),
        limit_debit: None,
        duration: None,
        session: None,
    };

    let market_context = vertical_entry_market_context(
        &chain,
        underlying,
        expiry,
        today,
        &put_map,
        short_strike,
        long_strike,
        entry.max_width,
        credit,
        entry.max_contracts_per_trade as f64,
    );

    Ok(Some(json!({
        "type": "entry",
        "strategy": "vertical",
        "account_hash": account_hash,
        "params": params,
        "estimated_credit": credit,
        "market_context": market_context,
    })))
}

async fn evaluate_condor_entry(
    market: &MarketDataApi,
    rules: &RulesConfig,
    underlying: &str,
    today: NaiveDate,
    state: &AgentState,
    account_hash: &str,
) -> Result<Option<Value>> {
    let entry = &rules.entry_rules.iron_condor;
    let open_count = state.count_open_for_strategy(account_hash, StrategyKind::IronCondor);
    if open_count >= entry.max_open_positions {
        return Ok(None);
    }

    let chain = market
        .chains()
        .get(&ChainQuery {
            symbol: underlying,
            contract_type: Some("ALL"),
            strike_count: Some(40),
            include_underlying_quote: Some(true),
            ..Default::default()
        })
        .await?;

    let underlying_price = chain
        .pointer("/underlying/last")
        .or_else(|| chain.pointer("/underlyingPrice"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if underlying_price <= 0.0 {
        return Ok(None);
    }

    let (expiry, put_map) = pick_expiry_map(&chain, "putExpDateMap", entry.dte_min, entry.dte_max, today)?;
    let (_, call_map) = pick_expiry_map(&chain, "callExpDateMap", entry.dte_min, entry.dte_max, today)?;

    let put_short = pick_otm_strike(&put_map, underlying_price, entry.short_delta, true)?;
    let put_long = put_short - entry.wing_width;
    let call_short = pick_otm_strike(&call_map, underlying_price, entry.short_delta, false)?;
    let call_long = call_short + entry.wing_width;

    let put_credit = estimate_spread_credit(&put_map, put_short, put_long)?;
    let call_credit = estimate_spread_credit(&call_map, call_short, call_long)?;
    let total_credit = put_credit + call_credit;
    if total_credit < entry.min_credit {
        return Ok(None);
    }

    let params = IronCondorParams {
        underlying: underlying.to_string(),
        expiry: expiry.to_string(),
        put_short,
        put_long,
        call_short,
        call_long,
        contracts: entry.max_contracts_per_trade as f64,
        limit_credit: total_credit,
        duration: None,
        session: None,
    };

    Ok(Some(json!({
        "type": "entry",
        "strategy": "iron_condor",
        "account_hash": account_hash,
        "params": params,
        "estimated_credit": total_credit,
    })))
}

fn pick_expiry_map(
    chain: &Value,
    map_key: &str,
    dte_min: u32,
    dte_max: u32,
    today: NaiveDate,
) -> Result<(NaiveDate, Value)> {
    let map = chain
        .get(map_key)
        .context("chain missing exp date map")?
        .as_object()
        .context("exp date map not an object")?;

    for key in map.keys() {
        let date_part = key.split(':').next().unwrap_or(key);
        if let Ok(expiry) = parse_expiry(date_part) {
            let dte = days_to_expiry(expiry, today);
            if dte >= dte_min as i64 && dte <= dte_max as i64 {
                if let Some(strikes) = map.get(key) {
                    return Ok((expiry, strikes.clone()));
                }
            }
        }
    }
    anyhow::bail!("no expiry found in DTE window {dte_min}-{dte_max}")
}

fn pick_otm_strike(strike_map: &Value, underlying: f64, otm_pct: f64, puts: bool) -> Result<f64> {
    let target = if puts {
        underlying * (1.0 - otm_pct)
    } else {
        underlying * (1.0 + otm_pct)
    };
    pick_nearest_strike(strike_map, target)
}

/// For put credit spreads, long strike is below short by approximately `width`.
fn pick_wing_strike(
    strike_map: &Value,
    short_strike: f64,
    width: f64,
    puts: bool,
) -> Result<f64> {
    let target = if puts {
        short_strike - width
    } else {
        short_strike + width
    };
    pick_nearest_strike(strike_map, target)
}

fn pick_nearest_strike(strike_map: &Value, target: f64) -> Result<f64> {
    let obj = strike_map.as_object().context("strike map not object")?;
    let candidates: Vec<f64> = obj
        .keys()
        .filter_map(|k| k.parse::<f64>().ok())
        .collect();
    if candidates.is_empty() {
        anyhow::bail!("no strikes in chain");
    }
    candidates
        .into_iter()
        .min_by(|a, b| {
            ((*a - target).abs())
                .partial_cmp(&(*b - target).abs())
                .unwrap()
        })
        .context("no strike candidates")
}

/// Pick put strike whose |delta| is closest to the middle of [delta_min, delta_max].
fn pick_strike_by_delta(
    strike_map: &Value,
    delta_min: f64,
    delta_max: f64,
    puts: bool,
) -> Option<f64> {
    let obj = strike_map.as_object()?;
    let target = (delta_min + delta_max) / 2.0;
    let mut best: Option<(f64, f64)> = None;
    for (key, contracts) in obj {
        let strike = key.parse::<f64>().ok()?;
        let delta = contracts
            .as_array()?
            .first()?
            .get("delta")?
            .as_f64()?;
        let abs_delta = delta.abs();
        if puts && delta > 0.0 {
            continue;
        }
        let dist = (abs_delta - target).abs();
        if best.is_none() || dist < best.unwrap().1 {
            best = Some((strike, dist));
        }
    }
    best.map(|(s, _)| s)
}

fn estimate_spread_credit(put_map: &Value, short: f64, long: f64) -> Result<f64> {
    let short_bid = strike_quote_field(put_map, short, "bid")?;
    let long_ask = strike_quote_field(put_map, long, "ask")?;
    Ok((short_bid - long_ask).max(0.0))
}

fn strike_quote_field(strike_map: &Value, strike: f64, field: &str) -> Result<f64> {
    for key in strike_key_candidates(strike) {
        if let Some(val) = strike_map
            .get(&key)
            .and_then(|contracts| contracts.as_array()?.first())
            .and_then(|c| c.get(field))
            .and_then(|v| v.as_f64())
        {
            return Ok(val);
        }
    }
    anyhow::bail!("missing {field} for strike {strike}")
}

fn strike_key_candidates(strike: f64) -> Vec<String> {
    vec![
        format!("{strike:.1}"),
        format!("{strike:.0}"),
        strike.to_string(),
    ]
}

async fn maybe_execute_entry(
    runtime: &RuntimeConfig,
    trader: &Arc<TraderApi>,
    account_hash: &str,
    kind: StrategyKind,
    signal: &Value,
    rules: &RulesConfig,
    state: &mut AgentState,
) -> Result<Option<Value>> {
    if runtime.dry_run {
        return Ok(None);
    }

    if state.trades_today >= rules.risk.max_trades_per_day {
        return Ok(None);
    }

    let params = signal.get("params").cloned().context("signal missing params")?;
    let margin = crate::options::validate::estimate_order_margin(&json!({}), kind, &params)?;
    if margin > rules.risk.max_risk_per_trade_usd {
        return Ok(None);
    }

    require_trading_approval(
        runtime,
        "agent entry",
        &format!("Open {kind:?} on {account_hash}"),
    )?;

    ensure_option_buying_power(trader, account_hash, margin).await?;
    let order = build_order_for_strategy(kind, &params)?;
    runtime.safety.validate_order(&order, None, None)?;

    let place = execute_trading_order(runtime, trader, account_hash, &order).await?;

    let order_id = place
        .get("order_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let wait_result = if let Some(ref order_id) = order_id {
        let condition = if rules.execution.wait_for_fill {
            WaitCondition::Terminal
        } else {
            WaitCondition::Accepted
        };
        Some(
            wait_for_order(
                trader,
                account_hash,
                order_id,
                WaitOptions {
                    condition,
                    timeout: std::time::Duration::from_secs(rules.execution.fill_timeout_seconds),
                    interval: std::time::Duration::from_secs(5),
                    proceed_on_partial_fill: false,
                    requested_quantity: None,
                },
            )
            .await?,
        )
    } else {
        None
    };

    let fill_status = wait_result
        .as_ref()
        .and_then(|w| w.final_status.as_deref())
        .unwrap_or("ACCEPTED");

    if is_failure_status(fill_status) {
        let detail = json!({
            "signal": signal,
            "place": place,
            "wait": wait_result.as_ref().map(wait_result_json),
            "fill_status": fill_status,
        });
        state.record_action("entry_rejected", detail.clone());
        return Ok(Some(detail));
    }

    if fill_status != "FILLED" && rules.execution.wait_for_fill {
        let detail = json!({
            "signal": signal,
            "place": place,
            "wait": wait_result.as_ref().map(wait_result_json),
            "fill_status": fill_status,
            "note": "Limit order working; position not opened in agent state until filled",
        });
        state.record_action("entry_working", detail.clone());
        return Ok(Some(detail));
    }

    state.trades_today += 1;
    let underlying = params
        .get("underlying")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let expiry = params
        .get("expiry")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let position_id = position_key(&underlying, &expiry);
    state.open_positions.insert(
        position_id.clone(),
        TrackedPosition {
            position_id,
            account_hash: account_hash.to_string(),
            underlying,
            expiry,
            strategy: kind.as_str().to_string(),
            opened_at: Utc::now(),
            entry_credit: signal.get("estimated_credit").and_then(|v| v.as_f64()),
            max_loss_usd: margin,
        },
    );
    state.record_action("entry", signal.clone());

    Ok(Some(json!({
        "entry": place,
        "signal": signal,
        "wait": wait_result.as_ref().map(wait_result_json),
        "fill_status": fill_status,
    })))
}

async fn execute_exit(
    runtime: &RuntimeConfig,
    trader: &Arc<TraderApi>,
    account_hash: &str,
    rules: &RulesConfig,
    group: &crate::options::OptionPositionGroup,
    signal: &Value,
    state: &mut AgentState,
) -> Result<Value> {
    require_trading_approval(
        runtime,
        "agent exit",
        &format!("Close position {}", group.id),
    )?;

    let order = build_close_order_for_group(group)?;
    runtime.safety.validate_order(&order, None, None)?;
    let place = execute_trading_order(runtime, trader, account_hash, &order).await?;

    if rules.execution.wait_for_fill {
        if let Some(order_id) = place.get("order_id").and_then(|v| v.as_str()) {
            let _ = wait_for_order(
                trader,
                account_hash,
                order_id,
                WaitOptions {
                    condition: WaitCondition::Filled,
                    timeout: std::time::Duration::from_secs(rules.execution.fill_timeout_seconds),
                    interval: std::time::Duration::from_secs(5),
                    proceed_on_partial_fill: false,
                    requested_quantity: None,
                },
            )
            .await;
        }
    }

    state.open_positions.remove(&group.id);
    state.record_action("exit", signal.clone());
    Ok(json!({ "exit": place, "signal": signal }))
}
