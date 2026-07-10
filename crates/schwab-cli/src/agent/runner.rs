use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, Utc};
use schwab_api::TraderApi;
use schwab_market_data::endpoints::chains::ChainQuery;
use schwab_market_data::MarketDataApi;
use serde_json::{json, Value};

use crate::auth_reminder::{
    assess_refresh_token, maybe_notify_auth_reminder, notify_auth_required,
};
use crate::config::RuntimeConfig;
use crate::notify::TelegramNotifier;
use crate::options::positions::build_close_order_for_group_with_limit;
use crate::options::{
    build_order_for_strategy, candidate_position_id, days_to_expiry, ensure_option_buying_power,
    group_option_legs, list_option_positions, parse_expiry, IronCondorParams, StrategyKind,
    VerticalParams,
};
use crate::order_status::{
    is_failure_status, is_terminal_status, order_status, wait_for_order, wait_result_json,
    WaitCondition, WaitOptions,
};
use crate::rules::{LlmPhase, RulesConfig, VerticalEntryRules};
use crate::safety::{execute_trading_order, require_trading_approval};
use crate::trade_audio::{self, TradeAudioEvent};

use super::exits::{
    candidate_fails_thesis_gates, evaluate_position_monitor, exit_signal_json_for_account,
    find_tracked_position, option_group_from_tracked, reconcile_open_positions, stable_position_key,
};
use super::llm::OpenRouterClient;
use super::market_context::{market_context_summary_for_llm, vertical_entry_market_context};
use super::regime::{detect_options_regime, OptionsRegimeSnapshot};
use super::spread_analytics::{analytics_from_json, entry_analytics_pass};
use super::paths::{active_state_path, load_agent_state, load_sim_agent_state};
use super::sim::{ensure_ledger, record_sim_entry, record_sim_exit};
use super::schedule::{self, AgentSession};
use super::state::{
    is_thesis_exit_reason, save_state, update_peak_profit_pct, AgentState, PendingOrder,
    PendingOrderAction, RedeploySignal, TrackedPosition,
};
use super::telegram_format::{
    format_action_telegram, format_llm_review_telegram, format_market_open_telegram,
    format_overnight_telegram, record_llm_telegram_sent, should_send_llm_telegram,
};
use crate::ui::agent_health::{is_fatal_auth_error, SharedAgentHealth};

