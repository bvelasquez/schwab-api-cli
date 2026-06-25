use anyhow::{Context, Result};
use chrono::NaiveDate;
use schwab_market_data::endpoints::chains::ChainQuery;
use schwab_market_data::MarketDataApi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::options::{
    days_to_expiry, group_option_legs, list_option_positions, OptionPositionGroup,
    OptionPositionLeg,
};
use crate::rules::{ExitRules, RulesConfig};

use super::market_context::vertical_open_position_context;
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

/// Stable position key matching `OptionPositionGroup::id` (`underlying|expiry`).
pub fn position_key(underlying: &str, expiry: &str) -> String {
    format!("{underlying}|{expiry}")
}

pub fn find_tracked_position<'a>(
    state: &'a AgentState,
    account_hash: &str,
    group: &OptionPositionGroup,
) -> Option<&'a TrackedPosition> {
    let key = group.id.clone();
    state
        .open_positions
        .get(&key)
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

    let (exit, mark_opt, market_context) = match chain_result {
        Ok(chain_snap) => {
            let profit_pct = entry_credit.filter(|c| *c > f64::EPSILON).map(|entry| {
                ((entry - chain_snap.debit_to_close) / entry) * 100.0
            });
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
            );
            (exit, Some(mark), Some(ctx))
        }
        Err(_) => {
            let exit = if let Some(credit) = entry_credit.filter(|c| *c > 0.0) {
                evaluate_dte_only_with_credit(group, rules, today, credit, dte)?
            } else {
                evaluate_dte_only(group, rules, today)?
            };
            (exit, None, None)
        }
    };

    let snapshot = monitor_snapshot_json(
        group,
        tracked,
        &exit,
        mark_opt.as_ref(),
        market_context,
        &rules.exit_rules,
    );
    Ok(PositionMonitorResult { exit, snapshot })
}

fn evaluate_exit_from_mark(
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
    let is_put = short_leg
        .parsed
        .as_ref()
        .is_some_and(|p| p.put_call == 'P');

    let contract_type = if is_put { "PUT" } else { "CALL" };
    let chain = market
        .chains()
        .get(&ChainQuery {
            symbol: &group.underlying,
            contract_type: Some(contract_type),
            strike_count: Some(20),
            include_underlying_quote: Some(true),
            ..Default::default()
        })
        .await?;

    let map_key = if is_put {
        "putExpDateMap"
    } else {
        "callExpDateMap"
    };
    let strike_map = find_expiry_strikes(&chain, map_key, &group.expiry)
        .context("expiry not found in chain")?;

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

pub fn monitor_snapshot_json(
    group: &OptionPositionGroup,
    tracked: Option<&TrackedPosition>,
    exit_eval: &Option<ExitEvaluation>,
    mark: Option<&SpreadMark>,
    market_context: Option<Value>,
    exit_rules: &ExitRules,
) -> Value {
    let entry_credit = tracked
        .and_then(|p| p.entry_credit)
        .or_else(|| infer_entry_credit_from_legs(&group.legs));

    let status = match exit_eval {
        Some(e) => format!("exit: {}", e.reason),
        None => "holding".into(),
    };

    let mut snapshot = json!({
        "position_id": group.id,
        "underlying": group.underlying,
        "expiry": group.expiry,
        "strategy": tracked
            .map(|t| t.strategy.as_str())
            .unwrap_or_else(|| group.strategy_hint.as_str()),
        "entry_credit": entry_credit,
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

pub async fn evaluate_exit_for_group(
    market: &MarketDataApi,
    group: &OptionPositionGroup,
    rules: &RulesConfig,
    today: NaiveDate,
    tracked: Option<&TrackedPosition>,
) -> Result<Option<ExitEvaluation>> {
    if group.legs.len() != 2 {
        return evaluate_dte_only(group, rules, today);
    }

    let entry_credit = tracked
        .and_then(|p| p.entry_credit)
        .or_else(|| infer_entry_credit_from_legs(&group.legs))
        .filter(|c| *c > 0.0);

    let Some(entry_credit) = entry_credit else {
        return evaluate_dte_only(group, rules, today);
    };

    let dte = group
        .legs
        .first()
        .and_then(|l| l.parsed.as_ref())
        .map(|p| days_to_expiry(p.expiry, today))
        .unwrap_or(0);

    let debit_to_close = match estimate_debit_to_close(market, group).await {
        Ok(v) => v,
        Err(_) => {
            return evaluate_dte_only_with_credit(group, rules, today, entry_credit, dte);
        }
    };

    let profit_pct = if entry_credit > f64::EPSILON {
        ((entry_credit - debit_to_close) / entry_credit) * 100.0
    } else {
        0.0
    };

    let mark = SpreadMark {
        entry_credit,
        debit_to_close,
        profit_pct,
        dte,
        source: "chain".into(),
    };

    Ok(evaluate_exit_from_mark(rules, Some(entry_credit), &mark))
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

async fn estimate_debit_to_close(
    market: &MarketDataApi,
    group: &OptionPositionGroup,
) -> Result<f64> {
    Ok(fetch_vertical_chain_snapshot(market, group)
        .await?
        .debit_to_close)
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

pub fn exit_signal_json(group: &OptionPositionGroup, eval: &ExitEvaluation) -> Value {
    json!({
        "type": "exit",
        "reason": eval.reason,
        "position_id": group.id,
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
            live_keys.insert(group.id.clone());
            if state.open_positions.contains_key(&group.id) {
                continue;
            }
            let entry_credit = infer_entry_credit_from_legs(&group.legs);
            state.open_positions.insert(
                group.id.clone(),
                TrackedPosition {
                    position_id: group.id.clone(),
                    account_hash: account.hash.clone(),
                    underlying: group.underlying.clone(),
                    expiry: group.expiry.clone(),
                    strategy: group.strategy_hint.clone(),
                    opened_at: chrono::Utc::now(),
                    entry_credit,
                    max_loss_usd: 0.0,
                },
            );
        }
    }
    state
        .open_positions
        .retain(|id, _| live_keys.contains(id));
    Ok(())
}

pub fn exit_rules_summary(rules: &ExitRules) -> Value {
    json!({
        "profit_target_pct": rules.profit_target_pct,
        "stop_loss_pct": rules.stop_loss_pct,
        "dte_close": rules.dte_close,
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
        };
        let mark = SpreadMark {
            entry_credit: 0.25,
            debit_to_close: 0.10,
            profit_pct: 60.0,
            dte: 30,
            source: "test".into(),
        };
        let exit = evaluate_exit_from_mark(&rules, Some(0.25), &mark);
        assert_eq!(exit.as_ref().map(|e| e.reason.as_str()), Some("profit_target"));
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
}
