//! LLM learn loop: context, bounded patch validation, YAML apply.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::adaptation::profile_catalog;
use crate::agent::state::TraderState;
use crate::journal;
use crate::rules::TraderRules;
use crate::sim;

/// Narrow learn loop — exit tuning and RSI only (no sizing/method/entry caps).
pub const LEARN_ADAPTABLE_PATHS: &[&str] = &[
    "playbook.exit.profit_target_pct",
    "playbook.exit.stop_loss_pct",
    "playbook.exit.trailing.trail_atr_multiple",
    "playbook.exit.trailing.activate_after_profit_pct",
    "playbook.entry.rsi_14_range",
];

/// Drop placeholder `{}` entries the LLM sometimes emits when it has no real patches.
pub fn valid_rule_patches(patches: &[Value]) -> Vec<Value> {
    patches
        .iter()
        .filter(|p| {
            p.get("path")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty())
                && p.get("value").is_some()
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedPatch {
    pub path: String,
    pub old_value: Value,
    pub new_value: Value,
    pub reason: String,
}

pub fn should_run_learn(rules: &TraderRules, state: &TraderState, backtest: bool) -> bool {
    if !rules.llm.enabled || !rules.llm.allow_rule_adaptation {
        return false;
    }
    let min_trades = if backtest {
        rules
            .llm
            .backtest_learn_min_closed_trades
            .max(rules.llm.learn_min_closed_trades)
    } else {
        rules.llm.learn_min_closed_trades.max(1)
    };
    if state.closed_trades_since_learn < min_trades {
        return false;
    }
    let cooldown = if backtest {
        rules
            .llm
            .backtest_learn_cooldown_ticks
            .max(rules.llm.learn_cooldown_ticks)
    } else {
        rules.llm.learn_cooldown_ticks
    };
    if cooldown > 0 {
        let since = state
            .tick_count
            .saturating_sub(state.last_learn_tick.unwrap_or(0));
        if since < cooldown {
            return false;
        }
    }
    true
}

pub fn build_learn_context(
    rules: &TraderRules,
    state: &TraderState,
    rules_path: &Path,
    backtest: bool,
) -> Result<Value> {
    let journal_events = if backtest {
        journal::read_all_backtest(rules_path)?
    } else {
        journal::read_recent(rules_path, 80)?
    };
    let closed: Vec<Value> = journal_events
        .iter()
        .filter(|e| {
            matches!(
                e.get("type").and_then(|v| v.as_str()),
                Some("sim_exit_filled") | Some("exit_filled")
            )
        })
        .map(|e| e.clone())
        .collect();
    let recent_closed: Vec<Value> = if backtest {
        closed.into_iter().rev().take(80).collect::<Vec<_>>().into_iter().rev().collect()
    } else {
        closed
    };

    Ok(json!({
        "phase": "learn",
        "playbook_style": rules.playbook.style,
        "adaptable_playbook": adaptable_playbook_snapshot(rules),
        "adaptation_bounds": rules.llm.adaptation_bounds,
        "immutable_fields": rules.llm.immutable_fields,
        "allowed_patch_paths": LEARN_ADAPTABLE_PATHS,
        "profile_catalog": profile_catalog(rules),
        "active_profile": state.active_profile,
        "last_regime": state.last_regime,
        "patch_format": {
            "path": "dotted field path from allowed_patch_paths",
            "value": "new scalar or [low, high] for rsi_14_range",
            "reason": "cite trade ids and metrics"
        },
        "recent_closed_trades": recent_closed,
        "sim_stats": sim::compute_stats(state),
        "trades_today": state.trades_today,
        "tick_count": state.tick_count,
        "closed_trades_since_learn": state.closed_trades_since_learn,
        "backtest": backtest,
    }))
}

pub fn adaptable_playbook_snapshot(rules: &TraderRules) -> Value {
    json!({
        "exit": {
            "profit_target_pct": rules.playbook.exit.profit_target_pct,
            "stop_loss_pct": rules.playbook.exit.stop_loss_pct,
            "trailing": {
                "trail_atr_multiple": rules.playbook.exit.trailing.trail_atr_multiple,
                "activate_after_profit_pct": rules.playbook.exit.trailing.activate_after_profit_pct,
            },
            "time_stop_days": rules.playbook.exit.time_stop_days,
            "time_stop_minutes": rules.playbook.exit.time_stop_minutes,
        },
        "entry": {
            "rsi_14_range": rules.playbook.entry.rsi_14_range,
            "max_new_entries_per_day": rules.playbook.entry.max_new_entries_per_day,
            "position_size": rules.playbook.entry.position_size,
        },
        "intraday": rules.playbook.intraday,
    })
}

pub fn apply_rule_patches(
    rules: &mut TraderRules,
    patches: &[Value],
) -> Result<Vec<AppliedPatch>> {
    apply_rule_patches_with_allowlist(rules, patches, LEARN_ADAPTABLE_PATHS)
}

pub fn apply_rule_patches_with_allowlist(
    rules: &mut TraderRules,
    patches: &[Value],
    allowed_paths: &[&str],
) -> Result<Vec<AppliedPatch>> {
    let mut applied = Vec::new();
    for patch in patches {
        let path = patch
            .get("path")
            .and_then(|v| v.as_str())
            .context("patch missing path")?;
        let value = patch.get("value").context("patch missing value")?;
        let reason = patch
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if rules.llm.immutable_fields.iter().any(|f| f == path) {
            continue;
        }
        if !allowed_paths.contains(&path) {
            continue;
        }

        let old = read_path(rules, path)?;
        let new = bound_value(rules, path, value, &old)?;
        if new == old {
            continue;
        }
        write_path(rules, path, &new)?;
        applied.push(AppliedPatch {
            path: path.to_string(),
            old_value: old,
            new_value: new,
            reason,
        });
    }
    rules.validate()?;
    Ok(applied)
}

fn read_path(rules: &TraderRules, path: &str) -> Result<Value> {
    let v = match path {
        "playbook.exit.profit_target_pct" => json!(rules.playbook.exit.profit_target_pct),
        "playbook.exit.stop_loss_pct" => json!(rules.playbook.exit.stop_loss_pct),
        "playbook.exit.trailing.trail_atr_multiple" => {
            json!(rules.playbook.exit.trailing.trail_atr_multiple)
        }
        "playbook.exit.trailing.activate_after_profit_pct" => {
            json!(rules.playbook.exit.trailing.activate_after_profit_pct)
        }
        "playbook.exit.time_stop_days" => json!(rules.playbook.exit.time_stop_days),
        "playbook.exit.time_stop_minutes" => json!(rules.playbook.exit.time_stop_minutes),
        "playbook.entry.rsi_14_range" => json!(rules.playbook.entry.rsi_14_range),
        "playbook.entry.position_size.risk_per_trade_pct" => {
            json!(rules.playbook.entry.position_size.risk_per_trade_pct)
        }
        "playbook.entry.position_size.method" => {
            json!(rules.playbook.entry.position_size.method)
        }
        "playbook.entry.max_new_entries_per_day" => {
            json!(rules.playbook.entry.max_new_entries_per_day)
        }
        "playbook.intraday.min_relative_volume" => {
            json!(rules.playbook.intraday.min_relative_volume)
        }
        "playbook.intraday.momentum_rsi_min" => json!(rules.playbook.intraday.momentum_rsi_min),
        _ => anyhow::bail!("unsupported path {path}"),
    };
    Ok(v)
}

fn write_path(rules: &mut TraderRules, path: &str, value: &Value) -> Result<()> {
    match path {
        "playbook.exit.profit_target_pct" => {
            rules.playbook.exit.profit_target_pct = value.as_f64().context("number")?;
        }
        "playbook.exit.stop_loss_pct" => {
            rules.playbook.exit.stop_loss_pct = value.as_f64().context("number")?;
        }
        "playbook.exit.trailing.trail_atr_multiple" => {
            rules.playbook.exit.trailing.trail_atr_multiple = value.as_f64().context("number")?;
        }
        "playbook.exit.trailing.activate_after_profit_pct" => {
            rules.playbook.exit.trailing.activate_after_profit_pct =
                value.as_f64().context("number")?;
        }
        "playbook.exit.time_stop_days" => {
            rules.playbook.exit.time_stop_days = value.as_u64().context("integer")? as u32;
        }
        "playbook.exit.time_stop_minutes" => {
            rules.playbook.exit.time_stop_minutes = value.as_u64().context("integer")? as u32;
        }
        "playbook.entry.rsi_14_range" => {
            let arr = value.as_array().context("rsi_14_range array")?;
            anyhow::ensure!(arr.len() == 2, "rsi_14_range needs [low, high]");
            rules.playbook.entry.rsi_14_range = [
                arr[0].as_f64().context("low")?,
                arr[1].as_f64().context("high")?,
            ];
        }
        "playbook.entry.position_size.risk_per_trade_pct" => {
            rules.playbook.entry.position_size.risk_per_trade_pct =
                value.as_f64().context("number")?;
        }
        "playbook.entry.position_size.method" => {
            rules.playbook.entry.position_size.method =
                value.as_str().context("string")?.to_string();
        }
        "playbook.entry.max_new_entries_per_day" => {
            rules.playbook.entry.max_new_entries_per_day =
                value.as_u64().context("integer")? as u32;
        }
        "playbook.intraday.min_relative_volume" => {
            rules.playbook.intraday.min_relative_volume = value.as_f64().context("number")?;
        }
        "playbook.intraday.momentum_rsi_min" => {
            rules.playbook.intraday.momentum_rsi_min = value.as_f64().context("number")?;
        }
        _ => anyhow::bail!("unsupported path {path}"),
    }
    Ok(())
}

fn bound_value(
    rules: &TraderRules,
    path: &str,
    proposed: &Value,
    current: &Value,
) -> Result<Value> {
    let bounds_key = match path {
        "playbook.exit.profit_target_pct" => Some("profit_target_pct"),
        "playbook.exit.stop_loss_pct" => Some("stop_loss_pct"),
        "playbook.exit.trailing.trail_atr_multiple" => Some("trail_atr_multiple"),
        "playbook.exit.trailing.activate_after_profit_pct" => Some("activate_after_profit_pct"),
        "playbook.exit.time_stop_days" => Some("time_stop_days"),
        "playbook.exit.time_stop_minutes" => Some("time_stop_minutes"),
        "playbook.entry.rsi_14_range" => Some("rsi_14_range"),
        "playbook.entry.position_size.risk_per_trade_pct" => Some("risk_per_trade_pct"),
        "playbook.entry.max_new_entries_per_day" => Some("max_new_entries_per_day"),
        "playbook.intraday.min_relative_volume" => Some("min_relative_volume"),
        "playbook.intraday.momentum_rsi_min" => Some("momentum_rsi_min"),
        "playbook.entry.position_size.method" => None,
        _ => None,
    };

    if path == "playbook.entry.position_size.method" {
        let method = proposed.as_str().context("method string")?;
        anyhow::ensure!(
            method == "risk_pct" || method == "atr_normalized",
            "method must be risk_pct or atr_normalized"
        );
        return Ok(json!(method));
    }

    if path == "playbook.entry.rsi_14_range" {
        let arr = proposed.as_array().context("rsi_14_range")?;
        anyhow::ensure!(arr.len() == 2);
        let mut low = arr[0].as_f64().context("low")?;
        let mut high = arr[1].as_f64().context("high")?;
        if let Some(b) = rules.llm.adaptation_bounds.get("rsi_14_range") {
            if let Some(v) = b.get("min_low").and_then(|x| x.as_f64()) {
                low = low.max(v);
            }
            if let Some(v) = b.get("max_low").and_then(|x| x.as_f64()) {
                low = low.min(v);
            }
            if let Some(v) = b.get("min_high").and_then(|x| x.as_f64()) {
                high = high.max(v);
            }
            if let Some(v) = b.get("max_high").and_then(|x| x.as_f64()) {
                high = high.min(v);
            }
        }
        anyhow::ensure!(low < high, "rsi low must be < high");
        return Ok(json!([low, high]));
    }

    if path == "playbook.entry.max_new_entries_per_day"
        || path == "playbook.exit.time_stop_days"
        || path == "playbook.exit.time_stop_minutes"
    {
        let new = proposed.as_u64().context("integer patch value")? as f64;
        let cur = current.as_u64().map(|v| v as f64).unwrap_or(new);
        let bounds = bounds_key.and_then(|k| rules.llm.adaptation_bounds.get(k));
        let min = bounds
            .and_then(|b| b.get("min"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let max = bounds
            .and_then(|b| b.get("max"))
            .and_then(|v| v.as_f64())
            .unwrap_or(new);
        let mut v = new.clamp(min, max);
        if let Some(delta) = bounds
            .and_then(|b| b.get("max_delta_per_change"))
            .and_then(|v| v.as_f64())
        {
            v = v.clamp(cur - delta, cur + delta);
            v = v.clamp(min, max);
        }
        return Ok(json!(v as u64));
    }

    let new = proposed.as_f64().context("numeric patch value")?;
    let cur = current.as_f64().unwrap_or(new);
    let Some(key) = bounds_key else {
        return Ok(json!(new));
    };
    let bounds = rules.llm.adaptation_bounds.get(key);
    let min = bounds
        .and_then(|b| b.get("min"))
        .and_then(|v| v.as_f64())
        .unwrap_or(new);
    let max = bounds
        .and_then(|b| b.get("max"))
        .and_then(|v| v.as_f64())
        .unwrap_or(new);
    let mut v = new.clamp(min, max);
    if let Some(delta) = bounds
        .and_then(|b| b.get("max_delta_per_change"))
        .and_then(|v| v.as_f64())
    {
        v = v.clamp(cur - delta, cur + delta);
        v = v.clamp(min, max);
    }
    Ok(json!(v))
}

pub fn adaptation_allowed(
    runtime_dry_run: bool,
    runtime_simulate: bool,
    rules: &TraderRules,
    backtest: bool,
) -> bool {
    if runtime_dry_run {
        return false;
    }
    if backtest || runtime_simulate {
        return rules
            .simulation
            .as_ref()
            .map(|s| s.allow_rule_adaptation)
            .unwrap_or(true);
    }
    rules.adaptation.live_auto_apply
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::state::TraderState;

    #[test]
    fn learn_throttle_requires_min_trades_and_cooldown() {
        let mut rules = TraderRules::default();
        rules.trader_id = "t".into();
        rules.accounts = vec![crate::rules::TraderAccount {
            hash: "a".into(),
            label: None,
            r#type: crate::rules::AccountType::Margin,
            enabled: true,
        }];
        rules.llm.enabled = true;
        rules.llm.allow_rule_adaptation = true;
        rules.llm.learn_min_closed_trades = 3;
        rules.llm.learn_cooldown_ticks = 10;

        let mut state = TraderState::default();
        state.closed_trades_since_learn = 2;
        assert!(!should_run_learn(&rules, &state, false));

        state.closed_trades_since_learn = 3;
        state.tick_count = 5;
        state.last_learn_tick = Some(0);
        assert!(!should_run_learn(&rules, &state, false));

        state.tick_count = 10;
        assert!(should_run_learn(&rules, &state, false));

        rules.llm.backtest_learn_min_closed_trades = 5;
        rules.llm.backtest_learn_cooldown_ticks = 30;
        state.closed_trades_since_learn = 4;
        state.tick_count = 100;
        assert!(!should_run_learn(&rules, &state, true));
    }

    #[test]
    fn bounds_limit_profit_target_delta() {
        let mut rules = TraderRules::default();
        rules.trader_id = "test".into();
        rules.accounts = vec![crate::rules::TraderAccount {
            hash: "abc".into(),
            label: None,
            r#type: crate::rules::AccountType::Margin,
            enabled: true,
        }];
        rules.playbook.exit.profit_target_pct = 8.0;
        rules.llm.adaptation_bounds = json!({
            "profit_target_pct": { "min": 5.0, "max": 12.0, "max_delta_per_change": 1.0 }
        });
        let applied = apply_rule_patches(
            &mut rules,
            &[json!({
                "path": "playbook.exit.profit_target_pct",
                "value": 11.0,
                "reason": "test"
            })],
        )
        .unwrap();
        assert_eq!(applied.len(), 1);
        assert!((rules.playbook.exit.profit_target_pct - 9.0).abs() < 0.01);
    }

    #[test]
    fn valid_rule_patches_drops_empty_placeholders() {
        let raw = vec![
            json!({}),
            json!({"path": "playbook.exit.profit_target_pct", "value": 7.5}),
            json!({"path": "", "value": 1.0}),
        ];
        let valid = valid_rule_patches(&raw);
        assert_eq!(valid.len(), 1);
        assert_eq!(
            valid[0].get("path").and_then(|v| v.as_str()),
            Some("playbook.exit.profit_target_pct")
        );
    }
}
