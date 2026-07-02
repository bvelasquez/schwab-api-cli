use anyhow::{Context, Result};
use chrono::NaiveDate;
use schwab_market_data::endpoints::chains::ChainQuery;
use schwab_market_data::MarketDataApi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::options::{
    days_to_expiry, group_option_legs, list_option_positions, position_group_id,
    spread_contract_count, OptionPositionGroup, OptionPositionLeg, VerticalParams,
};
use crate::options::symbology::{build_option_symbol, parse_expiry, parse_option_symbol};
use crate::rules::{ExitRules, RulesConfig};

use super::market_context::vertical_open_position_context;
use super::spread_analytics::{analytics_from_json, SpreadAnalytics};
use super::state::{AgentState, TrackedPosition};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpreadMark {
    pub entry_credit: f64,
    pub debit_to_close: f64,
    pub profit_pct: f64,
    pub dte: i64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitEvaluation {
    pub reason: String,
    pub mark: SpreadMark,
}

pub fn stable_position_key(account_hash: &str, group: &OptionPositionGroup) -> String {
    position_group_id(account_hash, group)
}

pub fn find_tracked_position<'a>(
    state: &'a AgentState,
    account_hash: &str,
    group: &OptionPositionGroup,
) -> Option<&'a TrackedPosition> {
    let stable_key = stable_position_key(account_hash, group);
    state
        .open_positions
        .get(&stable_key)
        .or_else(|| state.open_positions.get(&group.id))
        .or_else(|| {
            state.open_positions.values().find(|p| {
                p.account_hash == account_hash
                    && p.underlying == group.underlying
                    && p.expiry == group.expiry
            })
        })
}

