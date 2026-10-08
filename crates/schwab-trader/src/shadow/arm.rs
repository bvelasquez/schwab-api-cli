//! Arm definitions → effective TraderRules, per-arm state, and the loaded set.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::paths::{shadow_journal_path, shadow_state_path};
use crate::agent::state::TraderState;
use crate::rules::{ShadowArmConfig, ShadowConfig, TraderRules};

/// Maps merge recursively; arrays, scalars, and nulls replace.
pub fn merge_overrides(base: &mut Value, overrides: &Value) {
    match (base, overrides) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) => merge_overrides(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

fn validate_arm_id(id: &str) -> Result<()> {
    anyhow::ensure!(!id.is_empty(), "shadow arm id is required");
    anyhow::ensure!(
        id.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'),
        "shadow arm id `{id}` must be lowercase [a-z0-9_-] (used in file names)"
    );
    Ok(())
}

/// Absolute paths as-is; relative paths resolve from the working directory
/// when that file exists (e.g. `rules/arms/x.yaml` from the repo root), else
/// from the production rules file's directory.
pub fn resolve_arm_rules_file(rules_path: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel.trim());
    if p.is_absolute() || p.is_file() {
        return p.to_path_buf();
    }
    rules_path
        .parent()
        .map(|d| d.join(p))
        .unwrap_or_else(|| p.to_path_buf())
}

/// The arm's effective rules: `rules_file` (else production) with `overrides`
/// deep-merged on top, re-validated like a freshly loaded rules file.
pub fn build_arm_rules(
    production: &TraderRules,
    rules_path: &Path,
    arm: &ShadowArmConfig,
) -> Result<TraderRules> {
    validate_arm_id(&arm.id)?;
    let mut base = match arm.rules_file.as_deref().filter(|f| !f.trim().is_empty()) {
        Some(file) => {
            let path = resolve_arm_rules_file(rules_path, file);
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read arm rules file {}", path.display()))?;
            crate::rules::parse_yaml_merged::<Value>(&raw)
                .with_context(|| format!("parse arm rules file {}", path.display()))?
        }
        None => serde_json::to_value(production).context("serialize production rules")?,
    };
    if let Some(obj) = base.as_object_mut() {
        obj.remove("shadow");
    }
    if !arm.overrides.is_null() {
        merge_overrides(&mut base, &arm.overrides);
    }
    let mut rules: TraderRules =
        serde_json::from_value(base).context("arm rules do not deserialize as TraderRules")?;
    rules.shadow = ShadowConfig::default();
    rules.normalize_adaptation();
    rules
        .validate_shadow_arm()
        .context("arm rules failed validation")?;
    Ok(rules)
}

/// Arm `rules_file`s to watch alongside the production rules for reloads.
pub fn arm_watch_paths(rules: &TraderRules, rules_path: &Path) -> Vec<PathBuf> {
    if !rules.shadow.enabled {
        return vec![];
    }
    rules
        .shadow
        .arms
        .iter()
        .filter_map(|a| a.rules_file.as_deref())
        .filter(|f| !f.trim().is_empty())
        .map(|f| resolve_arm_rules_file(rules_path, f))
        .collect()
}

/// Running totals for the current trading day, flushed as `shadow_day_summary`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowDay {
    pub date: NaiveDate,
    pub arm_start_equity_usd: f64,
    pub arm_last_equity_usd: f64,
    pub prod_start_equity_usd: f64,
    pub prod_last_equity_usd: f64,
    #[serde(default)]
    pub entries: u32,
    #[serde(default)]
    pub exits: u32,
    #[serde(default)]
    pub realized_pnl_usd: f64,
    #[serde(default)]
    pub rejections: BTreeMap<String, u32>,
}

impl ShadowDay {
    pub fn new(date: NaiveDate, arm_equity: f64, prod_equity: f64) -> Self {
        Self {
            date,
            arm_start_equity_usd: arm_equity,
            arm_last_equity_usd: arm_equity,
            prod_start_equity_usd: prod_equity,
            prod_last_equity_usd: prod_equity,
            entries: 0,
            exits: 0,
            realized_pnl_usd: 0.0,
            rejections: BTreeMap::new(),
        }
    }

