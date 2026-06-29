//! LLM learn loop: context, bounded patch validation, YAML apply.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::state::TraderState;
use crate::journal;
use crate::rules::TraderRules;
use crate::sim;

const ADAPTABLE_PATHS: &[&str] = &[
    "playbook.exit.profit_target_pct",
    "playbook.exit.stop_loss_pct",
    "playbook.exit.trailing.trail_atr_multiple",
    "playbook.entry.rsi_14_range",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedPatch {
    pub path: String,
    pub old_value: Value,
    pub new_value: Value,
    pub reason: String,
}

pub fn should_run_learn(rules: &TraderRules, state: &TraderState) -> bool {
    if !rules.llm.enabled || !rules.llm.allow_rule_adaptation {
        return false;
    }
    if state.closed_trades_since_learn >= rules.llm.learn_min_closed_trades.max(1) {
        return true;
    }
    if rules.llm.learn_every_ticks > 0
        && state.closed_trades_since_learn > 0
        && state
            .tick_count
            .saturating_sub(state.last_learn_tick.unwrap_or(0))
            >= rules.llm.learn_every_ticks
    {
        return true;
    }
    false
}

pub fn build_learn_context(
    rules: &TraderRules,
    state: &TraderState,
    rules_path: &Path,
) -> Result<Value> {
    let journal_events = journal::read_recent(rules_path, 80)?;
    let closed: Vec<Value> = journal_events
        .iter()
        .filter(|e| {
            matches!(
                e.get("type").and_then(|v| v.as_str()),
                Some("sim_exit_filled") | Some("exit_filled")
            )
        })
        .cloned()
        .collect();

    Ok(json!({
        "phase": "learn",
        "playbook_style": rules.playbook.style,
        "adaptable_playbook": adaptable_playbook_snapshot(rules),
        "adaptation_bounds": rules.llm.adaptation_bounds,
        "immutable_fields": rules.llm.immutable_fields,
        "allowed_patch_paths": ADAPTABLE_PATHS,
        "patch_format": {
            "path": "dotted field path from allowed_patch_paths",
            "value": "new scalar or [low, high] for rsi_14_range",
            "reason": "cite trade ids and metrics"
        },
        "recent_closed_trades": closed,
        "sim_stats": sim::compute_stats(state),
        "trades_today": state.trades_today,
        "tick_count": state.tick_count,
        "closed_trades_since_learn": state.closed_trades_since_learn,
    }))
}

pub fn adaptable_playbook_snapshot(rules: &TraderRules) -> Value {
    json!({
        "exit": {
            "profit_target_pct": rules.playbook.exit.profit_target_pct,
            "stop_loss_pct": rules.playbook.exit.stop_loss_pct,
            "trailing": { "trail_atr_multiple": rules.playbook.exit.trailing.trail_atr_multiple },
            "time_stop_days": rules.playbook.exit.time_stop_days,
            "time_stop_minutes": rules.playbook.exit.time_stop_minutes,
        },
        "entry": {
            "rsi_14_range": rules.playbook.entry.rsi_14_range,
        },
    })
}

pub fn apply_rule_patches(
    rules: &mut TraderRules,
    patches: &[Value],
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
        if !ADAPTABLE_PATHS.contains(&path) {
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
        "playbook.entry.rsi_14_range" => json!(rules.playbook.entry.rsi_14_range),
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
        "playbook.entry.rsi_14_range" => {
            let arr = value.as_array().context("rsi_14_range array")?;
            anyhow::ensure!(arr.len() == 2, "rsi_14_range needs [low, high]");
            rules.playbook.entry.rsi_14_range = [
                arr[0].as_f64().context("low")?,
                arr[1].as_f64().context("high")?,
            ];
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
        "playbook.entry.rsi_14_range" => Some("rsi_14_range"),
        _ => None,
    };

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

pub fn adaptation_allowed(runtime_dry_run: bool, runtime_simulate: bool, rules: &TraderRules) -> bool {
    if runtime_dry_run {
        return false;
    }
    if runtime_simulate {
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
}