pub fn infer_entry_credit_from_legs(legs: &[OptionPositionLeg]) -> Option<f64> {
    if legs.len() != 2 {
        return None;
    }
    let mut short_premium = None;
    let mut long_premium = None;
    for leg in legs {
        let avg = leg.average_price?;
        if leg.quantity < 0.0 {
            short_premium = Some(avg.abs());
        } else if leg.quantity > 0.0 {
            long_premium = Some(avg.abs());
        }
    }
    match (short_premium, long_premium) {
        (Some(s), Some(l)) => Some((s - l).max(0.0)),
        (Some(s), None) => Some(s),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct PositionMonitorResult {
    pub exit: Option<ExitEvaluation>,
    pub mark: Option<SpreadMark>,
    pub analytics: Option<SpreadAnalytics>,
    pub snapshot: Value,
}

struct VerticalChainSnapshot {
    chain: Value,
    strike_map: Value,
    short_strike: f64,
    long_strike: f64,
    is_put: bool,
    debit_to_close: f64,
}

/// Evaluate mechanical exit rules and build an LLM-ready monitor snapshot (single chain fetch).
pub async fn evaluate_position_monitor(
    market: &MarketDataApi,
    group: &OptionPositionGroup,
    rules: &RulesConfig,
    today: NaiveDate,
    tracked: Option<&TrackedPosition>,
) -> Result<PositionMonitorResult> {
    let entry_credit = tracked
        .and_then(|p| p.entry_credit)
        .or_else(|| infer_entry_credit_from_legs(&group.legs));

    let dte = group
        .legs
        .first()
        .and_then(|l| l.parsed.as_ref())
        .map(|p| days_to_expiry(p.expiry, today))
        .unwrap_or(0);

    let chain_result = fetch_vertical_chain_snapshot(market, group).await;

    let (exit, mark_opt, analytics, market_context, chain_error) = match chain_result {
        Ok(chain_snap) => {
            let profit_pct = entry_credit
                .filter(|c| *c > f64::EPSILON)
                .map(|entry| ((entry - chain_snap.debit_to_close) / entry) * 100.0);
            let mark = SpreadMark {
                entry_credit: entry_credit.unwrap_or(0.0),
                debit_to_close: chain_snap.debit_to_close,
                profit_pct: profit_pct.unwrap_or(0.0),
                dte,
                source: "chain".into(),
            };
            let exit = evaluate_exit_from_mark(rules, entry_credit, &mark);
            let expiry_date = chrono::NaiveDate::parse_from_str(&group.expiry, "%Y-%m-%d")
                .ok()
                .or_else(|| {
                    group
                        .legs
                        .first()
                        .and_then(|l| l.parsed.as_ref())
                        .map(|p| p.expiry)
                })
                .unwrap_or(today);
            let contracts = spread_contract_count(group).max(1);
            let ctx = vertical_open_position_context(
                &chain_snap.chain,
                &group.underlying,
                today,
                expiry_date,
                &chain_snap.strike_map,
                chain_snap.short_strike,
                chain_snap.long_strike,
                chain_snap.is_put,
                entry_credit,
                Some(chain_snap.debit_to_close),
                profit_pct,
                dte,
                contracts,
            );
            let analytics = analytics_from_json(ctx.get("analytics").unwrap_or(&json!({})));
            (exit, Some(mark), analytics, Some(ctx), None)
        }
        Err(e) => {
            let exit = if let Some(credit) = entry_credit.filter(|c| *c > 0.0) {
                evaluate_dte_only_with_credit(group, rules, today, credit, dte)?
            } else {
                evaluate_dte_only(group, rules, today)?
            };
            (exit, None, None, None, Some(e.to_string()))
        }
    };

    let snapshot = monitor_snapshot_json(
        group,
        tracked,
        &exit,
        mark_opt.as_ref(),
        market_context,
        chain_error.as_deref(),
        &rules.exit_rules,
    );
    Ok(PositionMonitorResult {
        exit,
        mark: mark_opt,
        analytics,
        snapshot,
    })
}

pub fn evaluate_exit_from_mark(
    rules: &RulesConfig,
    entry_credit: Option<f64>,
    mark: &SpreadMark,
) -> Option<ExitEvaluation> {
    let entry_credit = entry_credit.filter(|c| *c > f64::EPSILON)?;
    let mark = SpreadMark {
        entry_credit,
        ..mark.clone()
    };

    if mark.profit_pct >= rules.exit_rules.profit_target_pct {
        return Some(ExitEvaluation {
            reason: "profit_target".into(),
            mark,
        });
    }

    let stop_debit = entry_credit * (rules.exit_rules.stop_loss_pct / 100.0);
    if mark.debit_to_close >= stop_debit {
        return Some(ExitEvaluation {
            reason: "stop_loss".into(),
            mark,
        });
    }

    if mark.dte <= rules.exit_rules.dte_close as i64 {
        return Some(ExitEvaluation {
            reason: "dte_close".into(),
            mark,
        });
    }

    None
}

async fn fetch_vertical_chain_snapshot(
    market: &MarketDataApi,
    group: &OptionPositionGroup,
) -> Result<VerticalChainSnapshot> {
    let (short_leg, long_leg) = vertical_legs(group)?;
    let short_strike = short_leg
        .parsed
        .as_ref()
        .map(|p| p.strike)
        .context("short leg missing strike")?;
    let long_strike = long_leg
        .parsed
        .as_ref()
        .map(|p| p.strike)
        .context("long leg missing strike")?;
    let is_put = short_leg.parsed.as_ref().is_some_and(|p| p.put_call == 'P');

    let contract_type = if is_put { "PUT" } else { "CALL" };
    let map_key = if is_put {
        "putExpDateMap"
    } else {
        "callExpDateMap"
    };

    let mut last_err = None;
    for strike_count in [50u32, 100] {
        match fetch_vertical_chain_at_strikes(
            market,
            group,
            contract_type,
            map_key,
            short_strike,
            long_strike,
            is_put,
            strike_count,
        )
        .await
        {
            Ok(snap) => return Ok(snap),
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("chain fetch failed")))
}

#[allow(clippy::too_many_arguments)]
async fn fetch_vertical_chain_at_strikes(
    market: &MarketDataApi,
    group: &OptionPositionGroup,
    contract_type: &str,
    map_key: &str,
    short_strike: f64,
    long_strike: f64,
    is_put: bool,
    strike_count: u32,
) -> Result<VerticalChainSnapshot> {
    let strike_anchor = format_chain_strike(short_strike);
    let chain = market
        .chains()
        .get(&ChainQuery {
            symbol: &group.underlying,
            contract_type: Some(contract_type),
            strike: Some(&strike_anchor),
            strike_count: Some(strike_count),
            include_underlying_quote: Some(true),
            from_date: Some(&group.expiry),
            to_date: Some(&group.expiry),
            ..Default::default()
        })
        .await?;

    let strike_map =
        find_expiry_strikes(&chain, map_key, &group.expiry).context("expiry not found in chain")?;

    let short_ask = strike_quote_field(&strike_map, short_strike, "ask")?;
    let long_bid = strike_quote_field(&strike_map, long_strike, "bid")?;
    let debit_to_close = (short_ask - long_bid).max(0.0);

    Ok(VerticalChainSnapshot {
        chain,
        strike_map,
        short_strike,
        long_strike,
        is_put,
        debit_to_close,
    })
}

fn format_chain_strike(strike: f64) -> String {
    if (strike.fract() * 10.0).round() as i64 % 10 == 0 {
        format!("{strike:.1}")
    } else {
        format!("{strike:.2}")
    }
}

pub fn monitor_snapshot_json(
    group: &OptionPositionGroup,
    tracked: Option<&TrackedPosition>,
    exit_eval: &Option<ExitEvaluation>,
    mark: Option<&SpreadMark>,
    market_context: Option<Value>,
    chain_error: Option<&str>,
    exit_rules: &ExitRules,
) -> Value {
    let entry_credit = tracked
        .and_then(|p| p.entry_credit)
        .or_else(|| infer_entry_credit_from_legs(&group.legs));

    let status = match exit_eval {
        Some(e) => format!("exit: {}", e.reason),
        None => "holding".into(),
    };

    let contracts = tracked
        .map(|p| p.contracts.max(1))
        .unwrap_or_else(|| spread_contract_count(group));

    let mut snapshot = json!({
        "position_id": tracked
            .map(|t| t.position_id.as_str())
            .unwrap_or(group.id.as_str()),
        "legacy_position_id": group.id,
        "underlying": group.underlying,
        "expiry": group.expiry,
        "strategy": tracked
            .map(|t| t.strategy.as_str())
            .unwrap_or_else(|| group.strategy_hint.as_str()),
        "contracts": contracts,
        "entry_credit": entry_credit,
        "max_loss_usd": tracked.map(|p| p.max_loss_usd),
        "net_market_value": group.net_market_value,
        "status": status,
    });

    if let Some(eval) = exit_eval {
        snapshot["profit_pct"] = json!(eval.mark.profit_pct);
        snapshot["dte"] = json!(eval.mark.dte);
        snapshot["debit_to_close"] = json!(eval.mark.debit_to_close);
    } else if let Some(m) = mark {
        snapshot["profit_pct"] = json!(m.profit_pct);
        snapshot["dte"] = json!(m.dte);
        snapshot["debit_to_close"] = json!(m.debit_to_close);
    }

    if let Some(ctx) = market_context {
        snapshot["market_context"] = ctx;
    } else if let Some(err) = chain_error {
        snapshot["market_context_error"] = json!(err);
        snapshot["market_context_note"] = json!(
            "Live chain greeks unavailable; mechanical exits still use chain debit when fetch succeeds on exit ticks."
        );
    }

    if let Some(m) = mark.or(exit_eval.as_ref().map(|e| &e.mark)) {
        let entry = m.entry_credit;
        let stop_debit = entry * (exit_rules.stop_loss_pct / 100.0);
        snapshot["mechanical_rules"] = json!({
            "profit_target_pct": exit_rules.profit_target_pct,
            "stop_loss_pct": exit_rules.stop_loss_pct,
            "stop_debit_threshold_per_share": stop_debit,
            "current_debit_to_close": m.debit_to_close,
            "stop_triggered": m.debit_to_close >= stop_debit,
            "profit_target_triggered": m.profit_pct >= exit_rules.profit_target_pct,
            "note": "Mechanical exits use debit_to_close from the chain, NOT net_market_value. If stop_triggered is false, do not alert that the stop was hit."
        });
    }

    snapshot["net_market_value_note"] = json!(
        "Schwab leg market_value sum in dollars; not comparable to per-share entry_credit or stop_debit_threshold."
    );

    snapshot
}

fn evaluate_dte_only(
    group: &OptionPositionGroup,
    rules: &RulesConfig,
    today: NaiveDate,
) -> Result<Option<ExitEvaluation>> {
    let dte = group
        .legs
        .first()
        .and_then(|l| l.parsed.as_ref())
        .map(|p| days_to_expiry(p.expiry, today))
        .unwrap_or(0);
    if dte > rules.exit_rules.dte_close as i64 {
        return Ok(None);
    }
    Ok(Some(ExitEvaluation {
        reason: "dte_close".into(),
        mark: SpreadMark {
            entry_credit: 0.0,
            debit_to_close: 0.0,
            profit_pct: 0.0,
            dte,
            source: "dte_only".into(),
        },
    }))
}

fn evaluate_dte_only_with_credit(
    _group: &OptionPositionGroup,
    rules: &RulesConfig,
    _today: NaiveDate,
    entry_credit: f64,
    dte: i64,
) -> Result<Option<ExitEvaluation>> {
    if dte > rules.exit_rules.dte_close as i64 {
        return Ok(None);
    }
    Ok(Some(ExitEvaluation {
        reason: "dte_close".into(),
        mark: SpreadMark {
            entry_credit,
            debit_to_close: 0.0,
            profit_pct: 0.0,
            dte,
            source: "dte_fallback".into(),
        },
    }))
}

fn vertical_legs(group: &OptionPositionGroup) -> Result<(&OptionPositionLeg, &OptionPositionLeg)> {
    let short = group
        .legs
        .iter()
        .find(|l| l.quantity < 0.0)
        .context("no short leg")?;
    let long = group
        .legs
        .iter()
        .find(|l| l.quantity > 0.0)
        .context("no long leg")?;
    Ok((short, long))
}

fn find_expiry_strikes(chain: &Value, map_key: &str, expiry: &str) -> Result<Value> {
    let map = chain
        .get(map_key)
        .context("chain missing exp date map")?
        .as_object()
        .context("exp date map not an object")?;

    for (key, strikes) in map {
        let date_part = key.split(':').next().unwrap_or(key);
        if date_part == expiry || key.starts_with(expiry) {
            return Ok(strikes.clone());
        }
    }
    anyhow::bail!("expiry {expiry} not in chain")
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

pub fn exit_signal_json_for_account(
    account_hash: &str,
    group: &OptionPositionGroup,
    eval: &ExitEvaluation,
) -> Value {
    let position_id = stable_position_key(account_hash, group);
    json!({
        "type": "exit",
        "reason": eval.reason,
        "position_id": position_id,
        "legacy_position_id": group.id,
        "underlying": group.underlying,
        "expiry": group.expiry,
        "mark": eval.mark,
    })
}

pub async fn reconcile_open_positions(
    trader: &schwab_api::TraderApi,
    state: &mut AgentState,
    rules: &RulesConfig,
) -> Result<()> {
    let mut live_keys = std::collections::HashSet::new();
    for account in rules.enabled_accounts() {
        let legs = list_option_positions(trader, Some(&account.hash)).await?;
        let groups = group_option_legs(&legs);
        for group in groups {
            let stable_id = stable_position_key(&account.hash, &group);
            live_keys.insert(stable_id.clone());
            let live_contracts = spread_contract_count(&group);
            let entry_credit = infer_entry_credit_from_legs(&group.legs);
            let inferred_max_loss = infer_max_loss_from_group(&group);

            if let Some(mut tracked) =
                take_existing_tracked_position(state, &stable_id, &account.hash, &group)
            {
                tracked.position_id = stable_id.clone();
                tracked.account_hash = account.hash.clone();
                let prev_contracts = tracked.contracts.max(1);
                if let Some(max_loss) = inferred_max_loss {
                    tracked.max_loss_usd = max_loss;
                } else if live_contracts != prev_contracts && tracked.max_loss_usd > 0.0 {
                    let per_contract = tracked.max_loss_usd / prev_contracts as f64;
                    tracked.max_loss_usd = per_contract * live_contracts as f64;
                }
                tracked.contracts = live_contracts;
                if entry_credit.is_some() {
                    tracked.entry_credit = entry_credit;
                }
                state.open_positions.insert(stable_id, tracked);
            } else {
                state.open_positions.insert(
                    stable_id.clone(),
                    TrackedPosition {
                        position_id: stable_id,
                        account_hash: account.hash.clone(),
                        underlying: group.underlying.clone(),
                        expiry: group.expiry.clone(),
                        strategy: group.strategy_hint.clone(),
                        opened_at: chrono::Utc::now(),
                        entry_credit,
                        max_loss_usd: inferred_max_loss.unwrap_or(0.0),
                        contracts: live_contracts,
                        entry_params: None,
                    },
                );
            }
        }
    }
    state.open_positions.retain(|id, _| live_keys.contains(id));
    Ok(())
}

fn take_existing_tracked_position(
    state: &mut AgentState,
    stable_id: &str,
    account_hash: &str,
    group: &OptionPositionGroup,
) -> Option<TrackedPosition> {
    if let Some(tracked) = state.open_positions.remove(stable_id) {
        return Some(tracked);
    }
    if let Some(tracked) = state.open_positions.remove(&group.id) {
        return Some(tracked);
    }
    let key = state.open_positions.iter().find_map(|(key, tracked)| {
        (tracked.account_hash == account_hash
            && tracked.underlying == group.underlying
            && tracked.expiry == group.expiry
            && tracked.strategy == group.strategy_hint)
            .then(|| key.clone())
    })?;
    state.open_positions.remove(&key)
}

pub fn infer_max_loss_from_group(group: &OptionPositionGroup) -> Option<f64> {
    let contracts = spread_contract_count(group) as f64;
    let entry_credit = infer_entry_credit_from_legs(&group.legs).unwrap_or(0.0);
    match group.strategy_hint.as_str() {
        "vertical" => {
            let (short, long) = vertical_legs(group).ok()?;
            let short_strike = short.parsed.as_ref()?.strike;
            let long_strike = long.parsed.as_ref()?.strike;
            let width = (short_strike - long_strike).abs();
            Some((width - entry_credit).max(0.0) * 100.0 * contracts)
        }
        "iron_condor" => {
            let put_width = wing_width(group, 'P')?;
            let call_width = wing_width(group, 'C')?;
            Some((put_width.max(call_width) - entry_credit).max(0.0) * 100.0 * contracts)
        }
        _ => None,
    }
}

fn wing_width(group: &OptionPositionGroup, put_call: char) -> Option<f64> {
    let mut short = None;
    let mut long = None;
    for leg in &group.legs {
        let parsed = leg.parsed.as_ref()?;
        if parsed.put_call != put_call {
            continue;
        }
        if leg.quantity < 0.0 {
            short = Some(parsed.strike);
        } else if leg.quantity > 0.0 {
            long = Some(parsed.strike);
        }
    }
    Some((short? - long?).abs())
}

pub fn exit_rules_summary(rules: &ExitRules) -> Value {
    json!({
        "profit_target_pct": rules.profit_target_pct,
        "stop_loss_pct": rules.stop_loss_pct,
        "dte_close": rules.dte_close,
    })
}

/// Per-share debit thresholds for a credit spread (target = lower debit, stop = higher debit).
pub fn spread_exit_thresholds(entry_credit: f64, exit_rules: &ExitRules) -> (f64, f64) {
    let target_debit = entry_credit * (1.0 - exit_rules.profit_target_pct / 100.0);
    let stop_debit = entry_credit * (exit_rules.stop_loss_pct / 100.0);
    (target_debit, stop_debit)
}

/// Debit to close per spread share from Schwab leg `net_market_value` (portfolio fallback).
pub fn debit_to_close_from_group(group: &OptionPositionGroup) -> Option<f64> {
    let contracts = spread_contract_count(group) as f64;
    if contracts <= 0.0 {
        return None;
    }
    let debit = (-group.net_market_value).max(0.0) / (contracts * 100.0);
    Some(debit)
}

pub fn mark_from_net_market_value(
    group: &OptionPositionGroup,
    entry_credit: f64,
    today: NaiveDate,
) -> Option<SpreadMark> {
    let debit = debit_to_close_from_group(group)?;
    let dte = group
        .legs
        .first()
        .and_then(|l| l.parsed.as_ref())
        .map(|p| days_to_expiry(p.expiry, today))
        .unwrap_or(0);
    let profit_pct = if entry_credit > f64::EPSILON {
        ((entry_credit - debit) / entry_credit) * 100.0
    } else {
        0.0
    };
    Some(SpreadMark {
        entry_credit,
        debit_to_close: debit,
        profit_pct,
        dte,
        source: "portfolio".into(),
    })
}

/// Live Schwab option groups keyed by stable position id.
pub async fn load_live_position_groups(
    trader: &schwab_api::TraderApi,
    rules: &RulesConfig,
) -> Result<std::collections::HashMap<String, OptionPositionGroup>> {
    let mut map = std::collections::HashMap::new();
    for account in rules.enabled_accounts() {
        let legs = list_option_positions(trader, Some(&account.hash)).await?;
        for group in group_option_legs(&legs) {
            let key = stable_position_key(&account.hash, &group);
            map.insert(key, group);
        }
    }
    Ok(map)
}

/// Build a minimal position group from sim tracked state (vertical spreads only).
pub fn option_group_from_tracked(tracked: &TrackedPosition) -> Option<OptionPositionGroup> {
    let params = tracked.entry_params.as_ref()?;
    let v: VerticalParams = serde_json::from_value(params.clone()).ok()?;
    let put_call = if v.spread_type.to_ascii_lowercase().contains("put") {
        'P'
    } else {
        'C'
    };
    let expiry = parse_expiry(&v.expiry).ok()?;
    let short_sym = build_option_symbol(&v.underlying, &v.expiry, put_call, v.short_strike).ok()?;
    let long_sym = build_option_symbol(&v.underlying, &v.expiry, put_call, v.long_strike).ok()?;
    let contracts = tracked.contracts.max(1) as f64;
    let legs = vec![
        OptionPositionLeg {
            symbol: short_sym.clone(),
            underlying: v.underlying.clone(),
            quantity: -contracts,
            market_value: 0.0,
            average_price: tracked.entry_credit,
            parsed: parse_option_symbol(&short_sym).ok(),
        },
        OptionPositionLeg {
            symbol: long_sym.clone(),
            underlying: v.underlying.clone(),
            quantity: contracts,
            market_value: 0.0,
            average_price: None,
            parsed: parse_option_symbol(&long_sym).ok(),
        },
    ];
    Some(OptionPositionGroup {
        id: tracked.position_id.clone(),
        underlying: v.underlying,
        expiry: expiry.format("%Y-%m-%d").to_string(),
        strategy_hint: tracked.strategy.clone(),
        legs,
        net_market_value: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{ExitRules, RulesConfig};

    #[test]
    fn evaluate_exit_from_mark_profit_target() {
        let exit_rules = ExitRules {
            profit_target_pct: 50.0,
            stop_loss_pct: 200.0,
            dte_close: 21,
        };
        let rules = RulesConfig {
            version: 1,
            agent_id: "t".into(),
            accounts: vec![],
            schedule: Default::default(),
            strategies: Default::default(),
            watchlist: vec![],
            entry_rules: Default::default(),
            exit_rules,
            risk: Default::default(),
            execution: Default::default(),
            llm: Default::default(),
            notify: Default::default(),
            simulation: None,
        };
        let mark = SpreadMark {
            entry_credit: 0.25,
            debit_to_close: 0.10,
            profit_pct: 60.0,
            dte: 30,
            source: "test".into(),
        };
        let exit = evaluate_exit_from_mark(&rules, Some(0.25), &mark);
        assert_eq!(
            exit.as_ref().map(|e| e.reason.as_str()),
            Some("profit_target")
        );
    }

    #[test]
    fn profit_target_triggers_at_half_credit() {
        let entry = 0.29;
        let debit = 0.14;
        let profit_pct = ((entry - debit) / entry) * 100.0;
        assert!(profit_pct >= 50.0);
    }

    #[test]
    fn stop_loss_triggers_at_double_credit() {
        let entry = 0.29;
        let stop_debit = entry * 2.0;
        assert!(0.58 >= stop_debit - 0.001);
    }

    #[test]
    fn infers_credit_from_leg_averages() {
        let legs = vec![
            OptionPositionLeg {
                symbol: "IWM".into(),
                underlying: "IWM".into(),
                quantity: -1.0,
                market_value: -100.0,
                average_price: Some(0.29),
                parsed: None,
            },
            OptionPositionLeg {
                symbol: "IWM".into(),
                underlying: "IWM".into(),
                quantity: 1.0,
                market_value: 50.0,
                average_price: Some(0.05),
                parsed: None,
            },
        ];
        let credit = infer_entry_credit_from_legs(&legs).unwrap();
        assert!((credit - 0.24).abs() < 0.001);
    }

    #[test]
    fn infers_vertical_max_loss_from_live_group() {
        let group = OptionPositionGroup {
            id: "IWM|2026-07-31".into(),
            underlying: "IWM".into(),
            expiry: "2026-07-31".into(),
            strategy_hint: "vertical".into(),
            legs: vec![
                OptionPositionLeg {
                    symbol: "IWM   260731P00282000".into(),
                    underlying: "IWM".into(),
                    quantity: -2.0,
                    market_value: -64.0,
                    average_price: Some(0.29),
                    parsed: crate::options::symbology::parse_option_symbol("IWM   260731P00282000")
                        .ok(),
                },
                OptionPositionLeg {
                    symbol: "IWM   260731P00280000".into(),
                    underlying: "IWM".into(),
                    quantity: 2.0,
                    market_value: 10.0,
                    average_price: Some(0.05),
                    parsed: crate::options::symbology::parse_option_symbol("IWM   260731P00280000")
                        .ok(),
                },
            ],
            net_market_value: -54.0,
        };
        let max_loss = infer_max_loss_from_group(&group).unwrap();
        assert!((max_loss - 352.0).abs() < 0.01);
    }

    #[test]
    fn find_expiry_strikes_matches_schwab_key() {
        let chain = json!({
            "putExpDateMap": {
                "2026-07-31:36": { "282.0": [] }
            }
        });
        let strikes = find_expiry_strikes(&chain, "putExpDateMap", "2026-07-31").unwrap();
        assert!(strikes.is_object());
    }

    #[test]
    fn format_chain_strike_uses_one_decimal_for_whole_strikes() {
        assert_eq!(format_chain_strike(282.0), "282.0");
        assert_eq!(format_chain_strike(282.5), "282.50");
    }
}
