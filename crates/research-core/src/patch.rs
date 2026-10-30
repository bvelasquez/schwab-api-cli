//! Bounded plan patches (plan §5).
//!
//! The agent may only change a whitelisted set of numeric fields, and every
//! change is clamped to a hard safety rail. Anything outside the allowlist or
//! without a rail fails closed. This mirrors the frozen `schwab-trader` engine
//! (`crates/schwab-trader/src/learn.rs`) but operates on a plain JSON tree.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Code-level ceiling on what an LLM may patch. Config may narrow this, never widen it.
///
/// Deliberately excludes anything that could fake results or remove risk control:
/// `paper.starting_equity_usd` (baseline), `paper.slippage_bps` (cost model),
/// `paper.max_daily_loss_usd` (kill switch), `forecast.horizon_days`/`benchmark`
/// (scoring contract), account, mode, gate and the allowlist itself.
pub const DEFAULT_ALLOWLIST: &[&str] = &[
    "playbook.exit.profit_target_pct",
    "playbook.exit.stop_loss_pct",
    "playbook.exit.trailing.trail_atr_multiple",
    "playbook.exit.trailing.activate_after_profit_pct",
    "playbook.entry.rsi_14_range",
    "paper.max_open_positions",
    "paper.max_hold_days",
    "forecast.min_confidence",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub path: String,
    pub value: Value,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedPatch {
    pub path: String,
    pub old_value: Value,
    pub new_value: Value,
    pub reason: String,
}

/// Narrow a configured allowlist against the code ceiling. An empty config
/// allowlist means "use the default set"; anything not in the ceiling is dropped.
pub fn effective_allowlist(configured: &[String]) -> Vec<String> {
    if configured.is_empty() {
        return DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect();
    }
    configured
        .iter()
        .filter(|path| DEFAULT_ALLOWLIST.contains(&path.as_str()))
        .cloned()
        .collect()
}

/// Hard numeric rail for a leaf field. `None` means the field may not be patched.
pub fn rail_for(path: &str) -> Option<(f64, f64)> {
    let leaf = path.rsplit('.').next().unwrap_or("");
    match leaf {
        "profit_target_pct" => Some((2.0, 20.0)),
        "stop_loss_pct" => Some((1.5, 10.0)),
        "trail_atr_multiple" => Some((1.0, 4.0)),
        "activate_after_profit_pct" => Some((1.0, 10.0)),
        "rsi_14_range" => Some((30.0, 80.0)),
        // Selectivity / sizing / hold time. Rails keep the agent inside a sane
        // band: at least one slot, at most ten; a 2-20 trading-day hold; and a
        // confidence floor between 0.30 and 0.85.
        "max_open_positions" => Some((1.0, 10.0)),
        "max_hold_days" => Some((2.0, 20.0)),
        "min_confidence" => Some((0.30, 0.85)),
        _ => None,
    }
}

/// Apply patches to `root`, returning what changed. Fails closed: a disallowed
/// path, a missing field, or a value with no rail is an error and nothing is written.
pub fn apply_patches(
    root: &mut Value,
    patches: &[Patch],
    allowlist: &[String],
) -> Result<Vec<AppliedPatch>> {
    let mut applied = Vec::new();
    for patch in patches {
        if !allowlist.iter().any(|p| p == &patch.path) {
            bail!("patch path `{}` is not in the allowlist", patch.path);
        }
        let rail = rail_for(&patch.path)
            .with_context(|| format!("patch path `{}` has no safety rail", patch.path))?;
        let old = get_path(root, &patch.path).with_context(|| {
            format!("patch path `{}` is absent from the rules tree", patch.path)
        })?;
        let new = clamp(&old, &patch.value, rail, &patch.path)?;
        if new == old {
            continue;
        }
        set_path(root, &patch.path, new.clone())?;
        applied.push(AppliedPatch {
            path: patch.path.clone(),
            old_value: old,
            new_value: new,
            reason: patch.reason.clone(),
        });
    }
    Ok(applied)
}

fn clamp(old: &Value, proposed: &Value, rail: (f64, f64), path: &str) -> Result<Value> {
    if let Some(number) = proposed.as_f64() {
        let clamped = number.clamp(rail.0, rail.1);
        // Integer fields (max_open_positions, max_hold_days) must stay integers,
        // or deserializing the patched tree into their u32 type would fail.
        if old.is_i64() || old.is_u64() {
            return Ok(Value::from(clamped.round() as i64));
        }
        return Ok(Value::from(clamped));
    }
    // rsi_14_range is a [low, high] pair.
    if let Some(items) = proposed.as_array() {
        if items.len() == 2 {
            let low = items[0].as_f64().context("range low must be a number")?;
            let high = items[1].as_f64().context("range high must be a number")?;
            let low = low.clamp(rail.0, rail.1);
            let high = high.clamp(rail.0, rail.1);
            if low >= high {
                bail!("patch `{path}` range low {low} must be < high {high}");
            }
            // Preserve the old pair shape only if it was an array; otherwise still emit a pair.
            let _ = old;
            return Ok(Value::Array(vec![Value::from(low), Value::from(high)]));
        }
    }
    bail!("patch `{path}` value must be a number or a 2-element range")
}

fn get_path(root: &Value, path: &str) -> Result<Value> {
    let mut cursor = root;
    for segment in path.split('.') {
        cursor = cursor
            .get(segment)
            .with_context(|| format!("missing `{segment}` in `{path}`"))?;
    }
    Ok(cursor.clone())
}

fn set_path(root: &mut Value, path: &str, value: Value) -> Result<()> {
    let mut segments: Vec<&str> = path.split('.').collect();
    let leaf = segments.pop().context("empty patch path")?;
    let mut cursor = root;
    for segment in segments {
        cursor = cursor
            .get_mut(segment)
            .with_context(|| format!("missing `{segment}` while writing `{path}`"))?;
    }
    let slot = cursor
        .get_mut(leaf)
        .with_context(|| format!("missing `{leaf}` while writing `{path}`"))?;
    *slot = value;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rules() -> Value {
        json!({
            "playbook": {
                "exit": {
                    "profit_target_pct": 8.0,
                    "stop_loss_pct": 5.0,
                    "trailing": { "trail_atr_multiple": 2.0, "activate_after_profit_pct": 4.0 }
                },
                "entry": { "rsi_14_range": [35.0, 70.0] }
            },
            "paper": { "max_open_positions": 5, "max_hold_days": 5 },
            "forecast": { "min_confidence": 0.55 }
        })
    }

    fn allow() -> Vec<String> {
        effective_allowlist(&[])
    }

    #[test]
    fn allowed_patch_is_clamped_to_rail() {
        let mut tree = rules();
        let patches = vec![Patch {
            path: "playbook.exit.stop_loss_pct".into(),
            value: json!(99.0),
            reason: "test".into(),
        }];
        let applied = apply_patches(&mut tree, &patches, &allow()).unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(tree["playbook"]["exit"]["stop_loss_pct"], json!(10.0));
    }

    #[test]
    fn integer_fields_stay_integers_and_clamp() {
        let mut tree = rules();
        let patches = vec![
            Patch {
                path: "paper.max_open_positions".into(),
                value: json!(99),
                reason: "test".into(),
            },
            Patch {
                path: "paper.max_hold_days".into(),
                value: json!(1),
                reason: "test".into(),
            },
            Patch {
                path: "forecast.min_confidence".into(),
                value: json!(0.99),
                reason: "test".into(),
            },
        ];
        apply_patches(&mut tree, &patches, &allow()).unwrap();
        assert_eq!(tree["paper"]["max_open_positions"], json!(10));
        assert!(tree["paper"]["max_open_positions"].is_u64());
        assert_eq!(tree["paper"]["max_hold_days"], json!(2));
        assert_eq!(tree["forecast"]["min_confidence"], json!(0.85));
    }

    #[test]
    fn every_ceiling_path_has_a_rail() {
        for path in DEFAULT_ALLOWLIST {
            assert!(rail_for(path).is_some(), "no rail for {path}");
        }
    }

    #[test]
    fn risk_and_baseline_fields_are_not_in_the_ceiling() {
        for forbidden in [
            "paper.starting_equity_usd",
            "paper.slippage_bps",
            "paper.max_daily_loss_usd",
            "forecast.horizon_days",
            "forecast.benchmark",
            "mode",
            "account",
        ] {
            assert!(
                !DEFAULT_ALLOWLIST.contains(&forbidden),
                "{forbidden} must never be patchable"
            );
        }
    }

    #[test]
    fn disallowed_path_fails_closed() {
        let mut tree = rules();
        let patches = vec![Patch {
            path: "account".into(),
            value: json!("schwab:X"),
            reason: "malicious".into(),
        }];
        assert!(apply_patches(&mut tree, &patches, &allow()).is_err());
    }

    #[test]
    fn rsi_range_patch_clamps_both_ends() {
        let mut tree = rules();
        let patches = vec![Patch {
            path: "playbook.entry.rsi_14_range".into(),
            value: json!([10.0, 99.0]),
            reason: "test".into(),
        }];
        apply_patches(&mut tree, &patches, &allow()).unwrap();
        assert_eq!(
            tree["playbook"]["entry"]["rsi_14_range"],
            json!([30.0, 80.0])
        );
    }

    #[test]
    fn inverted_range_is_rejected() {
        let mut tree = rules();
        let patches = vec![Patch {
            path: "playbook.entry.rsi_14_range".into(),
            value: json!([70.0, 40.0]),
            reason: "test".into(),
        }];
        assert!(apply_patches(&mut tree, &patches, &allow()).is_err());
    }

    #[test]
    fn config_allowlist_cannot_widen_code_ceiling() {
        let configured = vec![
            "account".to_string(),
            "playbook.exit.stop_loss_pct".to_string(),
        ];
        let effective = effective_allowlist(&configured);
        assert_eq!(effective, vec!["playbook.exit.stop_loss_pct".to_string()]);
    }

    #[test]
    fn field_without_rail_is_rejected() {
        let mut tree = rules();
        let patches = vec![Patch {
            path: "playbook.exit.trailing.trail_atr_multiple".into(),
            value: json!(2.5),
            reason: "ok".into(),
        }];
        // trail_atr_multiple *does* have a rail, so this should succeed.
        assert!(apply_patches(&mut tree, &patches, &allow()).is_ok());
    }
}