    pub fn summary_payload(&self, open_positions: usize, active_profile: Option<&str>) -> Value {
        let mut top: Vec<(&String, &u32)> = self.rejections.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let top: Vec<Value> = top
            .into_iter()
            .take(5)
            .map(|(code, count)| json!({ "reason_code": code, "count": count }))
            .collect();
        json!({
            "date": self.date.to_string(),
            "equity_usd": self.arm_last_equity_usd,
            "arm_start_equity_usd": self.arm_start_equity_usd,
            "arm_equity_change_usd": self.arm_last_equity_usd - self.arm_start_equity_usd,
            "prod_start_equity_usd": self.prod_start_equity_usd,
            "prod_end_equity_usd": self.prod_last_equity_usd,
            "prod_equity_change_usd": self.prod_last_equity_usd - self.prod_start_equity_usd,
            "open_positions": open_positions,
            "entries": self.entries,
            "exits": self.exits,
            "realized_pnl_usd": self.realized_pnl_usd,
            "active_profile": active_profile,
            "top_rejections": top,
        })
    }
}

/// On-disk arm state: the arm's own paper `TraderState` plus day totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShadowArmState {
    pub arm_id: String,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub day: Option<ShadowDay>,
    #[serde(default)]
    pub trader: TraderState,
}