const TICK_ERROR_BACKOFF_SECS: u64 = 60;
const MAX_ENTRY_QUOTE_WIDTH_RATIO: f64 = 1.0;
const EXIT_LIMIT_SLIPPAGE: f64 = 0.05;

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
    watch_health: Option<SharedAgentHealth>,
) -> Result<()> {
    let rules = RulesConfig::load(rules_path)?;
    let state_path = active_state_path(rules_path, runtime.simulate);
    let mut state = if runtime.simulate {
        load_sim_agent_state(rules_path, &rules.agent_id)
    } else {
        load_agent_state(rules_path, &rules.agent_id)
    };
    state.agent_id = rules.agent_id.clone();

    trade_audio::init(runtime.no_audio);

    if rules.execution.require_preview && !runtime.safety.require_preview_before_place {
        anyhow::bail!(
            "rules require preview before order placement, but safety.json has require_preview_before_place=false"
        );
    }

    if !runtime.dry_run && !runtime.simulate {
        crate::safety::require_trading_approval(
            runtime,
            "agent run",
            &format!("Run options agent `{}`", rules.agent_id),
        )?;
    }

    let trader = runtime.build_api()?;
    let market = runtime.build_market_api()?;
    let telegram = TelegramNotifier::from_env(&rules.notify.telegram)
        .ok()
        .flatten();
    let llm_client = if rules.llm.enabled {
        match OpenRouterClient::from_env() {
            Ok(client) => Some(client),
            Err(e) => {
                let msg = format!(
                    "LLM disabled for this run: {e:#} (set OPENROUTER_API_KEY or llm.enabled: false)"
                );
                let _ = super::paths::append_agent_log(rules_path, &msg);
                if let Some(h) = watch_health.as_ref() {
                    if let Ok(mut g) = h.lock() {
                        g.record_error(&msg);
                    }
                }
                None
            }
        }
    } else {
        None
    };

    let mut consecutive_errors = 0u32;
    let mut last_logged_error: Option<String> = None;

    loop {
        match tick_once(
            runtime,
            rules_path,
            &rules,
            &trader,
            &market,
            &mut state,
            llm_client.as_ref(),
            telegram.as_ref(),
        )
        .await
        {
            Ok(result) => {
                consecutive_errors = 0;
                state.last_tick = Some(Utc::now());
                save_state(&state_path, &state)?;

                if let Some(h) = watch_health.as_ref() {
                    if let Ok(mut g) = h.lock() {
                        g.record_tick();
                    }
                }

                let tick_payload = json!({
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
                    "simulate": runtime.simulate,
                });

                if runtime.suppress_tick_output {
                    let summary = format!(
                        "{} session={} signals={} actions={} skipped={}",
                        Utc::now().format("%Y-%m-%d %H:%M:%S"),
                        result.session,
                        result.signals.len(),
                        result.actions.len(),
                        result.skipped.len()
                    );
                    let _ = super::paths::append_agent_log(rules_path, &summary);
                } else {
                    runtime.emit(crate::output::ResponseEnvelope::ok(
                        if once { "agent run once" } else { "agent tick" },
                        tick_payload,
                    ));
                }

                notify_tick(telegram.as_ref(), &rules, &result, runtime.dry_run).await;

                if once {
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_secs(result.next_sleep_seconds)).await;
            }
            Err(e) => {
                consecutive_errors += 1;
                let err_str = format!("{e:#}");

                if is_fatal_auth_error(&err_str) {
                    let msg = "agent stopped: Schwab login required (refresh token invalid). Run: schwab auth login";
                    if last_logged_error.as_deref() != Some(msg) {
                        let _ = super::paths::append_agent_log(rules_path, msg);
                    }
                    if let Some(h) = watch_health.as_ref() {
                        if let Ok(mut g) = h.lock() {
                            g.record_error(msg);
                        }
                    }
                    notify_auth_required(telegram.as_ref(), msg).await;
                    if once {
                        return Err(e);
                    }
                    break;
                }

                let msg = format!("tick error (#{consecutive_errors}): {err_str}");
                if last_logged_error.as_deref() != Some(msg.as_str()) {
                    let _ = super::paths::append_agent_log(rules_path, &msg);
                    last_logged_error = Some(msg.clone());
                }
                if let Some(h) = watch_health.as_ref() {
                    if let Ok(mut g) = h.lock() {
                        g.record_error(&msg);
                    }
                }
                if once {
                    return Err(e);
                }
                let backoff = TICK_ERROR_BACKOFF_SECS
                    .saturating_mul(consecutive_errors.min(5) as u64)
                    .max(30);
                tokio::time::sleep(Duration::from_secs(backoff)).await;
            }
        }
    }

    if let Some(h) = watch_health.as_ref() {
        if let Ok(mut g) = h.lock() {
            g.loop_running = false;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn tick_once(
    runtime: &RuntimeConfig,
    rules_path: &std::path::Path,
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

    check_auth_reminder(state, telegram).await;
    maybe_clear_stale_redeploy(state);

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

    if runtime.simulate {
        let _ = ensure_ledger(state, rules);
        result
            .skipped
            .push("simulate — using paper state (no Schwab reconcile)".into());
    } else {
        reconcile_open_positions(trader, state, rules).await?;
        poll_pending_orders(trader, state, rules, &mut result).await?;
    }

    let (market_open, hours) = fetch_option_market_status(market).await?;
    state.last_market_open = Some(market_open);
    if let Some(ref h) = hours {
        let _ = crate::ui::market_status::save_market_hours_cache(rules_path, h);
    }
    let transition =
        schedule::resolve_session(market_open, &rules.schedule, state.last_session.as_deref());
    result.session = transition.session.as_str().to_string();
    result.next_sleep_seconds = transition.sleep_seconds;
    state.last_session = Some(result.session.clone());

    match transition.session {
        AgentSession::Idle => {
            result.skipped.push("market closed (option hours)".into());
            return Ok(result);
        }
        AgentSession::Overnight => {
            return tick_overnight(runtime, rules, state, llm_client, telegram, &mut result).await;
        }
        AgentSession::RegularHours => {
            if transition.just_opened {
                result.at_open = true;
                result
                    .skipped
                    .push("market open — full evaluation (mechanical exits + live marks)".into());
            }
            state.regular_tick_count += 1;
        }
    }

    let prior_open_playbook = if result.at_open {
        let pb = state.open_playbook.take();
        notify_at_open(
            telegram,
            pb.as_ref(),
            state.open_positions.len(),
        )
        .await;
        pb
    } else {
        None
    };

    let entries_paused = rules.risk.max_trades_per_day > 0
        && state.trades_capacity_used() >= rules.risk.max_trades_per_day;
    let blocked_events_active = !rules.risk.blocked_events.is_empty();

    let mut regime_snap: Option<OptionsRegimeSnapshot> = None;
    if rules.regime.enabled && !entries_paused && !blocked_events_active {
        match detect_options_regime(market, &rules.regime).await {
            Ok(snap) => {
                state.last_regime = Some(snap.to_json());
                if snap.pause_entries {
                    result.skipped.push(format!(
                        "regime pause — {} (preferred={})",
                        snap.class, snap.preferred_strategy
                    ));
                }
                regime_snap = Some(snap);
            }
            Err(e) => {
                result
                    .skipped
                    .push(format!("regime detect failed (continuing with defaults): {e:#}"));
            }
        }
    }

    // Exit evaluation and position monitoring (always runs when market is open)
    if runtime.simulate {
        let position_ids: Vec<String> = state.open_positions.keys().cloned().collect();
        for position_id in position_ids {
            let Some(tracked) = state.open_positions.get(&position_id).cloned() else {
                continue;
            };
            let Some(group) = option_group_from_tracked(&tracked) else {
                result.skipped.push(format!(
                    "simulate exit skip {position_id}: missing entry_params"
                ));
                continue;
            };
            let monitor =
                evaluate_position_monitor(market, &group, rules, today, Some(&tracked)).await?;
            if let Some(profit) = monitor.mark.as_ref().map(|m| m.profit_pct) {
                if let Some(p) = state.open_positions.get_mut(&position_id) {
                    update_peak_profit_pct(p, profit);
                }
            }
            if !group.legs.is_empty() {
                result.monitored_positions.push(monitor.snapshot);
            }
            if let Some(eval) = monitor.exit {
                if is_thesis_exit_reason(&eval.reason) {
                    state.redeploy_signal = Some(RedeploySignal {
                        at: Utc::now(),
                        reason: eval.reason.clone(),
                        underlying: Some(tracked.underlying.clone()),
                    });
                }
                let exit = exit_signal_json_for_account(&tracked.account_hash, &group, &eval);
                result.signals.push(exit.clone());
                if !runtime.dry_run {
                    match record_sim_exit(
                        rules_path,
                        state,
                        rules,
                        &position_id,
                        &eval.reason,
                        &eval.mark,
                        &exit,
                    ) {
                        Ok(action) => {
                            result.actions.push(action);
                            notify_action(telegram, "SIM EXIT", &exit).await;
                        }
                        Err(err) => {
                            result
                                .skipped
                                .push(format!("sim exit failed {position_id}: {err:#}"));
                        }
                    }
                }
            }
        }
    } else {
        for account in rules.enabled_accounts() {
            let legs = list_option_positions(trader, Some(&account.hash)).await?;
            let groups = group_option_legs(&legs);
            for group in &groups {
                let tracked = find_tracked_position(state, &account.hash, group);
                let monitor = evaluate_position_monitor(market, group, rules, today, tracked).await?;
                let position_id = stable_position_key(&account.hash, group);
                if let Some(profit) = monitor.mark.as_ref().map(|m| m.profit_pct) {
                    if let Some(p) = state.open_positions.get_mut(&position_id) {
                        update_peak_profit_pct(p, profit);
                    }
                }

                if !group.legs.is_empty() {
                    result.monitored_positions.push(monitor.snapshot);
                }

                if let Some(eval) = monitor.exit {
                    if is_thesis_exit_reason(&eval.reason) {
                        state.redeploy_signal = Some(RedeploySignal {
                            at: Utc::now(),
                            reason: eval.reason.clone(),
                            underlying: Some(group.underlying.clone()),
                        });
                    }
                    let exit = exit_signal_json_for_account(&account.hash, group, &eval);
                    result.signals.push(exit.clone());
                    if state.has_pending_position(&position_id) {
                        result
                            .skipped
                            .push(format!("exit already pending for {position_id}"));
                    } else if !runtime.dry_run {
                        if let Ok(action) =
                            execute_exit(runtime, trader, &account.hash, rules, group, &exit, state)
                                .await
                        {
                            result.actions.push(action);
                            notify_action(telegram, "EXIT", &exit).await;
                        }
                    }
                }
            }
        }
    }

    // Entry scan (signals collected; execution after LLM review)
    let mut pending_entries: Vec<(String, StrategyKind, Value)> = Vec::new();
    let regime_pause = regime_snap.as_ref().is_some_and(|s| s.pause_entries);
    let preferred = regime_snap
        .as_ref()
        .map(|s| s.preferred_strategy.as_str())
        .unwrap_or("put_credit");

    if entries_paused {
        result.skipped.push(format!(
            "new entries paused — max_trades_per_day ({}) reached or reserved by pending entries \
             (soft churn cap; hard gates are max_open_positions + portfolio/trade risk)",
            rules.risk.max_trades_per_day
        ));
    } else if blocked_events_active {
        result.skipped.push(format!(
            "new entries paused — blocked_events active: {}",
            rules.risk.blocked_events.join(", ")
        ));
    } else if regime_pause || preferred.eq_ignore_ascii_case("pause") {
        result.skipped.push(format!(
            "new entries paused — hostile/pause regime ({})",
            regime_snap
                .as_ref()
                .map(|s| s.class.as_str())
                .unwrap_or("pause")
        ));
    } else if !any_entry_slots_available(rules, state) {
        result
            .skipped
            .push("entry scan skipped — all enabled accounts at max_open_positions".into());
    } else {
        let scan_watchlist = watchlist_for_scan(rules, state);
        if let Some(sig) = &state.redeploy_signal {
            if let Some(u) = &sig.underlying {
                if redeploy_cooldown_active(rules, sig) {
                    let remaining = rules
                        .exit_rules
                        .thesis
                        .redeploy_cooldown_minutes
                        .unwrap_or(0) as i64
                        - Utc::now().signed_duration_since(sig.at).num_minutes();
                    result.skipped.push(format!(
                        "redeploy cooldown — skip {} for ~{}m after {}",
                        u.to_uppercase(),
                        remaining.max(0),
                        sig.reason
                    ));
                }
            }
        }
        let (want_vertical, want_condor, vertical_type) = if rules.regime.enabled {
            let want_vertical = preferred.eq_ignore_ascii_case("put_credit")
                || preferred.eq_ignore_ascii_case("call_credit");
            let want_condor = preferred.eq_ignore_ascii_case("iron_condor");
            let vertical_type = if preferred.eq_ignore_ascii_case("call_credit") {
                "call_credit"
            } else {
                "put_credit"
            };
            (want_vertical, want_condor, vertical_type)
        } else {
            // Legacy: scan configured vertical type + iron condor when enabled.
            (
                true,
                true,
                rules.entry_rules.vertical.r#type.as_str(),
            )
        };

        for account in rules.enabled_accounts() {
            for underlying in &scan_watchlist {
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

                if rules.strategies.vertical.enabled && want_vertical {
                    match evaluate_vertical_entry(
                        market,
                        rules,
                        &sym,
                        today,
                        state,
                        &account.hash,
                        vertical_type,
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
                        Err(e) => result.skipped.push(format!("{sym} vertical: {e:#}")),
                    }
                }

                if rules.strategies.iron_condor.enabled && want_condor {
                    match evaluate_condor_entry(market, rules, &sym, today, state, &account.hash)
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
                        Err(e) => result.skipped.push(format!("{sym} iron_condor: {e:#}")),
                    }
                }
            }
        }
        if rules.regime.enabled {
            result.skipped.push(format!(
                "regime={} preferred={} vix={}",
                regime_snap
                    .as_ref()
                    .map(|s| s.class.as_str())
                    .unwrap_or("?"),
                preferred,
                regime_snap
                    .as_ref()
                    .and_then(|s| s.vix)
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or_else(|| "?".into())
            ));
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
    let mechanical_alert =
        mechanical_alert_from_monitored(&result.monitored_positions, rules.exit_rules.dte_close);

    if let Some(client) = llm_client {
        if let Some(phase) = resolve_llm_phase(
            rules,
            state,
            has_candidates,
            has_positions,
            result.at_open,
            mechanical_alert,
        ) {
            let use_web =
                should_use_web_research(rules, state) && matches!(phase, LlmPhase::Selection);

            let open_playbook_for_llm = match phase {
                LlmPhase::Monitor if result.at_open => prior_open_playbook.as_ref(),
                _ => None,
            };

            let context = json!({
                "agent_id": rules.agent_id,
                "tick": state.regular_tick_count,
                "date": today.to_string(),
                "phase": match phase {
                    LlmPhase::Selection => "selection",
                    LlmPhase::Monitor => "monitor",
                    LlmPhase::OvernightDigest => "overnight_digest",
                },
                "at_open": result.at_open,
                "mechanical_alert": mechanical_alert,
                "market": market_context_summary_for_llm(),
                "regime": state.last_regime.clone(),
                "exit_rules": super::exits::exit_rules_summary(&rules.exit_rules),
                "open_positions": result.monitored_positions,
                "open_playbook": open_playbook_for_llm,
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

                    if rules.llm.veto_entries
                        && matches!(phase, LlmPhase::Selection)
                        && review.should_veto_entries()
                    {
                        llm_veto_entries = true;
                        result
                            .skipped
                            .push(format!("LLM veto entries: {}", review.entry_reasoning));
                    }

                    if rules.llm.allow_llm_exits {
                        for pos in review.urgent_close_positions() {
                            llm_close_ids.push(pos.position_id.clone());
                        }
                    }

                    let notify = should_send_llm_telegram(
                        &review,
                        &rules.notify.telegram,
                        state,
                        Utc::now(),
                    );
                    if notify {
                        notify_llm(
                            telegram,
                            &review,
                            &result.monitored_positions,
                            state,
                        )
                        .await;
                    }
                }
                Err(e) => {
                    result.skipped.push(format!("LLM review failed: {e:#}"));
                    if rules.llm.veto_entries && matches!(phase, LlmPhase::Selection) {
                        llm_veto_entries = true;
                        result
                            .skipped
                            .push("LLM selection failed closed — entries deferred".into());
                    }
                }
            }
        }
    } else if rules.llm.enabled && rules.llm.veto_entries && has_candidates {
        llm_veto_entries = true;
        result
            .skipped
            .push("LLM selection unavailable — entries deferred".into());
    }

    // LLM-requested exits (high urgency only, when enabled)
    if !llm_close_ids.is_empty() && !runtime.dry_run && !runtime.simulate {
        for account in rules.enabled_accounts() {
            let legs = list_option_positions(trader, Some(&account.hash)).await?;
            let groups = group_option_legs(&legs);
            for group in &groups {
                let position_id = stable_position_key(&account.hash, group);
                if !llm_close_ids
                    .iter()
                    .any(|id| id == &position_id || id == &group.id)
                {
                    continue;
                }
                if state.has_pending_position(&position_id) {
                    result
                        .skipped
                        .push(format!("LLM exit already pending for {position_id}"));
                    continue;
                }
                let exit = json!({
                    "type": "exit",
                    "reason": "llm_recommendation",
                    "position_id": position_id,
                    "legacy_position_id": group.id,
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
        if runtime.simulate {
            for (account_hash, kind, signal) in pending_entries {
                match record_sim_entry(rules_path, state, rules, &account_hash, kind, &signal) {
                    Ok(detail) => {
                        if detail
                            .get("fill_status")
                            .and_then(|v| v.as_str())
                            == Some("FILLED")
                        {
                            result.actions.push(detail.clone());
                            notify_action(telegram, "SIM ENTRY", &detail).await;
                        } else {
                            result.skipped.push(format!(
                                "sim entry skipped: {}",
                                detail
                                    .get("reason")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                            ));
                        }
                    }
                    Err(err) => result.skipped.push(format!("sim entry failed: {err:#}")),
                }
            }
        } else {
            for (account_hash, kind, signal) in pending_entries {
                if let Ok(Some(a)) =
                    maybe_execute_entry(runtime, trader, &account_hash, kind, &signal, rules, state)
                        .await
                {
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
    } else if llm_veto_entries && !pending_entries.is_empty() && !runtime.dry_run {
        trade_audio::speak(TradeAudioEvent::EntryDeferred);
    }

    Ok(result)
}

fn should_run_llm_review(rules: &RulesConfig, state: &AgentState, has_positions: bool) -> bool {
    let every = rules.llm.effective_llm_review_ticks(
        has_positions,
        min_open_position_dte(state),
        rules.exit_rules.dte_close,
    );
    schedule::should_run_monitor_review(
        state.regular_tick_count,
        state.last_llm_review_tick,
        every,
    )
}

fn min_open_position_dte(state: &AgentState) -> Option<i64> {
    let today = Local::now().date_naive();
    state
        .open_positions
        .values()
        .filter_map(|p| {
            chrono::NaiveDate::parse_from_str(&p.expiry, "%Y-%m-%d")
                .ok()
                .map(|exp| days_to_expiry(exp, today))
        })
        .min()
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

    let digest_due =
        schedule::should_run_overnight_digest(state, &rules.schedule.overnight, Utc::now());

    if !digest_due {
        result.skipped.push(format!(
            "overnight digest next in ~{} min",
            rules.schedule.overnight.tick_interval_seconds / 60
        ));
        return Ok(result.clone());
    }

    if !rules.llm.enabled {
        result
            .skipped
            .push("overnight digest skipped (llm.enabled false)".into());
        return Ok(result.clone());
    }

    let Some(client) = llm_client else {
        result
            .skipped
            .push("overnight digest skipped (no LLM client)".into());
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
        .review(&rules.llm, LlmPhase::OvernightDigest, &context, true)
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
                !review.risk_alerts.is_empty() || is_llm_urgent_for_overnight(&review)
            } else {
                should_send_llm_telegram(&review, &rules.notify.telegram, state, Utc::now())
            };
            if should_notify {
                notify_overnight_alert(telegram, &review, state).await;
            }
        }
        Err(e) => result
            .skipped
            .push(format!("overnight digest failed: {e:#}")),
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
                "contracts": p.contracts.max(1),
                "entry_credit": p.entry_credit,
                "max_loss_usd": p.max_loss_usd,
                "status": "overnight (reconciled, no live marks)",
            })
        })
        .collect()
}

async fn check_auth_reminder(state: &mut AgentState, telegram: Option<&TelegramNotifier>) {
    let Ok(config) = schwab_api::ClientConfig::from_env() else {
        return;
    };
    let oauth = schwab_api::OAuthClient::new(config);
    let Ok(Some(tokens)) = oauth.status().await else {
        return;
    };
    let reminder = assess_refresh_token(&tokens);
    maybe_notify_auth_reminder(telegram, state, &reminder).await;
}

async fn poll_pending_orders(
    trader: &Arc<TraderApi>,
    state: &mut AgentState,
    rules: &RulesConfig,
    result: &mut TickResult,
) -> Result<()> {
    let pending = state.pending_orders.clone();
    for pending_order in pending {
        let order = match trader
            .orders()
            .get(&pending_order.account_hash, &pending_order.order_id)
            .await
        {
            Ok(order) => order,
            Err(e) => {
                result.skipped.push(format!(
                    "pending order {} status unavailable: {e:#}",
                    pending_order.order_id
                ));
                continue;
            }
        };
        let status = order_status(&order).unwrap_or_else(|| "UNKNOWN".into());
        if let Some(stored) = state
            .pending_orders
            .iter_mut()
            .find(|p| p.order_id == pending_order.order_id)
        {
            stored.last_status = Some(status.clone());
        }

        match pending_order.action {
            PendingOrderAction::Entry => {
                if status == "FILLED" {
                    state.remove_pending_order(&pending_order.order_id);
                    if let Some(detail) = pending_order.detail.as_ref() {
                        track_filled_entry_from_pending(state, detail, &pending_order);
                    }
                    state.trades_today = state.trades_today.saturating_add(1);
                    state.record_action(
                        "entry_filled",
                        json!({
                            "order_id": pending_order.order_id,
                            "position_id": pending_order.position_id,
                            "status": status,
                        }),
                    );
                    trade_audio::speak(TradeAudioEvent::EntryOpened);
                } else if is_failure_status(&status) || is_terminal_status(&status) {
                    state.remove_pending_order(&pending_order.order_id);
                    state.record_action(
                        "entry_terminal",
                        json!({
                            "order_id": pending_order.order_id,
                            "position_id": pending_order.position_id,
                            "status": status,
                        }),
                    );
                } else if pending_is_stale(&pending_order, rules) {
                    match trader
                        .orders()
                        .cancel(&pending_order.account_hash, &pending_order.order_id)
                        .await
                    {
                        Ok(cancel) => {
                            state.remove_pending_order(&pending_order.order_id);
                            state.record_action(
                                "entry_cancelled_stale",
                                json!({
                                    "order_id": pending_order.order_id,
                                    "position_id": pending_order.position_id,
                                    "status": status,
                                    "cancel": {
                                        "status": cancel.status,
                                        "location": cancel.location,
                                    },
                                }),
                            );
                            trade_audio::speak(TradeAudioEvent::EntryCancelled);
                        }
                        Err(e) => result.skipped.push(format!(
                            "stale entry order {} cancel failed: {e:#}",
                            pending_order.order_id
                        )),
                    }
                }
            }
            PendingOrderAction::Exit => {
                if status == "FILLED" {
                    state.remove_pending_order(&pending_order.order_id);
                    state.open_positions.remove(&pending_order.position_id);
                    state.record_action(
                        "exit_filled",
                        json!({
                            "order_id": pending_order.order_id,
                            "position_id": pending_order.position_id,
                            "status": status,
                        }),
                    );
                    let reason = pending_order
                        .detail
                        .as_ref()
                        .and_then(|d| d.pointer("/signal/reason"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    trade_audio::speak_exit_reason(reason);
                } else if is_failure_status(&status) || is_terminal_status(&status) {
                    state.remove_pending_order(&pending_order.order_id);
                    state.record_action(
                        "exit_terminal_position_kept",
                        json!({
                            "order_id": pending_order.order_id,
                            "position_id": pending_order.position_id,
                            "status": status,
                        }),
                    );
                }
            }
        }
    }
    state.clear_legacy_pending_ids();
    Ok(())
}

fn pending_is_stale(pending: &PendingOrder, rules: &RulesConfig) -> bool {
    let timeout = rules.execution.fill_timeout_seconds.max(1) as i64;
    (Utc::now() - pending.submitted_at).num_seconds() >= timeout
}

fn track_filled_entry_from_pending(state: &mut AgentState, detail: &Value, pending: &PendingOrder) {
    let Some(signal) = detail.get("signal") else {
        return;
    };
    let Some(params) = signal.get("params") else {
        return;
    };
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
    let strategy = signal
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("vertical")
        .to_string();
    let contracts = params
        .get("contracts")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
        .round()
        .max(1.0) as u32;
    let entry_credit = signal.get("estimated_credit").and_then(|v| v.as_f64());

    state
        .open_positions
        .entry(pending.position_id.clone())
        .or_insert_with(|| TrackedPosition {
            position_id: pending.position_id.clone(),
            account_hash: pending.account_hash.clone(),
            underlying,
            expiry,
            strategy,
            opened_at: Utc::now(),
            entry_credit,
            max_loss_usd: pending.reserved_risk_usd,
            contracts,
            entry_params: None,
            peak_profit_pct: None,
            entry_pop_pct: signal
                .pointer("/market_context/spread_pop_pct")
                .and_then(|v| v.as_f64()),
            entry_short_delta: signal
                .pointer("/market_context/short_delta")
                .and_then(|v| v.as_f64())
                .map(f64::abs),
        });
}

fn watchlist_for_scan(rules: &RulesConfig, state: &AgentState) -> Vec<String> {
    let mut wl = rules.watchlist.clone();
    if let Some(sig) = &state.redeploy_signal {
        if let Some(u) = &sig.underlying {
            let key = u.to_uppercase();
            if redeploy_cooldown_active(rules, sig) {
                wl.retain(|s| !s.eq_ignore_ascii_case(&key));
            } else {
                wl.retain(|s| !s.eq_ignore_ascii_case(&key));
                wl.insert(0, key);
            }
        }
    }
    wl
}

fn redeploy_cooldown_active(rules: &RulesConfig, sig: &RedeploySignal) -> bool {
    let cooldown = rules
        .exit_rules
        .thesis
        .redeploy_cooldown_minutes
        .unwrap_or(0);
    if cooldown == 0 {
        return false;
    }
    Utc::now().signed_duration_since(sig.at).num_minutes() < cooldown as i64
}

fn maybe_clear_stale_redeploy(state: &mut AgentState) {
    let Some(sig) = &state.redeploy_signal else {
        return;
    };
    if Utc::now().signed_duration_since(sig.at).num_hours() >= 4 {
        state.redeploy_signal = None;
    }
}

fn clear_redeploy_after_entry(state: &mut AgentState) {
    state.redeploy_signal = None;
}

async fn notify_at_open(
    telegram: Option<&TelegramNotifier>,
    playbook: Option<&Value>,
    open_position_count: usize,
) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let body = format_market_open_telegram(playbook, open_position_count);
    let _ = tg.send(&body).await;
}

async fn notify_overnight_alert(
    telegram: Option<&TelegramNotifier>,
    review: &super::llm::LlmReview,
    state: &mut AgentState,
) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let now = Utc::now();
    let _ = tg
        .send(&format_overnight_telegram(review))
        .await;
    record_llm_telegram_sent(state, review, now);
}

fn is_llm_urgent_for_overnight(review: &super::llm::LlmReview) -> bool {
    super::telegram_format::is_llm_urgent(review)
        || review
            .position_reviews
            .iter()
            .any(|p| p.urgency.eq_ignore_ascii_case("high"))
}

async fn fetch_option_market_status(market: &MarketDataApi) -> Result<(bool, Option<Value>)> {
    let hours = market.markets().hours("option", None).await?;
    let open = crate::market_hours::option_market_open_from_hours(&hours, chrono::Utc::now())
        .unwrap_or(false);
    Ok((open, Some(hours)))
}

fn resolve_llm_phase(
    rules: &RulesConfig,
    state: &AgentState,
    has_candidates: bool,
    has_positions: bool,
    at_open: bool,
    mechanical_alert: bool,
) -> Option<LlmPhase> {
    if !rules.llm.enabled {
        return None;
    }
    if !has_candidates && !has_positions {
        return None;
    }
    if at_open && !has_candidates && !mechanical_alert {
        return None;
    }
    if !should_run_llm_review(rules, state, has_positions) {
        return None;
    }
    if has_candidates {
        return Some(LlmPhase::Selection);
    }
    Some(LlmPhase::Monitor)
}

/// True when live marks show a mechanical exit threshold is near or breached.
fn mechanical_alert_from_monitored(monitored_positions: &[Value], dte_close: u32) -> bool {
    monitored_positions.iter().any(|pos| {
        if let Some(rules) = pos.get("mechanical_rules") {
            if rules
                .get("stop_triggered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return true;
            }
            if rules
                .get("profit_target_triggered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return true;
            }
        }
        pos.get("dte")
            .and_then(|v| v.as_i64())
            .is_some_and(|dte| dte <= dte_close as i64)
    })
}

fn any_entry_slots_available(rules: &RulesConfig, state: &AgentState) -> bool {
    let pending = state.pending_entry_count();
    for account in rules.enabled_accounts() {
        if rules.strategies.vertical.enabled
            && state.count_open_for_strategy(&account.hash, StrategyKind::Vertical) + pending
                < rules.entry_rules.vertical.max_open_positions
        {
            return true;
        }
        if rules.strategies.iron_condor.enabled
            && state.count_open_for_strategy(&account.hash, StrategyKind::IronCondor) + pending
                < rules.entry_rules.iron_condor.max_open_positions
        {
            return true;
        }
    }
    false
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
    trade_audio::speak_from_action(kind, detail);
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let Some(msg) = format_action_telegram(kind, detail) else {
        return;
    };
    let _ = tg.send(&msg).await;
}

async fn notify_llm(
    telegram: Option<&TelegramNotifier>,
    review: &super::llm::LlmReview,
    monitored: &[Value],
    state: &mut AgentState,
) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let now = Utc::now();
    let _ = tg
        .send(&format_llm_review_telegram(review, monitored))
        .await;
    record_llm_telegram_sent(state, review, now);
}

async fn evaluate_vertical_entry(
    market: &MarketDataApi,
    rules: &RulesConfig,
    underlying: &str,
    today: NaiveDate,
    state: &AgentState,
    account_hash: &str,
    spread_type: &str,
) -> Result<Option<Value>> {
    let entry = &rules.entry_rules.vertical;
    let open_count = state.count_open_for_strategy(account_hash, StrategyKind::Vertical);
    if open_count + state.pending_entry_count() >= entry.max_open_positions {
        return Ok(None);
    }

    let is_put = !spread_type.eq_ignore_ascii_case("call_credit");
    let contract_type = if is_put { "PUT" } else { "CALL" };
    let map_key = if is_put {
        "putExpDateMap"
    } else {
        "callExpDateMap"
    };

    let chain = market
        .chains()
        .get(&ChainQuery {
            symbol: underlying,
            contract_type: Some(contract_type),
            strike_count: Some(50),
            include_underlying_quote: Some(true),
            ..Default::default()
        })
        .await?;

    let (expiry, strike_map) =
        pick_expiry_map(&chain, map_key, entry.dte_min, entry.dte_max, today)?;
    let underlying_price = chain
        .pointer("/underlying/last")
        .or_else(|| chain.pointer("/underlyingPrice"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    if underlying_price <= 0.0 {
        return Ok(None);
    }

    let short_strike = pick_strike_by_delta(
        &strike_map,
        entry.short_delta_min,
        entry.short_delta_max,
        is_put,
    )
    .or_else(|| pick_otm_strike(&strike_map, underlying_price, 0.05, is_put).ok())
    .context("no suitable short strike")?;
    let long_strike = pick_wing_strike(&strike_map, short_strike, entry.max_width, is_put)?;
    let credit = estimate_spread_credit(&strike_map, short_strike, long_strike)?;
    if credit < entry.min_credit {
        return Ok(None);
    }
    let width = (short_strike - long_strike).abs();
    if !entry_quality_ok(&strike_map, short_strike, long_strike, width, credit, entry) {
        return Ok(None);
    }

    let market_context = vertical_entry_market_context(
        &chain,
        underlying,
        expiry,
        today,
        &strike_map,
        short_strike,
        long_strike,
        width,
        credit,
        entry.max_contracts_per_trade as f64,
        is_put,
    );

    let analytics = analytics_from_json(market_context.get("analytics").unwrap_or(&json!({})));
    if let Some(ref a) = analytics {
        if !entry_analytics_pass(entry, a) {
            return Ok(None);
        }
        if candidate_fails_thesis_gates(rules, a).is_some() {
            return Ok(None);
        }
    }

    let right = if is_put { 'P' } else { 'C' };
    let candidate_id = candidate_position_id(
        account_hash,
        underlying,
        &expiry.to_string(),
        StrategyKind::Vertical.as_str(),
        vec![
            (right, short_strike, "S"),
            (right, long_strike, "L"),
        ],
    );
    if state.open_positions.contains_key(&candidate_id)
        || state.has_pending_position(&candidate_id)
        || has_legacy_duplicate(state, account_hash, underlying, &expiry.to_string())
    {
        return Ok(None);
    }

    let resolved_type = if is_put { "put_credit" } else { "call_credit" };
    let params = VerticalParams {
        underlying: underlying.to_string(),
        expiry: expiry.to_string(),
        spread_type: resolved_type.to_string(),
        short_strike,
        long_strike,
        contracts: entry.max_contracts_per_trade as f64,
        limit_credit: Some(credit),
        limit_debit: None,
        duration: None,
        session: None,
    };

    Ok(Some(json!({
        "type": "entry",
        "strategy": "vertical",
        "account_hash": account_hash,
        "position_id": candidate_id,
        "params": params,
        "estimated_credit": credit,
        "market_context": market_context,
        "regime_preferred": resolved_type,
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
    if open_count + state.pending_entry_count() >= entry.max_open_positions {
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

    let (expiry, put_map) =
        pick_expiry_map(&chain, "putExpDateMap", entry.dte_min, entry.dte_max, today)?;
    let (_, call_map) = pick_expiry_map(
        &chain,
        "callExpDateMap",
        entry.dte_min,
        entry.dte_max,
        today,
    )?;

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
    let candidate_id = candidate_position_id(
        account_hash,
        underlying,
        &expiry.to_string(),
        StrategyKind::IronCondor.as_str(),
        vec![
            ('P', put_short, "S"),
            ('P', put_long, "L"),
            ('C', call_short, "S"),
            ('C', call_long, "L"),
        ],
    );
    if state.open_positions.contains_key(&candidate_id)
        || state.has_pending_position(&candidate_id)
        || has_legacy_duplicate(state, account_hash, underlying, &expiry.to_string())
    {
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
        "position_id": candidate_id,
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
fn pick_wing_strike(strike_map: &Value, short_strike: f64, width: f64, puts: bool) -> Result<f64> {
    let target = if puts {
        short_strike - width
    } else {
        short_strike + width
    };
    pick_nearest_strike(strike_map, target)
}

fn pick_nearest_strike(strike_map: &Value, target: f64) -> Result<f64> {
    let obj = strike_map.as_object().context("strike map not object")?;
    let candidates: Vec<f64> = obj.keys().filter_map(|k| k.parse::<f64>().ok()).collect();
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

/// Pick strike whose |delta| is closest to the middle of [delta_min, delta_max].
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
        let delta = contracts.as_array()?.first()?.get("delta")?.as_f64()?;
        // Puts have negative delta; calls positive — skip wrong side.
        if puts && delta > 0.0 {
            continue;
        }
        if !puts && delta < 0.0 {
            continue;
        }
        let abs_delta = delta.abs();
        if abs_delta < delta_min || abs_delta > delta_max {
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

fn entry_quality_ok(
    strike_map: &Value,
    short_strike: f64,
    long_strike: f64,
    width: f64,
    credit: f64,
    entry: &VerticalEntryRules,
) -> bool {
    if width <= f64::EPSILON || credit <= f64::EPSILON {
        return false;
    }
    let min_ctw = entry.min_credit_to_width_pct.unwrap_or(12.5);
    let credit_to_width_pct = (credit / width) * 100.0;
    if credit_to_width_pct < min_ctw {
        return false;
    }
    let short_quote_width = quote_width(strike_map, short_strike).unwrap_or(f64::INFINITY);
    let long_quote_width = quote_width(strike_map, long_strike).unwrap_or(f64::INFINITY);
    (short_quote_width + long_quote_width) <= credit * MAX_ENTRY_QUOTE_WIDTH_RATIO
}

fn quote_width(strike_map: &Value, strike: f64) -> Option<f64> {
    let bid = strike_quote_field(strike_map, strike, "bid").ok()?;
    let ask = strike_quote_field(strike_map, strike, "ask").ok()?;
    if bid < 0.0 || ask <= 0.0 || ask < bid {
        return None;
    }
    Some(ask - bid)
}

fn has_legacy_duplicate(
    state: &AgentState,
    account_hash: &str,
    underlying: &str,
    expiry: &str,
) -> bool {
    state
        .open_positions
        .values()
        .any(|p| p.account_hash == account_hash && p.underlying == underlying && p.expiry == expiry)
}

fn candidate_id_from_params(account_hash: &str, kind: StrategyKind, params: &Value) -> String {
    let underlying = params
        .get("underlying")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let expiry = params.get("expiry").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        StrategyKind::Vertical => {
            let spread_type = params
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("put_credit");
            let put_call = if spread_type.starts_with("call") {
                'C'
            } else {
                'P'
            };
            let short = params
                .get("short_strike")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let long = params
                .get("long_strike")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            candidate_position_id(
                account_hash,
                underlying,
                expiry,
                kind.as_str(),
                vec![(put_call, short, "S"), (put_call, long, "L")],
            )
        }
        StrategyKind::IronCondor => candidate_position_id(
            account_hash,
            underlying,
            expiry,
            kind.as_str(),
            vec![
                (
                    'P',
                    params
                        .get("put_short")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    "S",
                ),
                (
                    'P',
                    params
                        .get("put_long")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    "L",
                ),
                (
                    'C',
                    params
                        .get("call_short")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    "S",
                ),
                (
                    'C',
                    params
                        .get("call_long")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    "L",
                ),
            ],
        ),
    }
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

    if rules.risk.max_trades_per_day > 0
        && state.trades_capacity_used() >= rules.risk.max_trades_per_day
    {
        return Ok(Some(json!({
            "fill_status": "SKIPPED",
            "reason": "max_trades_per_day reached or reserved by pending entries",
            "trades_today": state.trades_today,
            "pending_entries": state.pending_entry_count(),
            "max_trades_per_day": rules.risk.max_trades_per_day,
            "signal": signal,
        })));
    }

    let params = signal
        .get("params")
        .cloned()
        .context("signal missing params")?;
    let margin = crate::options::validate::estimate_order_margin(&json!({}), kind, &params)?;
    if margin > rules.risk.max_risk_per_trade_usd {
        return Ok(Some(json!({
            "fill_status": "SKIPPED",
            "reason": "max_risk_per_trade_usd exceeded",
            "required_margin_usd": margin,
            "max_risk_per_trade_usd": rules.risk.max_risk_per_trade_usd,
            "signal": signal,
        })));
    }
    let reserved = state.reserved_risk_usd();
    if reserved + margin > rules.risk.max_portfolio_risk_usd {
        return Ok(Some(json!({
            "fill_status": "SKIPPED",
            "reason": "max_portfolio_risk_usd exceeded",
            "reserved_risk_usd": reserved,
            "new_order_margin_usd": margin,
            "projected_reserved_risk_usd": reserved + margin,
            "max_portfolio_risk_usd": rules.risk.max_portfolio_risk_usd,
            "signal": signal,
        })));
    }
    let position_id = signal
        .get("position_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| candidate_id_from_params(account_hash, kind, &params));
    if state.open_positions.contains_key(&position_id) || state.has_pending_position(&position_id) {
        return Ok(Some(json!({
            "fill_status": "SKIPPED",
            "reason": "position already open or pending",
            "position_id": position_id,
            "signal": signal,
        })));
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
        if let Some(order_id) = order_id.as_ref() {
            state.add_pending_order(PendingOrder {
                order_id: order_id.clone(),
                account_hash: account_hash.to_string(),
                action: PendingOrderAction::Entry,
                position_id: position_id.clone(),
                reserved_risk_usd: margin,
                submitted_at: Utc::now(),
                last_status: Some(fill_status.to_string()),
                detail: Some(json!({
                    "signal": signal,
                    "place": place.clone(),
                    "wait": wait_result.as_ref().map(wait_result_json),
                })),
            });
        }
        let detail = json!({
            "signal": signal,
            "place": place,
            "wait": wait_result.as_ref().map(wait_result_json),
            "fill_status": fill_status,
            "position_id": position_id,
            "reserved_risk_usd": margin,
            "note": "Limit order working; risk and trade capacity reserved until terminal status",
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
    let order_contracts = params
        .get("contracts")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
        .round()
        .max(1.0) as u32;
    let new_credit = signal.get("estimated_credit").and_then(|v| v.as_f64());

    if let Some(existing) = state.open_positions.get_mut(&position_id) {
        let prev_contracts = existing.contracts.max(1);
        existing.contracts = prev_contracts + order_contracts;
        existing.max_loss_usd += margin;
        if let Some(credit) = new_credit {
            let blended = existing.entry_credit.unwrap_or(credit) * prev_contracts as f64
                + credit * order_contracts as f64;
            existing.entry_credit = Some(blended / existing.contracts as f64);
        }
    } else {
        state.open_positions.insert(
            position_id.clone(),
            TrackedPosition {
                position_id: position_id.clone(),
                account_hash: account_hash.to_string(),
                underlying,
                expiry,
                strategy: kind.as_str().to_string(),
                opened_at: Utc::now(),
                entry_credit: new_credit,
                max_loss_usd: margin,
                contracts: order_contracts,
                entry_params: None,
                peak_profit_pct: None,
                entry_pop_pct: signal
                    .pointer("/market_context/spread_pop_pct")
                    .and_then(|v| v.as_f64()),
                entry_short_delta: signal
                    .pointer("/market_context/short_delta")
                    .and_then(|v| v.as_f64())
                    .map(f64::abs),
            },
        );
    }
    clear_redeploy_after_entry(state);
    state.record_action("entry", signal.clone());

    Ok(Some(json!({
        "entry": place,
        "signal": signal,
        "wait": wait_result.as_ref().map(wait_result_json),
        "position_id": position_id,
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

    let position_id = stable_position_key(account_hash, group);
    if state.has_pending_position(&position_id) {
        return Ok(json!({
            "fill_status": "SKIPPED",
            "reason": "exit already pending",
            "position_id": position_id,
            "signal": signal,
        }));
    }

    let close_limit = close_limit_from_signal(signal)
        .or_else(|| close_limit_from_group_mark(group))
        .context("could not derive close limit price for spread exit")?;
    let order = build_close_order_for_group_with_limit(group, Some(close_limit))?;
    runtime.safety.validate_order(&order, None, None)?;
    let place = execute_trading_order(runtime, trader, account_hash, &order).await?;

    let mut wait_json = None;
    let mut fill_status = "ACCEPTED".to_string();
    let order_id = place
        .get("order_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if rules.execution.wait_for_fill {
        if let Some(order_id) = order_id.as_ref() {
            let wait = wait_for_order(
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
            match wait {
                Ok(wait) => {
                    fill_status = wait
                        .final_status
                        .as_deref()
                        .unwrap_or("UNKNOWN")
                        .to_string();
                    wait_json = Some(wait_result_json(&wait));
                }
                Err(e) => {
                    fill_status = "WAIT_ERROR".into();
                    wait_json = Some(json!({ "error": e.to_string() }));
                }
            }
        }
    }

    let detail = json!({
        "exit": place,
        "signal": signal,
        "position_id": position_id,
        "limit_price": close_limit,
        "wait": wait_json,
        "fill_status": fill_status.clone(),
    });

    if fill_status == "FILLED" || !rules.execution.wait_for_fill {
        state.open_positions.remove(&position_id);
        state.open_positions.remove(&group.id);
        state.record_action("exit", signal.clone());
    } else {
        if let Some(order_id) = order_id {
            state.add_pending_order(PendingOrder {
                order_id,
                account_hash: account_hash.to_string(),
                action: PendingOrderAction::Exit,
                position_id,
                reserved_risk_usd: 0.0,
                submitted_at: Utc::now(),
                last_status: Some(fill_status),
                detail: Some(detail.clone()),
            });
        }
        state.record_action("exit_working_position_kept", detail.clone());
    }

    Ok(detail)
}

fn close_limit_from_signal(signal: &Value) -> Option<f64> {
    let debit = signal
        .pointer("/mark/debit_to_close")
        .and_then(|v| v.as_f64())?;
    Some((debit + EXIT_LIMIT_SLIPPAGE).max(0.01))
}

fn close_limit_from_group_mark(group: &crate::options::OptionPositionGroup) -> Option<f64> {
    let contracts = crate::options::spread_contract_count(group) as f64;
    if contracts <= 0.0 {
        return None;
    }
    Some((group.net_market_value.abs() / contracts / 100.0 + EXIT_LIMIT_SLIPPAGE).max(0.01))
}

#[cfg(test)]
mod llm_schedule_tests {
    use super::*;
    use crate::agent::llm::LlmReview;
    use crate::agent::state::{AgentState, TrackedPosition};
    use crate::rules::{
        AccountType, EntryRules, ExecutionConfig, ExitRules, LlmConfig, NotifyConfig, RiskConfig,
        RulesAccount, RulesConfig, ScheduleConfig, StrategiesToggle, StrategyEnabled,
        VerticalEntryRules,
    };

    fn test_rules(max_open: u32) -> RulesConfig {
        RulesConfig {
            version: 1,
            agent_id: "test".into(),
            accounts: vec![RulesAccount {
                hash: "ACC".into(),
                label: Some("test".into()),
                r#type: AccountType::Ira,
                enabled: true,
            }],
            schedule: ScheduleConfig {
                tick_interval_seconds: 120,
                ..Default::default()
            },
            strategies: StrategiesToggle {
                vertical: StrategyEnabled { enabled: true },
                iron_condor: StrategyEnabled { enabled: false },
            },
            watchlist: vec!["IWM".into()],
            entry_rules: EntryRules {
                vertical: VerticalEntryRules {
                    max_open_positions: max_open,
                    ..Default::default()
                },
                ..Default::default()
            },
            exit_rules: ExitRules {
                dte_close: 21,
                ..Default::default()
            },
            risk: RiskConfig::default(),
            regime: Default::default(),
            execution: ExecutionConfig::default(),
            llm: LlmConfig {
                enabled: true,
                review_every_ticks: 15,
                monitor_review_every_ticks: Some(30),
                ..Default::default()
            },
            notify: NotifyConfig::default(),
            simulation: None,
        }
    }

    fn state_with_open_position() -> AgentState {
        let mut state = AgentState::default();
        state.open_positions.insert(
            "pos".into(),
            TrackedPosition {
                position_id: "pos".into(),
                account_hash: "ACC".into(),
                underlying: "IWM".into(),
                expiry: "2026-07-31".into(),
                strategy: "vertical".into(),
                opened_at: chrono::Utc::now(),
                entry_credit: Some(0.30),
                max_loss_usd: 170.0,
                contracts: 1,
                entry_params: None,
                peak_profit_pct: None,
                entry_pop_pct: None,
                entry_short_delta: None,
            },
        );
        state
    }

    #[test]
    fn selection_llm_is_throttled_when_candidates_exist() {
        let rules = test_rules(2);
        let mut state = state_with_open_position();
        state.regular_tick_count = 100;
        state.last_llm_review_tick = Some(95);

        assert!(resolve_llm_phase(&rules, &state, true, true, false, false).is_none());

        state.regular_tick_count = 125;
        assert!(matches!(
            resolve_llm_phase(&rules, &state, true, true, false, false),
            Some(LlmPhase::Selection)
        ));
    }

    #[test]
    fn monitor_runs_when_no_candidates_and_due() {
        let rules = test_rules(2);
        let mut state = state_with_open_position();
        state.regular_tick_count = 125;
        state.last_llm_review_tick = Some(95);

        assert!(matches!(
            resolve_llm_phase(&rules, &state, false, true, false, false),
            Some(LlmPhase::Monitor)
        ));
    }

    #[test]
    fn monitor_skipped_at_open_without_candidates_or_mechanical_alert() {
        let rules = test_rules(2);
        let mut state = state_with_open_position();
        state.regular_tick_count = 125;
        state.last_llm_review_tick = Some(95);

        assert!(resolve_llm_phase(&rules, &state, false, true, true, false).is_none());
    }

    #[test]
    fn monitor_runs_at_open_when_mechanical_alert() {
        let rules = test_rules(2);
        let mut state = state_with_open_position();
        state.regular_tick_count = 125;
        state.last_llm_review_tick = Some(95);

        assert!(matches!(
            resolve_llm_phase(&rules, &state, false, true, true, true),
            Some(LlmPhase::Monitor)
        ));
    }

    #[test]
    fn mechanical_alert_detects_stop_triggered() {
        let monitored = vec![json!({
            "mechanical_rules": { "stop_triggered": true }
        })];
        assert!(mechanical_alert_from_monitored(&monitored, 21));
    }

    #[test]
    fn entry_scan_skipped_when_all_accounts_at_capacity() {
        let rules = test_rules(1);
        let state = state_with_open_position();
        assert!(!any_entry_slots_available(&rules, &state));
    }

    #[test]
    fn redeploy_cooldown_removes_underlying_from_scan() {
        let mut rules = test_rules(2);
        rules.exit_rules.thesis.redeploy_cooldown_minutes = Some(120);
        let mut state = AgentState::default();
        state.redeploy_signal = Some(RedeploySignal {
            at: Utc::now(),
            reason: "thesis_near_strike".into(),
            underlying: Some("IWM".into()),
        });
        let wl = watchlist_for_scan(&rules, &state);
        assert!(!wl.iter().any(|s| s.eq_ignore_ascii_case("IWM")));

        state.redeploy_signal = Some(RedeploySignal {
            at: Utc::now() - chrono::Duration::minutes(121),
            reason: "thesis_near_strike".into(),
            underlying: Some("IWM".into()),
        });
        let wl = watchlist_for_scan(&rules, &state);
        assert_eq!(wl.first().map(String::as_str), Some("IWM"));
    }

    #[test]
    fn selection_telegram_only_on_proceed_or_urgent_close() {
        use crate::agent::telegram_format::is_llm_urgent;

        let defer = LlmReview {
            phase: "selection".into(),
            model: "test".into(),
            used_web: false,
            raw: serde_json::json!({}),
            market_commentary: "ok".into(),
            web_insights: vec![],
            position_reviews: vec![],
            entry_recommendation: "defer".into(),
            entry_reasoning: "wait".into(),
            risk_alerts: vec!["noise".into()],
        };
        assert!(!is_llm_urgent(&defer));

        let proceed = LlmReview {
            entry_recommendation: "proceed".into(),
            ..defer.clone()
        };
        assert!(is_llm_urgent(&proceed));
    }
}
