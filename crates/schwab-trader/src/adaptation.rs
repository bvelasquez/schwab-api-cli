//! Regime profiles, effective playbook merge, monitor exit adjustments.

use anyhow::Result;
use schwab_api::TraderApi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::agent::llm::TraderLlmReview;
use crate::agent::state::{save_state, TraderState};
use crate::capital::exit_prices;
use crate::config::TraderRuntime;
use crate::market_ctx::MarketCtx;
use crate::journal;
use crate::orders::replace_oco_bracket;
use crate::regime::RegimeSnapshot;
use crate::rules::{IntradayConfig, PlaybookConfig, PositionSizeConfig, TraderRules};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaybookProfileOverrides {
    #[serde(default)]
    pub exit: Option<ProfileExitOverrides>,
    #[serde(default)]
    pub entry: Option<ProfileEntryOverrides>,
    #[serde(default)]
    pub intraday: Option<IntradayConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileExitOverrides {
    pub profit_target_pct: Option<f64>,
    pub stop_loss_pct: Option<f64>,
    pub time_stop_days: Option<u32>,
    pub time_stop_minutes: Option<u32>,
    #[serde(default)]
    pub trailing: Option<ProfileTrailingOverrides>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileTrailingOverrides {
    pub activate_after_profit_pct: Option<f64>,
    pub trail_atr_multiple: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileEntryOverrides {
    pub rsi_14_range: Option<[f64; 2]>,
    pub max_new_entries_per_day: Option<u32>,
    #[serde(default)]
    pub position_size: Option<ProfilePositionSizeOverrides>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfilePositionSizeOverrides {
    pub method: Option<String>,
    pub risk_per_trade_pct: Option<f64>,
    pub max_position_pct: Option<f64>,
    pub atr_baseline_pct: Option<f64>,
    pub atr_vol_scalar_min: Option<f64>,
    pub atr_vol_scalar_max: Option<f64>,
}

pub fn effective_rules(base: &TraderRules, state: &TraderState) -> TraderRules {
    let mut rules = base.clone();
    let profile_name = state
        .active_profile
        .as_deref()
        .unwrap_or(base.adaptation.default_profile.as_str());
    if let Some(profile) = base.adaptation.profiles.get(profile_name) {
        rules.playbook = apply_profile_overrides(&rules.playbook, &profile.overrides);
    }
    rules
}

pub fn apply_profile_overrides(
    base: &PlaybookConfig,
    overrides: &PlaybookProfileOverrides,
) -> PlaybookConfig {
    let mut pb = base.clone();
    if let Some(exit) = &overrides.exit {
        if let Some(v) = exit.profit_target_pct {
            pb.exit.profit_target_pct = v;
        }
        if let Some(v) = exit.stop_loss_pct {
            pb.exit.stop_loss_pct = v;
        }
        if let Some(v) = exit.time_stop_days {
            pb.exit.time_stop_days = v;
        }
        if let Some(v) = exit.time_stop_minutes {
            pb.exit.time_stop_minutes = v;
        }
        if let Some(tr) = &exit.trailing {
            if let Some(v) = tr.activate_after_profit_pct {
                pb.exit.trailing.activate_after_profit_pct = v;
            }
            if let Some(v) = tr.trail_atr_multiple {
                pb.exit.trailing.trail_atr_multiple = v;
            }
        }
    }
    if let Some(entry) = &overrides.entry {
        if let Some(v) = entry.rsi_14_range {
            pb.entry.rsi_14_range = v;
        }
        if let Some(v) = entry.max_new_entries_per_day {
            pb.entry.max_new_entries_per_day = v;
        }
        if let Some(ps) = &entry.position_size {
            merge_position_size(&mut pb.entry.position_size, ps);
        }
    }
    if let Some(intraday) = &overrides.intraday {
        pb.intraday = intraday.clone();
    }
    pb
}

fn merge_position_size(base: &mut PositionSizeConfig, patch: &ProfilePositionSizeOverrides) {
    if let Some(v) = &patch.method {
        base.method = v.clone();
    }
    if let Some(v) = patch.risk_per_trade_pct {
        base.risk_per_trade_pct = v;
    }
    if let Some(v) = patch.max_position_pct {
        base.max_position_pct = v;
    }
    if let Some(v) = patch.atr_baseline_pct {
        base.atr_baseline_pct = v;
    }
    if let Some(v) = patch.atr_vol_scalar_min {
        base.atr_vol_scalar_min = v;
    }
    if let Some(v) = patch.atr_vol_scalar_max {
        base.atr_vol_scalar_max = v;
    }
}

pub fn apply_regime_profile(state: &mut TraderState, rules: &TraderRules, regime: &RegimeSnapshot) {
    if !rules.adaptation.enabled || !rules.adaptation.regime_auto_select {
        return;
    }
    if !rules.adaptation.profiles.contains_key(&regime.recommended_profile) {
        return;
    }
    state.active_profile = Some(regime.recommended_profile.clone());
    state.active_profile_source = Some("regime".into());
    state.active_profile_reason = Some(format!(
        "regime={} vix={:?} rv_pctile={:.0}",
        regime.class, regime.vix, regime.realized_vol_percentile
    ));
    state.last_regime = Some(regime.to_json());
}

pub fn apply_llm_profile_selection(
    state: &mut TraderState,
    rules: &TraderRules,
    review: &TraderLlmReview,
) -> bool {
    if !rules.adaptation.enabled || !rules.adaptation.llm_profile_select {
        return false;
    }
    let Some(name) = review.profile_name.as_deref() else {
        return false;
    };
    let name = name.trim();
    if name.is_empty() || !rules.adaptation.profiles.contains_key(name) {
        return false;
    }
    state.active_profile = Some(name.to_string());
    state.active_profile_source = Some("llm".into());
    state.active_profile_reason = review
        .profile_reasoning
        .clone()
        .or_else(|| Some(format!("LLM selected profile {name}")));
    true
}

pub fn profile_catalog(rules: &TraderRules) -> Value {
    let profiles: HashMap<String, Value> = rules
        .adaptation
        .profiles
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                json!({
                    "description": v.description,
                    "overrides": v.overrides,
                }),
            )
        })
        .collect();
    json!({
        "default_profile": rules.adaptation.default_profile,
        "profile_map": rules.adaptation.profile_map,
        "profiles": profiles,
    })
}

pub async fn apply_monitor_exit_adjustments(
    runtime: &TraderRuntime,
    rules_path: &Path,
    rules: &TraderRules,
    state: &mut TraderState,
    api: &Arc<TraderApi>,
    market: &MarketCtx,
    account_hash: &str,
    review: &TraderLlmReview,
) -> Result<Vec<Value>> {
    let cfg = &rules.adaptation.monitor_adjustments;
    if !rules.adaptation.enabled || !cfg.enabled || review.positions.is_empty() {
        return Ok(vec![]);
    }

    let mut updates = Vec::new();
    for pos_review in &review.positions {
        let action = pos_review.recommendation.to_ascii_lowercase();
        if action != "tighten_exits" && action != "widen_exits" {
            continue;
        }
        let Some(pos) = state.open_positions.get(&pos_review.position_id).cloned() else {
            continue;
        };
        let Some(oco_id) = pos.oco_order_id.clone() else {
            continue;
        };

        let snap = crate::technical::fetch_technical_snapshot(market, rules, &pos.symbol).await?;
        let last = snap.last;
        if last <= 0.0 {
            continue;
        }

        let (_, base_stop, _) = exit_prices(pos.entry_price, rules);
        let stop_range = (pos.entry_price - base_stop).max(0.01);
        let delta_pct = if action == "tighten_exits" {
            cfg.max_tighten_pct
        } else {
            -cfg.max_widen_pct
        };
        let mut new_stop = pos.stop_price + stop_range * (delta_pct / 100.0);
        let min_stop = last * (1.0 - cfg.min_stop_distance_from_price_pct / 100.0);
        let max_stop = last * (1.0 - cfg.max_stop_distance_from_price_pct / 100.0);
        new_stop = new_stop.clamp(max_stop, min_stop);

        if (new_stop - pos.stop_price).abs() < 0.01 {
            continue;
        }
        if action == "tighten_exits" && new_stop <= pos.stop_price {
            continue;
        }
        if action == "widen_exits" && new_stop >= pos.stop_price {
            continue;
        }

        let new_stop_limit = new_stop * 0.995;
        let bracket = replace_oco_bracket(
            runtime,
            api,
            account_hash,
            &oco_id,
            &pos.symbol,
            pos.quantity,
            pos.profit_limit,
            new_stop,
            new_stop_limit,
            &rules.execution.oco_duration,
        )
        .await?;

        let new_oco_id = bracket
            .order
            .get("order_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        if let Some(p) = state.open_positions.get_mut(&pos.position_id) {
            p.stop_price = new_stop;
            p.oco_order_id = new_oco_id.clone();
            p.exit_plan_version += 1;
        }

        let event = json!({
            "symbol": pos.symbol,
            "position_id": pos.position_id,
            "action": action,
            "old_stop": pos.stop_price,
            "new_stop": new_stop,
            "urgency": pos_review.urgency,
            "reasoning": pos_review.reasoning,
            "oco_order_id": new_oco_id,
        });
        updates.push(event.clone());
        journal::append_event(rules_path, "monitor_exit_adjusted", event)?;
        save_state(rules_path, state)?;
    }
    Ok(updates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{AdaptationConfig, TraderRules, TradingProfile};

    fn sample_rules() -> TraderRules {
        let mut rules = TraderRules::default();
        rules.trader_id = "test".into();
        rules.accounts = vec![crate::rules::TraderAccount {
            hash: "abc".into(),
            label: None,
            r#type: crate::rules::AccountType::Margin,
            enabled: true,
        }];
        rules.adaptation = AdaptationConfig::default_swing();
        rules
    }

    #[test]
    fn profile_overrides_merge_into_playbook() {
        let mut rules = sample_rules();
        rules.playbook.exit.profit_target_pct = 8.0;
        let profile = TradingProfile {
            description: "test".into(),
            overrides: PlaybookProfileOverrides {
                exit: Some(ProfileExitOverrides {
                    profit_target_pct: Some(10.0),
                    stop_loss_pct: Some(3.5),
                    ..Default::default()
                }),
                entry: Some(ProfileEntryOverrides {
                    max_new_entries_per_day: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        rules
            .adaptation
            .profiles
            .insert("low_vol_trend".into(), profile);

        let mut state = TraderState::default();
        state.active_profile = Some("low_vol_trend".into());
        let effective = effective_rules(&rules, &state);
        assert!((effective.playbook.exit.profit_target_pct - 10.0).abs() < 0.01);
        assert_eq!(effective.playbook.entry.max_new_entries_per_day, 0);
        assert!((rules.playbook.exit.profit_target_pct - 8.0).abs() < 0.01);
    }
}
