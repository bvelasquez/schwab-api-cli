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
use crate::order_status::{wait_for_order, WaitCondition, WaitOptions};
use crate::rules::{LlmPhase, RulesConfig};
use crate::safety::{execute_trading_order, require_trading_approval};

use super::exits::{
    evaluate_exit_for_group, exit_signal_json, find_tracked_position, position_key,
    reconcile_open_positions,
};
use super::llm::OpenRouterClient;
use super::state::{default_state_path, load_state, save_state, AgentState, TrackedPosition};

#[derive(Debug, Clone, serde::Serialize)]
pub struct TickResult {
    pub signals: Vec<Value>,
    pub actions: Vec<Value>,
    pub skipped: Vec<String>,
    pub llm_review: Option<Value>,
}

pub async fn run_agent_loop(
    runtime: &RuntimeConfig,
    rules_path: &std::path::Path,
    once: bool,
) -> Result<()> {
    let rules = RulesConfig::load(rules_path)?;
    let state_path = default_state_path(rules_path);
    let mut state = load_state(&state_path)?;
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
                "signals": result.signals,
                "actions": result.actions,
                "skipped": result.skipped,
                "llm_review": result.llm_review,
                "dry_run": runtime.dry_run,
            }),
        ));

        notify_tick(telegram.as_ref(), &rules, &result, runtime.dry_run).await;

        if once {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(
            rules.schedule.tick_interval_seconds.max(5),
        ))
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
        signals: vec![],
        actions: vec![],
        skipped: vec![],
        llm_review: None,
    };

    reconcile_open_positions(trader, state, rules).await?;

    if rules.schedule.market_hours_only && !market_is_open(market).await? {
        result
            .skipped
            .push("market closed (option hours)".into());
        return Ok(result);
    }

    if state.trades_today >= rules.risk.max_trades_per_day {
        result.skipped.push(format!(
            "max_trades_per_day ({}) reached",
            rules.risk.max_trades_per_day
        ));
        return Ok(result);
    }

    let mut position_snapshots = Vec::new();

    // Exit evaluation first
    for account in rules.enabled_accounts() {
        let legs = list_option_positions(trader, Some(&account.hash)).await?;
        let groups = group_option_legs(&legs);
        for group in &groups {
            let tracked = find_tracked_position(state, &account.hash, group);
            if let Some(tracked) = tracked {
                position_snapshots.push(json!({
                    "position_id": group.id,
                    "underlying": group.underlying,
                    "expiry": group.expiry,
                    "strategy": tracked.strategy,
                    "entry_credit": tracked.entry_credit,
                    "max_loss_usd": tracked.max_loss_usd,
                    "net_market_value": group.net_market_value,
                }));
            }

            if let Some(eval) =
                evaluate_exit_for_group(market, group, rules, today, tracked).await?
            {
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

    // Entry scan (signals always collected; execution after LLM review)
    let mut pending_entries: Vec<(String, StrategyKind, Value)> = Vec::new();
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

    for (_, _, signal) in &pending_entries {
        result.signals.push(signal.clone());
    }

    // LLM review — selection when candidates exist; monitor on schedule when positions open
    let mut llm_veto_entries = false;
    let mut llm_close_ids: Vec<String> = Vec::new();
    let has_candidates = !pending_entries.is_empty();
    let has_positions = !position_snapshots.is_empty();

    if let Some(client) = llm_client {
        if let Some(phase) = resolve_llm_phase(rules, state, has_candidates, has_positions) {
            let use_web =
                should_use_web_research(rules, state) && matches!(phase, LlmPhase::Selection);

            let context = json!({
                "agent_id": rules.agent_id,
                "tick": state.tick_count,
                "date": today.to_string(),
                "phase": match phase {
                    LlmPhase::Selection => "selection",
                    LlmPhase::Monitor => "monitor",
                },
                "exit_rules": super::exits::exit_rules_summary(&rules.exit_rules),
                "open_positions": position_snapshots,
                "candidate_entries": pending_entries.iter().map(|(_, _, s)| s).collect::<Vec<_>>(),
                "recent_signals": result.signals,
                "watchlist": rules.watchlist,
                "risk": {
                    "max_trades_per_day": rules.risk.max_trades_per_day,
                    "trades_today": state.trades_today,
                },
            });

            match client.review(&rules.llm, phase, &context, use_web).await {
                Ok(review) => {
                    let review_json = review.to_json();
                    result.llm_review = Some(review_json.clone());
                    state.last_llm_review_tick = Some(state.tick_count);
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
                    result.actions.push(a);
                    notify_action(telegram, "ENTRY", &signal).await;
                }
            }
        }
    }

    Ok(result)
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

fn should_run_monitor_review(rules: &RulesConfig, state: &AgentState) -> bool {
    let every = rules.llm.review_every_ticks.max(1);
    match state.last_llm_review_tick {
        None => true,
        Some(last) => state.tick_count.saturating_sub(last) >= every,
    }
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

    Ok(Some(json!({
        "type": "entry",
        "strategy": "vertical",
        "account_hash": account_hash,
        "params": params,
        "estimated_credit": credit,
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

    Ok(Some(json!({ "entry": place, "signal": signal })))
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
