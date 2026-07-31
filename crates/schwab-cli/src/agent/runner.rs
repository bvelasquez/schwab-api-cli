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
use crate::rules::{
    EntryScanMode, LlmPhase, RulesConfig, VerticalEntryRules, WatchlistItemConfig, WatchlistRole,
};
use crate::safety::{execute_trading_order, require_trading_approval};
use crate::trade_audio::{self, TradeAudioEvent};

use super::exits::{
    candidate_fails_thesis_gates, evaluate_position_monitor, exit_signal_json_for_account,
    find_tracked_position, option_group_from_tracked, reconcile_open_positions, stable_position_key,
    ExitEvaluation,
};
use super::journal;
use super::llm::OpenRouterClient;
use super::market_context::{
    iron_condor_entry_market_context, market_context_summary_for_llm, vertical_entry_market_context,
};
use super::regime::{detect_options_regime, OptionsRegimeSnapshot};
use super::risk::{drawdown_to_json, record_live_realized_pnl, update_drawdown};
use super::roll::{
    original_width_from_params, roll_biased_entry_rules, roll_eligible, roll_money_ok, roll_net,
    spread_type_from_tracked, RollEligibility,
};
use super::spread_analytics::{analytics_from_json, entry_analytics_pass, passes_min_iv_rv_ratio};
use super::volatility::{fetch_realized_vol_pct, iv_rv_ratio};
use super::paths::{active_state_path, load_agent_state, load_sim_agent_state};
use super::protective;
use super::resilience;
use super::sim::{ensure_ledger, record_sim_entry, record_sim_exit};
use super::schedule::{self, AgentSession};
use super::state::{
    backfill_entry_baselines_from_actions, candidate_fingerprint, entry_attempt_cooldown_active,
    entry_proceed_cache_valid, is_stop_loss_exit_reason, is_thesis_exit_reason,
    record_entry_attempt, record_stop_loss_exit, save_state, stop_loss_re_entry_blocked,
    update_peak_profit_pct, AgentState, EntryProceedCache, PendingOrder, PendingOrderAction,
    RedeploySignal, TrackedPosition,
};
use super::telegram_format::{
    format_action_telegram, format_llm_review_telegram, format_market_open_telegram,
    format_overnight_telegram, record_llm_telegram_sent, should_send_llm_telegram,
};
use crate::ui::agent_health::SharedAgentHealth;

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
    #[serde(default)]
    pub monitoring: Value,
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
    let _ = backfill_entry_baselines_from_actions(&mut state);

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
    let mut auth_notified = false;

    loop {
        // Soft re-auth probe so a fresh `schwab auth login` is picked up mid-run.
        if let Err(err) = trader.client().oauth().ensure_access_token().await {
            tracing::debug!("access token probe: {err}");
        }

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
                if consecutive_errors > 0 {
                    let msg = format!(
                        "agent recovered after {consecutive_errors} failure(s)"
                    );
                    let _ = super::paths::append_agent_log(rules_path, &msg);
                    if let Some(tg) = telegram.as_ref() {
                        if tg.wants_actions() {
                            let _ = tg
                                .send(&format!(
                                    "schwab [{}]\n✓ AGENT RECOVERED\nafter {consecutive_errors} failure(s) — exits armed again",
                                    rules.agent_id
                                ))
                                .await;
                        }
                    }
                }
                consecutive_errors = 0;
                auth_notified = false;
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
                // 3-tier classification (Recoverable / AuthFatal / Unexpected) so a real code
                // bug and a transient network blip get differentiated backoff and alerting,
                // instead of both falling into a single generic "not auth" bucket.
                let class = resilience::classify_agent_error(&e);
                let class_label = resilience::class_label(class);

                if class == resilience::AgentErrorClass::AuthFatal {
                    let msg = "agent degraded: Schwab login required (refresh token invalid). Run: schwab auth login — retrying";
                    if last_logged_error.as_deref() != Some(msg) {
                        let _ = super::paths::append_agent_log(rules_path, msg);
                        last_logged_error = Some(msg.to_string());
                    }
                    if let Some(h) = watch_health.as_ref() {
                        if let Ok(mut g) = h.lock() {
                            g.record_error(msg);
                        }
                    }
                    if !auth_notified {
                        notify_auth_required(telegram.as_ref(), msg).await;
                        auth_notified = true;
                    }
                    if once {
                        return Err(e);
                    }
                    let backoff = resilience::backoff_seconds(class, consecutive_errors);
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    continue;
                }

                let msg = format!("tick error ({class_label}, #{consecutive_errors}): {err_str}");
                if last_logged_error.as_deref() != Some(msg.as_str()) {
                    let _ = super::paths::append_agent_log(rules_path, &msg);
                    last_logged_error = Some(msg.clone());
                }
                if let Some(h) = watch_health.as_ref() {
                    if let Ok(mut g) = h.lock() {
                        g.record_error(&msg);
                    }
                }
                // Avoid Telegram spam: alert on the first failure, then every 10th, so both
                // Recoverable and Unexpected errors surface without flooding chat.
                if consecutive_errors == 1 || consecutive_errors % 10 == 0 {
                    if let Some(tg) = telegram.as_ref() {
                        if tg.wants_actions() {
                            let short: String = err_str.chars().take(280).collect();
                            let _ = tg
                                .send(&format!(
                                    "schwab [{}]\n⚠ AGENT DEGRADED ({class_label}) ×{consecutive_errors}\n{short}\nAgent staying up; backing off and retrying",
                                    rules.agent_id
                                ))
                                .await;
                        }
                    }
                }
                if once {
                    return Err(e);
                }
                let backoff = resilience::backoff_seconds(class, consecutive_errors);
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
        monitoring: Value::Null,
    };

    if runtime.simulate {
        let _ = ensure_ledger(state, rules);
        result
            .skipped
            .push("simulate — using paper state (no Schwab reconcile)".into());
    } else {
        reconcile_open_positions(trader, state, rules).await?;
        poll_pending_orders(trader, state, rules, &mut result).await?;
        let protective_summary =
            protective::reconcile_protective_orders(runtime, trader, rules, state).await;
        result.monitoring = json!({
            "unprotected_count": protective_summary.unprotected_count,
            "protective_orders_placed_this_tick": protective_summary.placed,
            "protective_orders_failed_this_tick": protective_summary.failed,
        });
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
    let active_blocked_dates = rules.risk.active_blocked_date_labels(today);
    let blocked_dates_active = !active_blocked_dates.is_empty();
    let calendar_or_manual_block = blocked_events_active || blocked_dates_active;

    if let Some(obj) = result.monitoring.as_object_mut() {
        obj.insert(
            "blocked_dates_active".into(),
            json!(active_blocked_dates),
        );
        obj.insert(
            "blocked_events_active".into(),
            json!(blocked_events_active),
        );
    } else {
        result.monitoring = json!({
            "blocked_dates_active": active_blocked_dates,
            "blocked_events_active": blocked_events_active,
        });
    }

    let mut regime_snap: Option<OptionsRegimeSnapshot> = None;
    if rules.regime.enabled && !entries_paused && !calendar_or_manual_block {
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
            let preferred_for_exits = regime_snap.as_ref().map(|s| s.preferred_strategy.as_str());
            let monitor =
                evaluate_position_monitor(market, &group, rules, today, Some(&tracked), preferred_for_exits).await?;
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

                    let mut handled_as_roll = false;
                    if is_stop_loss_exit_reason(&eval.reason) && !runtime.dry_run {
                        match try_defensive_roll(
                            runtime,
                            trader,
                            market,
                            rules_path,
                            rules,
                            state,
                            today,
                            &tracked.account_hash,
                            &position_id,
                            &tracked,
                            &group,
                            &eval,
                            monitor.analytics.as_ref().and_then(|a| a.short_otm_pct),
                            true,
                            telegram,
                        )
                        .await
                        {
                            Ok(DefensiveRollOutcome::Success(detail)) => {
                                handled_as_roll = true;
                                result.signals.push(detail.clone());
                                result.actions.push(detail.clone());
                                result.skipped.push(format!("rolled {position_id}"));
                                notify_action(telegram, "DEFENSIVE ROLL", &detail).await;
                            }
                            Ok(DefensiveRollOutcome::NotAttempted { reason }) => {
                                result.skipped.push(format!(
                                    "roll skipped {position_id}: {reason} — stop_loss"
                                ));
                            }
                            Ok(DefensiveRollOutcome::StopCompleted { detail, reason }) => {
                                handled_as_roll = true;
                                record_stop_loss_exit(state, &tracked.underlying);
                                state.redeploy_signal = None;
                                state.entry_proceed_cache = None;
                                result.skipped.push(format!(
                                    "roll aborted after close {position_id}: {reason} — stop_loss"
                                ));
                                if let Some(d) = detail {
                                    result.actions.push(d);
                                }
                                let exit =
                                    exit_signal_json_for_account(&tracked.account_hash, &group, &eval);
                                result.signals.push(exit.clone());
                                notify_action(telegram, "SIM EXIT", &exit).await;
                            }
                            Err(err) => {
                                result.skipped.push(format!(
                                    "roll error {position_id}: {err:#} — stop_loss"
                                ));
                            }
                        }
                    }

                    if !handled_as_roll {
                        if is_stop_loss_exit_reason(&eval.reason) {
                            record_stop_loss_exit(state, &tracked.underlying);
                            state.redeploy_signal = None;
                            state.entry_proceed_cache = None;
                        }
                        let exit =
                            exit_signal_json_for_account(&tracked.account_hash, &group, &eval);
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
        }
    } else {
        for account in rules.enabled_accounts() {
            let legs = list_option_positions(trader, Some(&account.hash)).await?;
            let groups = group_option_legs(&legs);
            for group in &groups {
                let tracked_owned = find_tracked_position(state, &account.hash, group).cloned();
                let preferred_for_exits = regime_snap.as_ref().map(|s| s.preferred_strategy.as_str());
                let monitor = evaluate_position_monitor(
                    market,
                    group,
                    rules,
                    today,
                    tracked_owned.as_ref(),
                    preferred_for_exits,
                )
                .await?;
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

                    let mut handled_as_roll = false;
                    if is_stop_loss_exit_reason(&eval.reason)
                        && !runtime.dry_run
                        && tracked_owned.is_some()
                        && !state.has_pending_position(&position_id)
                    {
                        let tracked_ref = tracked_owned.as_ref().expect("checked is_some");
                        match try_defensive_roll(
                            runtime,
                            trader,
                            market,
                            rules_path,
                            rules,
                            state,
                            today,
                            &account.hash,
                            &position_id,
                            tracked_ref,
                            group,
                            &eval,
                            monitor.analytics.as_ref().and_then(|a| a.short_otm_pct),
                            false,
                            telegram,
                        )
                        .await
                        {
                            Ok(DefensiveRollOutcome::Success(detail)) => {
                                handled_as_roll = true;
                                result.signals.push(detail.clone());
                                result.actions.push(detail.clone());
                                result.skipped.push(format!("rolled {position_id}"));
                                notify_action(telegram, "DEFENSIVE ROLL", &detail).await;
                            }
                            Ok(DefensiveRollOutcome::NotAttempted { reason }) => {
                                result.skipped.push(format!(
                                    "roll skipped {position_id}: {reason} — stop_loss"
                                ));
                            }
                            Ok(DefensiveRollOutcome::StopCompleted { detail, reason }) => {
                                handled_as_roll = true;
                                record_stop_loss_exit(state, &group.underlying);
                                state.redeploy_signal = None;
                                state.entry_proceed_cache = None;
                                result.skipped.push(format!(
                                    "roll aborted after close {position_id}: {reason} — stop_loss"
                                ));
                                if let Some(d) = detail {
                                    result.actions.push(d);
                                }
                                let exit =
                                    exit_signal_json_for_account(&account.hash, group, &eval);
                                result.signals.push(exit.clone());
                                notify_action(telegram, "EXIT", &exit).await;
                            }
                            Err(err) => {
                                result.skipped.push(format!(
                                    "roll error {position_id}: {err:#} — stop_loss"
                                ));
                            }
                        }
                    }

                    if !handled_as_roll {
                        if is_stop_loss_exit_reason(&eval.reason) {
                            record_stop_loss_exit(state, &group.underlying);
                            state.redeploy_signal = None;
                            state.entry_proceed_cache = None;
                        }
                        let exit = exit_signal_json_for_account(&account.hash, group, &eval);
                        result.signals.push(exit.clone());
                        if state.has_pending_position(&position_id) {
                            result
                                .skipped
                                .push(format!("exit already pending for {position_id}"));
                        } else if !runtime.dry_run {
                            if let Ok(action) = execute_exit(
                                runtime,
                                trader,
                                &account.hash,
                                rules_path,
                                rules,
                                group,
                                &exit,
                                state,
                                llm_client,
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
        }
    }

    // Entry scan (signals collected; execution after LLM review)
    let halted_before = state.trading_halted_reason.clone();
    let drawdown = update_drawdown(state, rules, &result.monitored_positions);
    if let Some(obj) = result.monitoring.as_object_mut() {
        obj.insert("drawdown".into(), drawdown_to_json(&drawdown));
        obj.insert(
            "trading_halted_reason".into(),
            json!(state.trading_halted_reason),
        );
    } else {
        result.monitoring = json!({
            "drawdown": drawdown_to_json(&drawdown),
            "trading_halted_reason": state.trading_halted_reason,
        });
    }
    if state.trading_halted_reason != halted_before {
        if let Some(reason) = &state.trading_halted_reason {
            notify_trading_halted(telegram, rules, reason).await;
        } else if halted_before.is_some() {
            notify_trading_recovered(telegram, rules).await;
        }
    }

    let mut pending_entries: Vec<(String, StrategyKind, Value)> = Vec::new();
    let regime_pause = regime_snap.as_ref().is_some_and(|s| s.pause_entries);
    let preferred = regime_snap
        .as_ref()
        .map(|s| s.preferred_strategy.as_str())
        .unwrap_or("put_credit");
    let trading_halted = state.trading_halted_reason.is_some();

    if entries_paused {
        result.skipped.push(format!(
            "new entries paused — max_trades_per_day ({}) reached or reserved by pending entries \
             (soft churn cap; hard gates are max_open_positions + portfolio/trade risk)",
            rules.risk.max_trades_per_day
        ));
    } else if trading_halted {
        result.skipped.push(format!(
            "new entries paused — {}",
            state
                .trading_halted_reason
                .as_deref()
                .unwrap_or("trading halted")
        ));
    } else if blocked_events_active {
        result.skipped.push(format!(
            "new entries paused — blocked_events active: {}",
            rules.risk.blocked_events.join(", ")
        ));
    } else if blocked_dates_active {
        result.skipped.push(format!(
            "new entries paused — blocked_dates active: {}",
            active_blocked_dates.join(", ")
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
            (
                true,
                true,
                rules.entry_rules.vertical.r#type.as_str(),
            )
        };

        for account in rules.enabled_accounts() {
            match scan_entries_for_account(
                market,
                rules,
                state,
                &account.hash,
                today,
                &scan_watchlist,
                want_vertical,
                want_condor,
                vertical_type,
            )
            .await
            {
                Ok(found) => {
                    for skip in found.skipped {
                        result.skipped.push(skip);
                    }
                    pending_entries.extend(found.entries);
                }
                Err(e) => result.skipped.push(format!("entry scan {}: {e:#}", account.hash)),
            }
        }

        // Chop prefers iron_condor; if none qualify, allow put_credit so the sleeve
        // is not stuck flat for days when IC credit/delta is unavailable.
        if want_condor
            && !want_vertical
            && pending_entries.is_empty()
            && rules.strategies.vertical.enabled
        {
            result.skipped.push(
                "iron_condor preferred but none qualified — falling back to put_credit scan"
                    .into(),
            );
            for account in rules.enabled_accounts() {
                match scan_entries_for_account(
                    market,
                    rules,
                    state,
                    &account.hash,
                    today,
                    &scan_watchlist,
                    true,
                    false,
                    "put_credit",
                )
                .await
                {
                    Ok(found) => {
                        for skip in found.skipped {
                            result.skipped.push(skip);
                        }
                        pending_entries.extend(found.entries);
                    }
                    Err(e) => {
                        result
                            .skipped
                            .push(format!("put_credit fallback scan {}: {e:#}", account.hash))
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
                "watchlist": rules.watchlist_items(),
                "entry_policy": {
                    "mode": format!("{:?}", rules.entry_policy.mode),
                    "fallback_only_after_primary_exhausted": rules.entry_policy.fallback_only_after_primary_exhausted,
                },
                "risk": {
                    "max_trades_per_day": rules.risk.max_trades_per_day,
                    "trades_today": state.trades_today,
                    "max_risk_per_trade_usd": rules.risk.max_risk_per_trade_usd,
                    "max_portfolio_risk_usd": rules.risk.max_portfolio_risk_usd,
                    "blocked_dates_active": active_blocked_dates,
                    "blocked_events": rules.risk.blocked_events,
                },
            });

            match client.review(&rules.llm, phase, &context, use_web).await {
                Ok(review) => {
                    let mut review_json = review.to_json();
                    result.llm_review = Some(review_json.clone());
                    state.last_llm_review_tick = Some(state.regular_tick_count);
                    state.llm_review_count += 1;
                    state.last_llm_summary = Some(review_json.clone());

                    if rules.llm.veto_entries
                        && matches!(phase, LlmPhase::Selection)
                        && review.should_veto_entries()
                    {
                        let fingerprints: Vec<String> = pending_entries
                            .iter()
                            .filter_map(|(_, _, s)| candidate_fingerprint(s))
                            .collect();
                        let decision = crate::agent::scorecard::record_selection_decision(
                            rules_path,
                            runtime.simulate,
                            state,
                            &review,
                            fingerprints.clone(),
                        );
                        review_json["effective_action"] = json!(decision.effective_action);
                        review_json["veto_honored"] = json!(decision.honored);
                        state.record_action("llm_review", review_json.clone());
                        state.last_llm_summary = Some(review_json.clone());
                        result.llm_review = Some(review_json.clone());
                        if decision.honored {
                            llm_veto_entries = true;
                            state.entry_proceed_cache = None;
                            result.skipped.push(format!(
                                "LLM veto entries (catalyst): {}",
                                review.entry_reasoning
                            ));
                        } else {
                            // Fail-open: ignore calendar/math/vague defer.
                            result.skipped.push(format!(
                                "LLM defer ignored (fail-open; category={}): {}",
                                review.veto_category, review.entry_reasoning
                            ));
                            state.entry_proceed_cache = Some(EntryProceedCache {
                                at: Utc::now(),
                                candidate_fingerprints: fingerprints,
                            });
                        }
                    } else if matches!(phase, LlmPhase::Selection) {
                        let fingerprints: Vec<String> = pending_entries
                            .iter()
                            .filter_map(|(_, _, s)| candidate_fingerprint(s))
                            .collect();
                        let decision = crate::agent::scorecard::record_selection_decision(
                            rules_path,
                            runtime.simulate,
                            state,
                            &review,
                            fingerprints.clone(),
                        );
                        review_json["effective_action"] = json!(decision.effective_action);
                        review_json["veto_honored"] = json!(decision.honored);
                        state.record_action("llm_review", review_json.clone());
                        state.last_llm_summary = Some(review_json.clone());
                        result.llm_review = Some(review_json.clone());
                        if review.entry_recommendation.eq_ignore_ascii_case("proceed") {
                            state.entry_proceed_cache = Some(EntryProceedCache {
                                at: Utc::now(),
                                candidate_fingerprints: fingerprints,
                            });
                        } else {
                            state.entry_proceed_cache = None;
                        }
                    } else {
                        state.record_action("llm_review", review_json.clone());
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
                        state.entry_proceed_cache = None;
                        result
                            .skipped
                            .push("LLM selection failed closed — entries deferred".into());
                    }
                }
            }
        }
    } else if entry_execution_requires_llm(rules) && has_candidates {
        llm_veto_entries = true;
        result
            .skipped
            .push("LLM selection unavailable — entries deferred".into());
    }

    let mut entries_blocked = llm_veto_entries;
    if !entries_blocked && has_candidates && entry_execution_requires_llm(rules) {
        let fingerprints: Vec<String> = pending_entries
            .iter()
            .filter_map(|(_, _, s)| candidate_fingerprint(s))
            .collect();
        let cache_ok = state
            .entry_proceed_cache
            .as_ref()
            .is_some_and(|cache| entry_proceed_cache_valid(&rules.entry_policy, cache, &fingerprints));
        if !cache_ok {
            entries_blocked = true;
            result.skipped.push(
                "entry deferred — awaiting LLM proceed for current candidate (see proceed_cache_minutes)"
                    .into(),
            );
        }
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
                if let Ok(action) = execute_exit(
                    runtime,
                    trader,
                    &account.hash,
                    rules_path,
                    rules,
                    group,
                    &exit,
                    state,
                    llm_client,
                )
                .await
                {
                    result.actions.push(action);
                    notify_action(telegram, "LLM EXIT", &exit).await;
                }
            }
        }
    }

    // Execute pending entries unless LLM vetoed (dry-run never executes)
    if !entries_blocked && !runtime.dry_run {
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
                    if label == "SKIPPED" {
                        result.skipped.push(format!(
                            "entry skipped: {} ({})",
                            a.get("reason").and_then(|v| v.as_str()).unwrap_or("unknown"),
                            a.get("position_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                        ));
                    }
                    match label {
                        "FILLED" => notify_action(telegram, "ENTRY FILLED", &a).await,
                        "WORKING" | "ACCEPTED" | "PENDING_ACTIVATION" | "QUEUED" => {
                            notify_action(telegram, "ORDER WORKING (limit)", &a).await
                        }
                        "SKIPPED" => {}
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
        "watchlist": rules.watchlist_items(),
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
                    if let Some(tracked) = state
                        .open_positions
                        .get(&pending_order.position_id)
                        .cloned()
                    {
                        let debit = pending_order
                            .detail
                            .as_ref()
                            .and_then(|d| d.pointer("/signal/mark/debit_to_close"))
                            .or_else(|| {
                                pending_order
                                    .detail
                                    .as_ref()
                                    .and_then(|d| d.get("limit_price"))
                            })
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        record_live_realized_pnl(state, &tracked, debit);
                    }
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

    let llm_decision_id = state
        .last_llm_entry_decision
        .as_ref()
        .filter(|d| {
            d.effective_action.starts_with("proceed")
                && d.candidate_fingerprints
                    .iter()
                    .any(|fp| fp == &pending.position_id)
        })
        .map(|d| d.decision_id.clone());

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
            entry_chain_iv_pct: signal
                .pointer("/market_context/chain_iv")
                .and_then(|v| v.as_f64())
                .or_else(|| {
                    signal
                        .pointer("/market_context/analytics/chain_iv_pct")
                        .and_then(|v| v.as_f64())
                }),
            llm_decision_id,
            ..Default::default()
        });
}

fn watchlist_for_scan(rules: &RulesConfig, state: &AgentState) -> Vec<WatchlistItemConfig> {
    let mut items = rules.watchlist_items();
    if let Some(sig) = &state.redeploy_signal {
        if let Some(u) = &sig.underlying {
            let key = u.to_uppercase();
            if redeploy_cooldown_active(rules, sig) {
                items.retain(|i| !i.symbol.eq_ignore_ascii_case(&key));
            } else if rules.entry_policy.promote_redeploy_symbol {
                if let Some(pos) = items.iter().position(|i| i.symbol.eq_ignore_ascii_case(&key)) {
                    let item = items.remove(pos);
                    items.insert(0, item);
                }
            }
        }
    }
    items
}

struct ScanEntriesResult {
    entries: Vec<(String, StrategyKind, Value)>,
    skipped: Vec<String>,
}

async fn scan_entries_for_account(
    market: &MarketDataApi,
    rules: &RulesConfig,
    state: &AgentState,
    account_hash: &str,
    today: NaiveDate,
    scan_watchlist: &[WatchlistItemConfig],
    want_vertical: bool,
    want_condor: bool,
    vertical_type: &str,
) -> Result<ScanEntriesResult> {
    let mut result = ScanEntriesResult {
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    let policy = &rules.entry_policy;

    let mut primary_items: Vec<&WatchlistItemConfig> = Vec::new();
    let mut fallback_items: Vec<&WatchlistItemConfig> = Vec::new();
    for item in scan_watchlist {
        match item.role {
            WatchlistRole::Primary => primary_items.push(item),
            WatchlistRole::Fallback => fallback_items.push(item),
        }
    }

    scan_watchlist_tier(
        market,
        rules,
        state,
        account_hash,
        today,
        &primary_items,
        want_vertical,
        want_condor,
        vertical_type,
        &mut result,
    )
    .await?;

    let need_fallback = result.entries.is_empty()
        && policy.fallback_only_after_primary_exhausted
        && !fallback_items.is_empty();
    let scan_all_fallbacks = !policy.fallback_only_after_primary_exhausted;

    if need_fallback || scan_all_fallbacks {
        scan_watchlist_tier(
            market,
            rules,
            state,
            account_hash,
            today,
            &fallback_items,
            want_vertical,
            want_condor,
            vertical_type,
            &mut result,
        )
        .await?;
    }

    if policy.mode == EntryScanMode::FirstQualifying && result.entries.len() > 1 {
        result.entries.truncate(1);
    }

    Ok(result)
}

async fn scan_watchlist_tier(
    market: &MarketDataApi,
    rules: &RulesConfig,
    state: &AgentState,
    account_hash: &str,
    today: NaiveDate,
    items: &[&WatchlistItemConfig],
    want_vertical: bool,
    want_condor: bool,
    vertical_type: &str,
    result: &mut ScanEntriesResult,
) -> Result<()> {
    let policy = &rules.entry_policy;

    for item in items {
        if policy.mode == EntryScanMode::FirstQualifying && !result.entries.is_empty() {
            break;
        }

        let sym = item.symbol.to_uppercase();
        if !rules.risk.allowed_underlyings.is_empty()
            && !rules
                .risk
                .allowed_underlyings
                .iter()
                .any(|u| u.eq_ignore_ascii_case(&sym))
        {
            continue;
        }
        if underlying_entry_cap_reached(rules, state, account_hash, &sym) {
            continue;
        }
        if let Some(group_name) = correlation_group_cap_reached(rules, state, account_hash, &sym) {
            result.skipped.push(format!(
                "correlation group cap — skip {sym} (group {group_name})"
            ));
            continue;
        }
        if let Some(days) = stop_loss_re_entry_blocked(rules, state, &sym) {
            result.skipped.push(format!(
                "stop-loss re-entry cooldown — skip {sym} for ~{days}d"
            ));
            continue;
        }

        let vertical_rules = rules.effective_vertical_entry(&sym);

        if rules.strategies.vertical.enabled && want_vertical {
            match evaluate_vertical_entry(
                market,
                rules,
                &vertical_rules,
                &sym,
                today,
                state,
                account_hash,
                vertical_type,
            )
            .await
            {
                Ok(Some(signal)) => {
                    result.entries.push((
                        account_hash.to_string(),
                        StrategyKind::Vertical,
                        signal,
                    ));
                    if policy.mode == EntryScanMode::FirstQualifying {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => result.skipped.push(format!("{sym} vertical: {e:#}")),
            }
        }

        if policy.mode == EntryScanMode::FirstQualifying && !result.entries.is_empty() {
            break;
        }

        if rules.strategies.iron_condor.enabled && want_condor {
            match evaluate_condor_entry(market, rules, &sym, today, state, account_hash).await {
                Ok(Some(signal)) => {
                    result.entries.push((
                        account_hash.to_string(),
                        StrategyKind::IronCondor,
                        signal,
                    ));
                    if policy.mode == EntryScanMode::FirstQualifying {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => result.skipped.push(format!("{sym} iron_condor: {e:#}")),
            }
        }
    }

    Ok(())
}

fn entry_execution_requires_llm(rules: &RulesConfig) -> bool {
    rules.llm.enabled && rules.llm.veto_entries && rules.entry_policy.require_llm_proceed
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

fn underlying_entry_cap_reached(
    rules: &RulesConfig,
    state: &AgentState,
    account_hash: &str,
    underlying: &str,
) -> bool {
    let Some(cap) = rules.risk.max_open_for_underlying(underlying) else {
        return false;
    };
    let open = state.count_open_for_underlying(account_hash, underlying);
    let pending = state.pending_entry_count_for_underlying(account_hash, underlying);
    open + pending >= cap
}

/// Returns the group name when opening `underlying` would exceed that group's `max_open`.
fn correlation_group_cap_reached(
    rules: &RulesConfig,
    state: &AgentState,
    account_hash: &str,
    underlying: &str,
) -> Option<String> {
    let group = rules.risk.correlation_group_for(underlying)?;
    if group.max_open == 0 {
        return None;
    }
    let open = state.count_open_in_symbol_set(account_hash, &group.symbols);
    let pending = state.pending_entry_count_in_symbol_set(account_hash, &group.symbols);
    if open + pending >= group.max_open {
        Some(group.name.clone())
    } else {
        None
    }
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

async fn notify_trading_halted(
    telegram: Option<&TelegramNotifier>,
    rules: &RulesConfig,
    reason: &str,
) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let _ = tg
        .send(&format!(
            "schwab [{}]\n⚠ TRADING HALTED\n{reason}\nExits still armed.",
            rules.agent_id
        ))
        .await;
}

async fn notify_trading_recovered(telegram: Option<&TelegramNotifier>, rules: &RulesConfig) {
    let Some(tg) = telegram else { return };
    if !tg.wants_actions() {
        return;
    }
    let _ = tg
        .send(&format!(
            "schwab [{}]\n✓ TRADING RESUMED\nDrawdown halt cleared — new entries allowed.",
            rules.agent_id
        ))
        .await;
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
    entry: &VerticalEntryRules,
    underlying: &str,
    today: NaiveDate,
    state: &AgentState,
    account_hash: &str,
    spread_type: &str,
) -> Result<Option<Value>> {
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
            strike_count: Some(120),
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

    // Never fall back to a fixed % OTM strike — that bypasses the delta band and
    // recreates ~20–25Δ "cheap premium" picks (QQQ 655/650 Jul 2026 post-mortem).
    let Some(short_strike) = pick_strike_by_delta(
        &strike_map,
        entry.short_delta_min,
        entry.short_delta_max,
        is_put,
    ) else {
        return Ok(None);
    };
    let long_strike = pick_wing_strike(&strike_map, short_strike, entry.max_width, is_put)?;
    let width = (short_strike - long_strike).abs();
    if width < entry.max_width * 0.5 {
        return Ok(None);
    }
    let credit = estimate_spread_credit(&strike_map, short_strike, long_strike)?;
    if credit < entry.min_credit {
        return Ok(None);
    }
    if !entry_quality_ok(&strike_map, short_strike, long_strike, width, credit, entry) {
        return Ok(None);
    }

    let lookback = rules.regime.realized_vol_lookback.max(5);
    let realized_vol_pct = fetch_realized_vol_pct(market, underlying, lookback)
        .await
        .ok()
        .flatten();

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
        realized_vol_pct,
    );

    let analytics = analytics_from_json(market_context.get("analytics").unwrap_or(&json!({})));
    if let Some(ref a) = analytics {
        if !entry_analytics_pass(entry, a) {
            return Ok(None);
        }
        if candidate_fails_thesis_gates(rules, a).is_some() {
            return Ok(None);
        }
    } else {
        // Analytics unavailable — cannot verify delta band / 1σ / IV-RV gates.
        return Ok(None);
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
            strike_count: Some(80),
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

    // `short_delta` is |delta|, not an OTM%. Use a practical band around the target.
    let delta_min = (entry.short_delta - 0.06).max(0.06);
    let delta_max = (entry.short_delta + 0.08).min(0.28);
    let Some(put_short) = pick_strike_by_delta(&put_map, delta_min, delta_max, true) else {
        anyhow::bail!("no put short in |delta| {delta_min:.2}-{delta_max:.2}");
    };
    let Some(call_short) = pick_strike_by_delta(&call_map, delta_min, delta_max, false) else {
        anyhow::bail!("no call short in |delta| {delta_min:.2}-{delta_max:.2}");
    };
    let put_long = pick_wing_strike(&put_map, put_short, entry.wing_width, true)?;
    let call_long = pick_wing_strike(&call_map, call_short, entry.wing_width, false)?;

    let put_credit = estimate_spread_credit(&put_map, put_short, put_long)?;
    let call_credit = estimate_spread_credit(&call_map, call_short, call_long)?;
    let total_credit = put_credit + call_credit;
    if total_credit < entry.min_credit {
        anyhow::bail!(
            "credit ${total_credit:.2} < min ${:.2} (put ${put_credit:.2} + call ${call_credit:.2})",
            entry.min_credit
        );
    }

    let lookback = rules.regime.realized_vol_lookback.max(5);
    let realized_vol_pct = fetch_realized_vol_pct(market, underlying, lookback)
        .await
        .ok()
        .flatten();
    if entry.min_iv_rv_ratio.is_some() {
        let chain_iv = chain.get("volatility").and_then(|v| v.as_f64());
        let ratio = iv_rv_ratio(chain_iv, realized_vol_pct);
        if !passes_min_iv_rv_ratio(entry.min_iv_rv_ratio, ratio) {
            anyhow::bail!(
                "iv_rv_ratio {:?} below min {:?}",
                ratio,
                entry.min_iv_rv_ratio
            );
        }
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
    let max_loss = crate::options::strategies::iron_condor_max_loss(&params);
    if max_loss > rules.risk.max_risk_per_trade_usd {
        anyhow::bail!(
            "max loss ${max_loss:.0} > max_risk_per_trade_usd ${:.0} (put width {:.0}, call width {:.0}, credit ${total_credit:.2})",
            rules.risk.max_risk_per_trade_usd,
            put_short - put_long,
            call_long - call_short,
        );
    }

    let market_context = iron_condor_entry_market_context(
        &chain,
        underlying,
        expiry,
        today,
        &put_map,
        &call_map,
        put_short,
        put_long,
        call_short,
        call_long,
        put_credit,
        call_credit,
        entry.max_contracts_per_trade as f64,
        realized_vol_pct,
    );

    Ok(Some(json!({
        "type": "entry",
        "strategy": "iron_condor",
        "account_hash": account_hash,
        "position_id": candidate_id,
        "params": params,
        "estimated_credit": total_credit,
        "market_context": market_context,
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

/// For put credit spreads, long strike is below short by approximately `width`.
/// Prefers an exact-width wing when listed; otherwise nearest without widening past `width`.
fn pick_wing_strike(strike_map: &Value, short_strike: f64, width: f64, puts: bool) -> Result<f64> {
    let target = if puts {
        short_strike - width
    } else {
        short_strike + width
    };
    let obj = strike_map.as_object().context("strike map not object")?;
    let candidates: Vec<f64> = obj
        .keys()
        .filter_map(|k| k.parse::<f64>().ok())
        .filter(|s| {
            if puts {
                *s < short_strike - f64::EPSILON
            } else {
                *s > short_strike + f64::EPSILON
            }
        })
        .collect();
    if candidates.is_empty() {
        anyhow::bail!("no wing strikes beyond short {short_strike}");
    }
    if let Some(exact) = candidates
        .iter()
        .copied()
        .find(|s| (*s - target).abs() < 0.011)
    {
        return Ok(exact);
    }
    // Prefer wings that do not exceed configured width (avoids max-loss blowouts).
    let within: Vec<f64> = candidates
        .iter()
        .copied()
        .filter(|s| {
            let w = if puts {
                short_strike - *s
            } else {
                *s - short_strike
            };
            w <= width + 0.011
        })
        .collect();
    let pool = if within.is_empty() {
        candidates
    } else {
        within
    };
    pool.into_iter()
        .min_by(|a, b| {
            ((*a - target).abs())
                .partial_cmp(&(*b - target).abs())
                .unwrap()
        })
        .context("no wing strike candidates")
}

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
        if let Some(contract) = strike_map
            .get(&key)
            .and_then(|contracts| contracts.as_array()?.first())
        {
            if let Some(val) = contract.get(field).and_then(|v| v.as_f64()).filter(|v| *v > 0.0)
            {
                return Ok(val);
            }
            // Thin quotes: fall back to mark for credit estimates.
            if let Some(mark) = contract.get("mark").and_then(|v| v.as_f64()).filter(|v| *v > 0.0)
            {
                return Ok(mark);
            }
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

    let roll_replacement = signal
        .get("roll_replacement")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !roll_replacement
        && rules.risk.max_trades_per_day > 0
        && state.trades_capacity_used() >= rules.risk.max_trades_per_day
    {
        let detail = json!({
            "fill_status": "SKIPPED",
            "reason": "max_trades_per_day reached or reserved by pending entries",
            "trades_today": state.trades_today,
            "pending_entries": state.pending_entry_count(),
            "max_trades_per_day": rules.risk.max_trades_per_day,
            "signal": signal,
        });
        state.record_action("entry_skipped", detail.clone());
        return Ok(Some(detail));
    }

    let params = signal
        .get("params")
        .cloned()
        .context("signal missing params")?;
    let margin = crate::options::validate::estimate_order_margin(&json!({}), kind, &params)?;
    if margin > rules.risk.max_risk_per_trade_usd {
        let detail = json!({
            "fill_status": "SKIPPED",
            "reason": "max_risk_per_trade_usd exceeded",
            "required_margin_usd": margin,
            "max_risk_per_trade_usd": rules.risk.max_risk_per_trade_usd,
            "signal": signal,
        });
        state.record_action("entry_skipped", detail.clone());
        return Ok(Some(detail));
    }
    let reserved = state.reserved_risk_usd();
    if reserved + margin > rules.risk.max_portfolio_risk_usd {
        let detail = json!({
            "fill_status": "SKIPPED",
            "reason": "max_portfolio_risk_usd exceeded",
            "reserved_risk_usd": reserved,
            "new_order_margin_usd": margin,
            "projected_reserved_risk_usd": reserved + margin,
            "max_portfolio_risk_usd": rules.risk.max_portfolio_risk_usd,
            "signal": signal,
        });
        state.record_action("entry_skipped", detail.clone());
        return Ok(Some(detail));
    }
    let position_id = signal
        .get("position_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| candidate_id_from_params(account_hash, kind, &params));
    if state.open_positions.contains_key(&position_id) || state.has_pending_position(&position_id) {
        let detail = json!({
            "fill_status": "SKIPPED",
            "reason": "position already open or pending",
            "position_id": position_id,
            "signal": signal,
        });
        state.record_action("entry_skipped", detail.clone());
        return Ok(Some(detail));
    }

    if let Some(remaining) =
        entry_attempt_cooldown_active(&rules.entry_policy, state, &position_id)
    {
        if !roll_replacement {
            let detail = json!({
                "fill_status": "SKIPPED",
                "reason": "entry_attempt_cooldown",
                "remaining_minutes": remaining,
                "position_id": position_id,
                "signal": signal,
            });
            state.record_action("entry_skipped", detail.clone());
            return Ok(Some(detail));
        }
    }

    require_trading_approval(
        runtime,
        "agent entry",
        &format!("Open {kind:?} on {account_hash}"),
    )?;

    ensure_option_buying_power(trader, account_hash, margin).await?;
    let order = build_order_for_strategy(kind, &params)?;
    runtime.safety.validate_order(&order, None, None)?;

    record_entry_attempt(state, &position_id);

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

    if !roll_replacement {
        state.trades_today += 1;
    }
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
    let rolls_used = signal
        .get("rolls_used")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let last_roll_at = if roll_replacement {
        Some(Utc::now())
    } else {
        None
    };

    let total_contracts;
    if let Some(existing) = state.open_positions.get_mut(&position_id) {
        let prev_contracts = existing.contracts.max(1);
        existing.contracts = prev_contracts + order_contracts;
        existing.max_loss_usd += margin;
        if let Some(credit) = new_credit {
            let blended = existing.entry_credit.unwrap_or(credit) * prev_contracts as f64
                + credit * order_contracts as f64;
            existing.entry_credit = Some(blended / existing.contracts as f64);
        }
        total_contracts = existing.contracts;
    } else {
        total_contracts = order_contracts;
        let mut tracked = TrackedPosition {
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
            entry_chain_iv_pct: signal
                .pointer("/market_context/chain_iv")
                .and_then(|v| v.as_f64())
                .or_else(|| {
                    signal
                        .pointer("/market_context/analytics/chain_iv_pct")
                        .and_then(|v| v.as_f64())
                }),
            rolls_used,
            last_roll_at,
            ..Default::default()
        };
        crate::agent::scorecard::attach_decision_to_position(&mut tracked, state);
        state.open_positions.insert(position_id.clone(), tracked);
    }
    clear_redeploy_after_entry(state);
    state.entry_proceed_cache = None;
    state.record_action("entry", signal.clone());

    // Rebuild the order for the position's TOTAL contract count (not just this fill's
    // incremental contracts) so a top-up to an existing position gets a protective order
    // sized for the whole stack, and stash the params so `reconcile_protective_orders` can
    // rebuild the same order later without needing this local state.
    let mut full_params = params.clone();
    full_params["contracts"] = json!(total_contracts);
    if let Some(p) = state.open_positions.get_mut(&position_id) {
        p.entry_params = Some(full_params.clone());
    }
    match build_order_for_strategy(kind, &full_params) {
        Ok(full_order) => {
            place_or_replace_protective_order(
                runtime,
                trader,
                account_hash,
                rules,
                &full_order,
                &position_id,
                state,
            )
            .await;
        }
        Err(e) => {
            tracing::warn!(
                "could not rebuild full-size order for protective placement on \
                 {position_id}: {e:#}"
            );
        }
    }

    Ok(Some(json!({
        "entry": place,
        "signal": signal,
        "wait": wait_result.as_ref().map(wait_result_json),
        "position_id": position_id,
        "fill_status": fill_status,
    })))
}

/// Place (or replace, if this fill added to an existing tracked position) the broker-resident
/// GTC profit-target close order for `position_id`. Best-effort: failure does not block the
/// entry, which is already filled — it's recorded on the position for the reconcile pass to
/// retry (see `reconcile_protective_orders`). Schwab does not support a stop trigger on
/// multi-leg option orders, so this only ever covers the profit-target side (see
/// `agent::protective` and `docs/OPTIONS_RULES.md`).
async fn place_or_replace_protective_order(
    runtime: &RuntimeConfig,
    trader: &Arc<TraderApi>,
    account_hash: &str,
    rules: &RulesConfig,
    entry_order: &Value,
    position_id: &str,
    state: &mut AgentState,
) {
    if !rules.execution.protective_order.enabled {
        return;
    }

    if let Some(stale_id) = state
        .open_positions
        .get(position_id)
        .and_then(|p| p.protective_order_id.clone())
    {
        if let Err(e) =
            protective::cancel_protective_order(runtime, trader, account_hash, &stale_id).await
        {
            tracing::warn!("failed to cancel stale protective order {stale_id}: {e:#}");
        }
        if let Some(p) = state.open_positions.get_mut(position_id) {
            p.protective_order_id = None;
            p.protective_order_status = None;
        }
    }

    let Some(entry_credit) = state
        .open_positions
        .get(position_id)
        .and_then(|p| p.entry_credit)
    else {
        return;
    };

    let cfg = &rules.execution.protective_order;
    match protective::place_profit_target_order_with_retry(
        runtime,
        trader,
        account_hash,
        entry_order,
        entry_credit,
        rules.exit_rules.profit_target_pct,
        cfg.max_attempts,
        cfg.max_seconds,
    )
    .await
    {
        Ok(result) => {
            if let Some(p) = state.open_positions.get_mut(position_id) {
                p.protective_order_id = result.order_id.clone();
                p.protective_order_status = Some("WORKING".to_string());
                p.protective_order_attempts = 0;
            }
            state.record_action(
                "protective_order_placed",
                json!({
                    "position_id": position_id,
                    "order_id": result.order_id,
                    "attempts": result.attempts,
                    "order": result.order,
                }),
            );
        }
        Err(e) => {
            if let Some(p) = state.open_positions.get_mut(position_id) {
                p.protective_order_attempts = p.protective_order_attempts.saturating_add(1);
            }
            tracing::warn!("protective order placement failed for {position_id}: {e:#}");
            state.record_action(
                "protective_order_failed",
                json!({
                    "position_id": position_id,
                    "error": format!("{e:#}"),
                }),
            );
        }
    }
}

enum DefensiveRollOutcome {
    /// Close + open succeeded; do not record stop_loss cooldown.
    Success(Value),
    /// Eligibility failed — caller should take the normal stop path.
    NotAttempted { reason: String },
    /// Close already applied (filled/pending) but replacement failed — no second exit.
    StopCompleted {
        detail: Option<Value>,
        reason: String,
    },
}

/// Intercept mechanical `stop_loss` with a managed vertical roll when eligible.
#[allow(clippy::too_many_arguments)]
async fn try_defensive_roll(
    runtime: &RuntimeConfig,
    trader: &Arc<TraderApi>,
    market: &MarketDataApi,
    rules_path: &std::path::Path,
    rules: &RulesConfig,
    state: &mut AgentState,
    today: NaiveDate,
    account_hash: &str,
    position_id: &str,
    tracked: &TrackedPosition,
    group: &crate::options::OptionPositionGroup,
    eval: &ExitEvaluation,
    short_otm_pct: Option<f64>,
    simulate: bool,
    _telegram: Option<&TelegramNotifier>,
) -> Result<DefensiveRollOutcome> {
    let roll_cfg = &rules.exit_rules.roll;
    if let Err(skip) = roll_eligible(
        roll_cfg,
        &RollEligibility {
            tracked,
            mark_dte: eval.mark.dte,
            short_otm_pct,
            rolls_today: state.rolls_today,
            reserved_risk_usd: state.reserved_risk_usd(),
            max_portfolio_risk_usd: rules.risk.max_portfolio_risk_usd,
        },
    ) {
        return Ok(DefensiveRollOutcome::NotAttempted {
            reason: skip.as_str().to_string(),
        });
    }

    let Some(spread_type) = spread_type_from_tracked(tracked) else {
        return Ok(DefensiveRollOutcome::NotAttempted {
            reason: "roll_missing_spread_type".into(),
        });
    };
    if !spread_type.eq_ignore_ascii_case("put_credit")
        && !spread_type.eq_ignore_ascii_case("call_credit")
    {
        return Ok(DefensiveRollOutcome::NotAttempted {
            reason: "roll_not_credit_vertical".into(),
        });
    }

    let close_debit = eval.mark.debit_to_close;
    let entry_credit = tracked
        .entry_credit
        .unwrap_or(eval.mark.entry_credit)
        .max(0.0);
    if entry_credit <= f64::EPSILON {
        return Ok(DefensiveRollOutcome::NotAttempted {
            reason: "roll_missing_entry_credit".into(),
        });
    }

    // Close first (cancels protective GTC via execute_exit / removes sim position).
    let exit_signal = exit_signal_json_for_account(account_hash, group, eval);
    let close_detail = if simulate {
        match record_sim_exit(
            rules_path,
            state,
            rules,
            position_id,
            "defensive_roll",
            &eval.mark,
            &exit_signal,
        ) {
            Ok(d) => d,
            Err(e) => {
                return Ok(DefensiveRollOutcome::NotAttempted {
                    reason: format!("roll_close_failed:{e:#}"),
                });
            }
        }
    } else {
        match execute_exit(
            runtime,
            trader,
            account_hash,
            rules_path,
            rules,
            group,
            &exit_signal,
            state,
            None,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                return Ok(DefensiveRollOutcome::NotAttempted {
                    reason: format!("roll_close_failed:{e:#}"),
                });
            }
        }
    };

    let close_fill = close_detail
        .get("fill_status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let close_ok = close_fill.eq_ignore_ascii_case("FILLED")
        || (!rules.execution.wait_for_fill && !close_fill.eq_ignore_ascii_case("SKIPPED"));
    if !close_ok {
        return Ok(DefensiveRollOutcome::StopCompleted {
            detail: Some(close_detail),
            reason: format!("close_not_filled:{close_fill}"),
        });
    }

    let original_width = tracked
        .entry_params
        .as_ref()
        .and_then(original_width_from_params);
    let biased = roll_biased_entry_rules(
        &rules.entry_rules.vertical,
        roll_cfg,
        eval.mark.dte,
        tracked.entry_short_delta,
        original_width,
        tracked.contracts.max(1),
    );

    let candidate = match evaluate_vertical_entry(
        market,
        rules,
        &biased,
        &tracked.underlying,
        today,
        state,
        account_hash,
        &spread_type,
    )
    .await
    {
        Ok(Some(mut signal)) => {
            let new_credit = signal
                .get("estimated_credit")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if !roll_money_ok(
                entry_credit,
                close_debit,
                new_credit,
                roll_cfg.max_debit_pct_of_entry_credit,
            ) {
                return Ok(DefensiveRollOutcome::StopCompleted {
                    detail: Some(close_detail),
                    reason: format!(
                        "roll_debit_too_large:net={:.3}",
                        roll_net(close_debit, new_credit)
                    ),
                });
            }
            let next_rolls = tracked.rolls_used.saturating_add(1);
            if let Some(obj) = signal.as_object_mut() {
                obj.insert("roll_replacement".into(), json!(true));
                obj.insert("rolls_used".into(), json!(next_rolls));
                obj.insert("closed_position_id".into(), json!(position_id));
                obj.insert("roll_close_debit".into(), json!(close_debit));
                obj.insert(
                    "roll_net".into(),
                    json!(roll_net(close_debit, new_credit)),
                );
            }
            signal
        }
        Ok(None) => {
            return Ok(DefensiveRollOutcome::StopCompleted {
                detail: Some(close_detail),
                reason: "no_roll_candidate".into(),
            });
        }
        Err(e) => {
            return Ok(DefensiveRollOutcome::StopCompleted {
                detail: Some(close_detail),
                reason: format!("roll_candidate_error:{e:#}"),
            });
        }
    };

    let new_credit = candidate
        .get("estimated_credit")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let new_id = candidate
        .get("position_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let open_detail = if simulate {
        match record_sim_entry(
            rules_path,
            state,
            rules,
            account_hash,
            StrategyKind::Vertical,
            &candidate,
        ) {
            Ok(d) => d,
            Err(e) => {
                return Ok(DefensiveRollOutcome::StopCompleted {
                    detail: Some(close_detail),
                    reason: format!("roll_open_failed:{e:#}"),
                });
            }
        }
    } else {
        match maybe_execute_entry(
            runtime,
            trader,
            account_hash,
            StrategyKind::Vertical,
            &candidate,
            rules,
            state,
        )
        .await
        {
            Ok(Some(d)) => d,
            Ok(None) => {
                return Ok(DefensiveRollOutcome::StopCompleted {
                    detail: Some(close_detail),
                    reason: "roll_open_dry_or_none".into(),
                });
            }
            Err(e) => {
                return Ok(DefensiveRollOutcome::StopCompleted {
                    detail: Some(close_detail),
                    reason: format!("roll_open_failed:{e:#}"),
                });
            }
        }
    };

    let open_fill = open_detail
        .get("fill_status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !open_fill.eq_ignore_ascii_case("FILLED")
        && !(open_fill.is_empty() && !rules.execution.wait_for_fill)
    {
        // Working/skipped after close — treat as stop completed (position already closed).
        if open_fill.eq_ignore_ascii_case("SKIPPED")
            || open_fill.eq_ignore_ascii_case("REJECTED")
            || open_fill.eq_ignore_ascii_case("CANCELED")
        {
            return Ok(DefensiveRollOutcome::StopCompleted {
                detail: Some(json!({
                    "close": close_detail,
                    "open": open_detail,
                })),
                reason: format!("roll_open_status:{open_fill}"),
            });
        }
        // WORKING: replacement pending — still count as roll in progress; bump rolls_today
        // only on FILLED. Treat working as incomplete stop path so cooldown applies.
        if !open_fill.eq_ignore_ascii_case("FILLED") {
            return Ok(DefensiveRollOutcome::StopCompleted {
                detail: Some(json!({
                    "close": close_detail,
                    "open": open_detail,
                })),
                reason: format!("roll_open_not_filled:{open_fill}"),
            });
        }
    }

    state.rolls_today = state.rolls_today.saturating_add(1);
    let net = roll_net(close_debit, new_credit);
    let detail = json!({
        "type": "defensive_roll",
        "fill_status": "FILLED",
        "reason": "rolled",
        "closed_id": position_id,
        "new_id": new_id,
        "close_debit": close_debit,
        "new_credit": new_credit,
        "net": net,
        "underlying": tracked.underlying,
        "rolls_used": tracked.rolls_used.saturating_add(1),
        "close": close_detail,
        "open": open_detail,
        "signal": candidate,
    });
    state.record_action("defensive_roll", detail.clone());
    let _ = journal::append_event(rules_path, simulate, "defensive_roll", detail.clone());
    Ok(DefensiveRollOutcome::Success(detail))
}

async fn execute_exit(
    runtime: &RuntimeConfig,
    trader: &Arc<TraderApi>,
    account_hash: &str,
    rules_path: &std::path::Path,
    rules: &RulesConfig,
    group: &crate::options::OptionPositionGroup,
    signal: &Value,
    state: &mut AgentState,
    llm_client: Option<&OpenRouterClient>,
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

    // Cancel any resting broker-side profit-target order first, to avoid a race where it
    // fills at the same time as this mechanical (stop/DTE/thesis) close.
    if let Some(protective_id) = state
        .open_positions
        .get(&position_id)
        .and_then(|p| p.protective_order_id.clone())
    {
        match protective::cancel_protective_order(runtime, trader, account_hash, &protective_id)
            .await
        {
            Ok(_) => {
                if let Some(p) = state.open_positions.get_mut(&position_id) {
                    p.protective_order_id = None;
                    p.protective_order_status = Some("CANCELED".to_string());
                }
            }
            Err(e) => {
                tracing::warn!(
                    "failed to cancel protective order {protective_id} before exit \
                     (it may have already filled): {e:#}"
                );
            }
        }
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
        if let Some(tracked) = state.open_positions.get(&position_id).cloned() {
            let debit = signal
                .pointer("/mark/debit_to_close")
                .and_then(|v| v.as_f64())
                .unwrap_or(close_limit);
            record_live_realized_pnl(state, &tracked, debit);
            let entry = tracked.entry_credit.unwrap_or(0.0);
            let pnl_usd =
                crate::agent::risk::credit_spread_pnl_usd(entry, debit, tracked.contracts);
            let pnl_pct = if entry > f64::EPSILON {
                ((entry - debit) / entry) * 100.0
            } else {
                0.0
            };
            let exit_reason = signal
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("exit");
            crate::agent::scorecard::resolve_on_exit(
                rules_path,
                runtime.simulate,
                state,
                &tracked,
                exit_reason,
                pnl_usd,
                pnl_pct,
            );
            if let Ok(Some(path)) = crate::agent::learn::write_postmortem_suggestions(
                rules_path,
                &rules.llm,
                llm_client,
                &state.llm_scorecard.clone(),
                &tracked,
                exit_reason,
                pnl_usd,
                pnl_pct,
            )
            .await
            {
                state.record_action(
                    "llm_suggestions",
                    json!({ "path": path.display().to_string() }),
                );
            }
        }
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
        AccountType, EntryPolicyConfig, EntryRules, ExecutionConfig, ExitRules, LlmConfig,
        NotifyConfig, RiskConfig, RulesAccount, RulesConfig, ScheduleConfig, StrategiesToggle,
        StrategyEnabled, VerticalEntryRules, WatchlistEntry,
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
            watchlist: vec![WatchlistEntry::from("IWM")],
            entry_policy: EntryPolicyConfig::default(),
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
                ..Default::default()
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
        rules.watchlist = vec![WatchlistEntry::from("SPY"), WatchlistEntry::from("IWM")];
        rules.exit_rules.thesis.redeploy_cooldown_minutes = Some(120);
        let mut state = AgentState::default();
        state.redeploy_signal = Some(RedeploySignal {
            at: Utc::now(),
            reason: "thesis_near_strike".into(),
            underlying: Some("IWM".into()),
        });
        let wl = watchlist_for_scan(&rules, &state);
        assert!(!wl.iter().any(|i| i.symbol.eq_ignore_ascii_case("IWM")));

        state.redeploy_signal = Some(RedeploySignal {
            at: Utc::now() - chrono::Duration::minutes(121),
            reason: "thesis_near_strike".into(),
            underlying: Some("IWM".into()),
        });
        let wl = watchlist_for_scan(&rules, &state);
        assert_eq!(wl.first().map(|i| i.symbol.as_str()), Some("SPY"));

        rules.entry_policy.promote_redeploy_symbol = true;
        let wl = watchlist_for_scan(&rules, &state);
        assert_eq!(wl.first().map(|i| i.symbol.as_str()), Some("IWM"));
    }

    #[test]
    fn underlying_entry_cap_blocks_second_iwm_spread() {
        let mut rules = test_rules(2);
        rules
            .risk
            .max_open_per_underlying
            .insert("IWM".into(), 1);
        let state = state_with_open_position();
        assert!(underlying_entry_cap_reached(
            &rules,
            &state,
            "ACC",
            "IWM"
        ));
        assert!(!underlying_entry_cap_reached(
            &rules,
            &state,
            "ACC",
            "SPY"
        ));
    }

    #[test]
    fn correlation_group_cap_blocks_second_index() {
        let mut rules = test_rules(2);
        rules.risk.correlation_groups = vec![crate::rules::CorrelationGroupConfig {
            name: "broad_market".into(),
            symbols: vec!["QQQ".into(), "IWM".into()],
            max_open: 1,
        }];
        let state = state_with_open_position(); // IWM open on ACC
        assert_eq!(
            correlation_group_cap_reached(&rules, &state, "ACC", "QQQ").as_deref(),
            Some("broad_market")
        );
        assert!(correlation_group_cap_reached(&rules, &AgentState::default(), "ACC", "QQQ").is_none());
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
            veto_category: "other".into(),
            evidence: String::new(),
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