impl ShadowArmState {
    pub fn load(path: &Path, arm_id: &str, trader_id: &str) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self {
                arm_id: arm_id.to_string(),
                trader: TraderState {
                    trader_id: trader_id.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).with_context(|| format!("parse shadow state {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_json::to_string_pretty(self)?;
        schwab_api::write_atomic_owner_sync(path, raw)
            .with_context(|| format!("write shadow state {}", path.display()))?;
        Ok(())
    }
}

pub struct ShadowArm {
    pub id: String,
    pub rules: TraderRules,
    pub state: ShadowArmState,
    pub state_path: PathBuf,
    pub journal_path: PathBuf,
}

/// Arms built from the production rules' `shadow` section. Rebuilt whenever
/// the production rules change (reload or learn patch) or `mark_dirty` is
/// called; a broken arm is logged and skipped, never fatal.
#[derive(Default)]
pub struct ShadowArms {
    pub(crate) arms: Vec<ShadowArm>,
    built_from: Option<u64>,
    dirty: bool,
}

impl ShadowArms {
    pub fn is_active(&self) -> bool {
        !self.arms.is_empty()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn ids(&self) -> Vec<&str> {
        self.arms.iter().map(|a| a.id.as_str()).collect()
    }

    pub fn sync(&mut self, rules_path: &Path, production: &TraderRules) {
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(production)
                .unwrap_or_default()
                .hash(&mut h);
            h.finish()
        };
        if !self.dirty && self.built_from == Some(key) {
            return;
        }
        self.dirty = false;
        self.built_from = Some(key);

        let mut previous: HashMap<String, ShadowArmState> =
            self.arms.drain(..).map(|a| (a.id, a.state)).collect();
        let had_arms = !previous.is_empty();

        if production.shadow.enabled {
            for cfg in &production.shadow.arms {
                if self.arms.iter().any(|a| a.id == cfg.id) {
                    shadow_warn(rules_path, &cfg.id, "duplicate arm id — skipped");
                    continue;
                }
                let rules = match build_arm_rules(production, rules_path, cfg) {
                    Ok(r) => r,
                    Err(err) => {
                        shadow_warn(rules_path, &cfg.id, &format!("disabled: {err:#}"));
                        continue;
                    }
                };
                let state_path = shadow_state_path(rules_path, &production.trader_id, &cfg.id);
                let state = match previous.remove(&cfg.id) {
                    Some(s) => s,
                    None => match ShadowArmState::load(&state_path, &cfg.id, &production.trader_id)
                    {
                        Ok(s) => s,
                        Err(err) => {
                            shadow_warn(rules_path, &cfg.id, &format!("disabled: {err:#}"));
                            continue;
                        }
                    },
                };
                self.arms.push(ShadowArm {
                    id: cfg.id.clone(),
                    rules,
                    state,
                    state_path,
                    journal_path: shadow_journal_path(rules_path, &production.trader_id, &cfg.id),
                });
            }
        }

        if had_arms || self.is_active() {
            let msg = format!("shadow arms active: [{}]", self.ids().join(", "));
            tracing::info!("{msg}");
            let _ = crate::agent::paths::append_trader_log(rules_path, &msg);
        }
    }
}

pub(crate) fn shadow_warn(rules_path: &Path, arm_id: &str, msg: &str) {
    let line = format!("shadow arm `{arm_id}`: {msg}");
    tracing::warn!("{line}");
    let _ = crate::agent::paths::append_trader_log(rules_path, &line);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn production() -> TraderRules {
        let mut rules = TraderRules::default();
        rules.trader_id = "swing".into();
        rules.accounts = vec![crate::rules::TraderAccount {
            hash: "abc".into(),
            label: None,
            r#type: crate::rules::AccountType::Margin,
            enabled: true,
        }];
        rules.playbook.filters.blocked_symbols = vec!["JPM".into()];
        rules
    }

    #[test]
    fn merge_maps_recursively_and_replaces_arrays_and_scalars() {
        let mut base = json!({"a": {"b": 1, "c": [1, 2]}, "d": 5});
        merge_overrides(&mut base, &json!({"a": {"c": [9], "e": true}, "d": null}));
        assert_eq!(base, json!({"a": {"b": 1, "c": [9], "e": true}, "d": null}));
    }

    #[test]
    fn overlay_arm_inherits_production_and_strips_shadow() {
        let mut prod = production();
        prod.shadow.enabled = true;
        prod.shadow.arms = vec![ShadowArmConfig {
            id: "x".into(),
            ..Default::default()
        }];
        let arm = ShadowArmConfig {
            id: "x".into(),
            rules_file: None,
            overrides: json!({"playbook": {"entry": {"max_positions": 1}}}),
        };
        let rules = build_arm_rules(&prod, Path::new("/tmp/rules/t.yaml"), &arm).unwrap();
        assert_eq!(rules.playbook.entry.max_positions, 1);
        assert_eq!(
            rules.playbook.filters.blocked_symbols,
            vec!["JPM".to_string()]
        );
        assert!(rules.shadow.is_empty());
        assert_eq!(rules.trader_id, "swing");
    }

    #[test]
    fn broken_overlay_is_an_error_not_a_panic() {
        let arm = ShadowArmConfig {
            id: "bad".into(),
            rules_file: None,
            overrides: json!({"playbook": {"entry": {"max_positions": "lots"}}}),
        };
        assert!(build_arm_rules(&production(), Path::new("/tmp/t.yaml"), &arm).is_err());
        let bad_id = ShadowArmConfig {
            id: "Bad/Id".into(),
            ..Default::default()
        };
        assert!(build_arm_rules(&production(), Path::new("/tmp/t.yaml"), &bad_id).is_err());
    }

    #[test]
    fn sync_skips_broken_arm_and_keeps_good_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let rules_path = dir.path().join("trader-swing.yaml");
        let mut prod = production();
        prod.shadow.enabled = true;
        prod.shadow.arms = vec![
            ShadowArmConfig {
                id: "good".into(),
                rules_file: None,
                overrides: json!({"playbook": {"entry": {"max_positions": 2}}}),
            },
            ShadowArmConfig {
                id: "missing-file".into(),
                rules_file: Some("does-not-exist.yaml".into()),
                overrides: Value::Null,
            },
        ];
        let mut arms = ShadowArms::default();
        arms.sync(&rules_path, &prod);
        assert_eq!(arms.ids(), vec!["good"]);
        assert!(arms.arms[0]
            .state_path
            .ends_with("trader-shadow-state-swing-good.json"));

        prod.shadow.enabled = false;
        arms.sync(&rules_path, &prod);
        assert!(!arms.is_active());
    }

    /// Leaf paths where two JSON values differ (objects recurse on the key union).
    fn diff_paths(a: &Value, b: &Value, prefix: &str, out: &mut Vec<String>) {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
                for k in keys {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    diff_paths(
                        x.get(k).unwrap_or(&Value::Null),
                        y.get(k).unwrap_or(&Value::Null),
                        &path,
                        out,
                    );
                }
            }
            _ if a != b => out.push(prefix.to_string()),
            _ => {}
        }
    }

    #[test]
    fn example_arms_overlay_exactly_the_intended_fields() {
        #[derive(Deserialize)]
        struct ExampleFile {
            shadow: ShadowConfig,
        }
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/arms/swing-arms.example.yaml"
        ))
        .expect("rules/arms/swing-arms.example.yaml");
        let example: ExampleFile = serde_yaml::from_str(&raw).expect("example parses");
        assert!(example.shadow.enabled);
        let ids: Vec<&str> = example.shadow.arms.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "pullback-sma9",
                "breakout-ok",
                "corr-groups",
                "low-vol-only"
            ]
        );

        let mut prod = production();
        prod.playbook.entry.require_above_sma = vec![20, 50];
        prod.playbook.filters.min_distance_from_52w_high_pct = Some(3.0);
        prod.playbook.exit.profit_target_recent_range_cap.enabled = true;
        prod.playbook.filters.symbol_groups = vec![crate::rules::SymbolGroupConfig {
            name: "semicap".into(),
            symbols: vec!["AMD".into()],
            max_open: 1,
        }];
        prod.shadow = example.shadow.clone();
        let mut prod_value = serde_json::to_value(&prod).unwrap();
        prod_value.as_object_mut().unwrap().remove("shadow");

        let build = |id: &str| {
            let cfg = prod.shadow.arms.iter().find(|a| a.id == id).unwrap();
            build_arm_rules(&prod, Path::new("/tmp/rules/t.yaml"), cfg).unwrap()
        };
        let changed = |rules: &TraderRules| {
            let mut out = Vec::new();
            diff_paths(
                &prod_value,
                &serde_json::to_value(rules).unwrap(),
                "",
                &mut out,
            );
            out
        };

        let pullback = build("pullback-sma9");
        assert_eq!(pullback.playbook.entry.require_below_sma, vec![9]);
        assert_eq!(pullback.playbook.entry.require_above_sma, vec![20, 50]);
        assert_eq!(changed(&pullback), ["playbook.entry.require_below_sma"]);

        let breakout = build("breakout-ok");
        assert_eq!(
            breakout.playbook.filters.min_distance_from_52w_high_pct,
            None
        );
        assert!(
            !breakout
                .playbook
                .exit
                .profit_target_recent_range_cap
                .enabled
        );
        assert_eq!(
            changed(&breakout),
            [
                "playbook.exit.profit_target_recent_range_cap.enabled",
                "playbook.filters.min_distance_from_52w_high_pct",
            ]
        );

        let corr = build("corr-groups");
        assert_eq!(
            corr.playbook.filters.blocked_symbols,
            vec!["TQQQ".to_string()]
        );
        let groups: Vec<(&str, usize, u32)> = corr
            .playbook
            .filters
            .symbol_groups
            .iter()
            .map(|g| (g.name.as_str(), g.symbols.len(), g.max_open))
            .collect();
        assert_eq!(
            groups,
            [
                ("semicap", 5, 1),
                ("industrial", 3, 1),
                ("healthcare", 2, 1),
                ("utilities", 4, 1),
                ("staples", 6, 1),
                ("gold", 2, 1),
                ("crypto", 4, 1),
                ("megacap_tech", 8, 2),
            ]
        );
        assert_eq!(corr.symbol_group_name("MSTR"), Some("crypto"));
        assert_eq!(corr.symbol_group_name("META"), Some("megacap_tech"));
        assert_eq!(corr.symbol_group_name("NVDA"), Some("semicap"));
        assert_eq!(
            changed(&corr),
            [
                "playbook.filters.blocked_symbols",
                "playbook.filters.symbol_groups"
            ]
        );

        let low_vol = build("low-vol-only");
        let entries_in = |profile: &str| {
            let state = TraderState {
                active_profile: Some(profile.into()),
                ..Default::default()
            };
            crate::adaptation::effective_rules(&low_vol, &state)
                .playbook
                .entry
                .max_new_entries_per_day
        };
        assert_eq!(entries_in("baseline"), 0);
        assert_eq!(entries_in("elevated_vol"), 0);
        assert_eq!(entries_in("high_vol_chop"), 0);
        assert!(entries_in("low_vol_trend") > 0);
        let ev = &low_vol.adaptation.profiles["elevated_vol"];
        assert!(!ev.description.is_empty(), "merge keeps sibling keys");
        assert_eq!(
            ev.overrides.exit.as_ref().and_then(|e| e.stop_loss_pct),
            Some(4.5)
        );
        assert_eq!(
            changed(&low_vol),
            [
                "adaptation.profiles.baseline.overrides.entry",
                "adaptation.profiles.elevated_vol.overrides.entry.max_new_entries_per_day",
            ]
        );
    }

    #[test]
    fn day_summary_ranks_rejections() {
        let mut day = ShadowDay::new(
            NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
            4000.0,
            4100.0,
        );
        day.arm_last_equity_usd = 4050.0;
        day.prod_last_equity_usd = 4080.0;
        day.rejections.insert("below_sma".into(), 3);
        day.rejections.insert("rsi_out_of_range".into(), 9);
        let p = day.summary_payload(2, Some("baseline"));
        assert_eq!(p["arm_equity_change_usd"], json!(50.0));
        assert_eq!(p["prod_equity_change_usd"], json!(-20.0));
        assert_eq!(
            p["top_rejections"][0]["reason_code"],
            json!("rsi_out_of_range")
        );
    }

    #[test]
    fn research_arms_keep_one_variable_and_reject_unconstrained_on_production() {
        #[derive(Deserialize)]
        struct ExampleFile {
            shadow: ShadowConfig,
        }
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/arms/research-arms.yaml"
        ))
        .unwrap();
        let example: ExampleFile = crate::rules::parse_yaml_merged(&raw).unwrap();
        let mut prod = production();
        prod.shadow = example.shadow;
        let build = |id: &str| {
            let cfg = prod.shadow.arms.iter().find(|a| a.id == id).unwrap();
            build_arm_rules(&prod, Path::new("/tmp/rules/t.yaml"), cfg).unwrap()
        };
        let tier_a = build("unconstrained");
        assert!(tier_a.capital.unconstrained);
        assert!((tier_a.capital.fixed_sleeve_cap_usd - 1_000_000.0).abs() < 1.0);
        assert_eq!(tier_a.playbook.entry.max_positions, 40);
        assert_eq!(tier_a.playbook.entry.max_new_entries_per_day, 15);
        assert_eq!(
            tier_a.playbook.entry.position_size.max_pct_of_adv,
            Some(1.0)
        );
        assert!((tier_a.playbook.entry.position_size.risk_per_trade_pct - 0.25).abs() < 1e-9);

        let twin = build("big-base");
        assert!(!twin.capital.unconstrained);
        assert!((twin.capital.fixed_sleeve_cap_usd - 30_000.0).abs() < 1.0);
        assert_eq!(twin.playbook.entry.max_positions, 10);
        assert!((twin.playbook.entry.position_size.risk_per_trade_pct - 0.75).abs() < 1e-9);
        assert!((twin.playbook.entry.position_size.max_position_pct - 12.0).abs() < 1e-9);

        let pullback = build("pullback");
        assert_eq!(pullback.playbook.entry.require_below_sma, vec![9]);
        assert_eq!(pullback.playbook.entry.max_positions, 40);
        let dip = build("dip-reversion");
        assert_eq!(dip.playbook.entry.require_above_sma, vec![200]);
        assert_eq!(dip.playbook.entry.rsi_14_range, [30.0, 48.0]);
        assert!(
            build("rangecap-fix")
                .playbook
                .exit
                .profit_target_recent_range_cap
                .skip_nonpositive_ceiling
        );
        assert!(!build("stop-at-trigger").playbook.exit.fill_stop_through_gap);
        assert!(build("no-blocked")
            .playbook
            .filters
            .blocked_symbols
            .is_empty());
        assert_eq!(
            build("wide-universe")
                .watchlists
                .candidate_pool_file
                .as_deref(),
            Some("universe/liquid-broad.yaml")
        );
        assert_eq!(build("llm-veto").llm_signal.policy.mode, "veto");
        assert_eq!(build("llm-agent").llm_signal.policy.mode, "agent");
        assert_eq!(build("llm-event-exit").llm_signal.policy.mode, "event_exit");
        let regime = build("regime-playbooks");
        assert_eq!(
            regime.adaptation.profiles["low_vol_trend"]
                .overrides
                .entry
                .as_ref()
                .and_then(|e| e.require_below_sma.clone()),
            Some(vec![9])
        );

        let mut live = production();
        live.trader_id = "swing".into();
        live.capital.unconstrained = true;
        assert!(live.validate().is_err());
    }
}
