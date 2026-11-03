//! Evidence gate for research-arm self-patches.
//!
//! `--apply-patches` may activate a proposal only when it pre-registered a
//! hypothesis and [`may_activate`] accepts the current scorecard and ledger.
//! Activations are appended to `rules/research/<arm>.patch-ledger.json`. The
//! newest open entry is judged once its `evaluate_after_days` window has passed
//! and is never rewritten after that.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::AdaptationConfig;
use crate::patch::AppliedPatch;
use crate::playbook;
use crate::scoring::Scorecard;

const NO_HYPOTHESIS: &str = "no hypothesis pre-registered";

/// Why a judged activation moved the scorecard the way it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Improved,
    Degraded,
    Inconclusive,
}

/// Deltas of the scorecard after the observation window, minus the card at activation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDeltas {
    pub mean_rank_ic: Option<f64>,
    pub hit_rate_pct: f64,
    pub mean_excess_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    /// Activation time. Dwell and the observation window are measured from this,
    /// not from process start.
    pub at: DateTime<Utc>,
    pub path: String,
    pub old_value: Value,
    pub new_value: Value,
    pub hypothesis: String,
    pub expected_effect: String,
    pub evaluate_after_days: u32,
    pub kill_criteria: String,
    pub scorecard_at_activation: Scorecard,
    pub evaluated_at: Option<DateTime<Utc>>,
    pub scorecard_after: Option<Scorecard>,
    pub verdict: Option<Verdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deltas: Option<MetricDeltas>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ledger {
    #[serde(default)]
    pub entries: Vec<LedgerEntry>,
}

/// `rules/research/<arm>.patch-ledger.json`, beside the arm YAML and its
/// `.versions` directory.
pub fn ledger_path(rules_path: &Path) -> PathBuf {
    let stem = rules_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arm".into());
    rules_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}.patch-ledger.json"))
}

pub fn load_ledger(path: &Path) -> Result<Ledger> {
    if !path.is_file() {
        return Ok(Ledger::default());
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Ledger::default());
    }
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

/// Atomic replace: write a temp file in the same directory, then rename.
pub fn save_ledger(path: &Path, ledger: &Ledger) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
    }
    let pretty = serde_json::to_string_pretty(ledger)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "patch-ledger.json".into());
    let tmp = path.with_file_name(format!(".{file_name}.tmp"));
    std::fs::write(&tmp, pretty).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

impl Ledger {
    pub fn record_activations(
        &mut self,
        applied: &[AppliedPatch],
        card: &Scorecard,
        now: DateTime<Utc>,
    ) {
        for change in applied {
            let id = format!(
                "{}:{}:{}",
                now.timestamp_millis(),
                self.entries.len(),
                change.path
            );
            self.entries.push(LedgerEntry {
                id,
                at: now,
                path: change.path.clone(),
                old_value: change.old_value.clone(),
                new_value: change.new_value.clone(),
                hypothesis: change.hypothesis.clone(),
                expected_effect: change.expected_effect.clone(),
                evaluate_after_days: change.evaluate_after_days,
                kill_criteria: change.kill_criteria.clone(),
                scorecard_at_activation: card.clone(),
                evaluated_at: None,
                scorecard_after: None,
                verdict: None,
                deltas: None,
            });
        }
    }
}

/// Deny when the scorecard is too thin, or a recent activation still owns the
/// dwell window. `Ok(())` means the gate allows activation.
pub fn may_activate(
    card: &Scorecard,
    ledger: &Ledger,
    cfg: &AdaptationConfig,
) -> Result<(), String> {
    if card.ic_days < cfg.min_ic_days as usize {
        return Err(format!(
            "ic_days {} is below min_ic_days {}",
            card.ic_days, cfg.min_ic_days
        ));
    }
    match card.mean_rank_ic {
        Some(ic) if ic.is_finite() && ic > cfg.min_rank_ic => {}
        Some(ic) => {
            return Err(format!(
                "mean_rank_ic {ic} is not above min_rank_ic {}",
                cfg.min_rank_ic
            ));
        }
        None => return Err("mean_rank_ic is missing".into()),
    }
    if let Some(t) = card.ic_t_stat {
        if t < cfg.min_ic_t {
            return Err(format!("ic_t_stat {t} is below min_ic_t {}", cfg.min_ic_t));
        }
    }
    let now = Utc::now();
    let dwell = Duration::days(i64::from(cfg.min_dwell_days));
    if let Some(live) =
        ledger.entries.iter().rev().find(|entry| {
            entry.evaluated_at.is_none() && now.signed_duration_since(entry.at) < dwell
        })
    {
        return Err(format!(
            "live patch {} is younger than min_dwell_days {} (activated_at {})",
            live.id,
            cfg.min_dwell_days,
            live.at.to_rfc3339()
        ));
    }
    if let Some(last) = ledger.entries.last() {
        if now.signed_duration_since(last.at) < dwell {
            return Err(format!(
                "min_dwell_days {} has not elapsed since the last activation at {}",
                cfg.min_dwell_days,
                last.at.to_rfc3339()
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationDecision {
    pub activate: bool,
    pub gate_reason: Option<String>,
}

/// `--apply-patches` is necessary but not sufficient. A missing hypothesis is
/// refused before the scorecard is consulted.
pub fn decide_activation(
    apply_patches: bool,
    applied: &[AppliedPatch],
    card: &Scorecard,
    ledger: &Ledger,
    cfg: &AdaptationConfig,
) -> ActivationDecision {
    if !apply_patches {
        return ActivationDecision {
            activate: false,
            gate_reason: None,
        };
    }
    if applied
        .iter()
        .any(|patch| patch.hypothesis.trim().is_empty())
    {
        return ActivationDecision {
            activate: false,
            gate_reason: Some(NO_HYPOTHESIS.into()),
        };
    }
    match may_activate(card, ledger, cfg) {
        Ok(()) => ActivationDecision {
            activate: true,
            gate_reason: None,
        },
        Err(reason) => ActivationDecision {
            activate: false,
            gate_reason: Some(reason),
        },
    }
}

pub struct ProposalOutcome {
    pub version_path: PathBuf,
    pub activated: bool,
    pub gate_reason: Option<String>,
}

/// Version the proposal. Activate it, and append ledger rows, only when the
/// gate allows. A denial still writes the snapshot with `activated: false` and
/// the reason on the snapshot meta.
pub fn consider_proposal(
    rules: &Path,
    root: &Value,
    applied: &[AppliedPatch],
    meta: Value,
    apply_patches: bool,
    card: &Scorecard,
    cfg: &AdaptationConfig,
    now: DateTime<Utc>,
) -> Result<ProposalOutcome> {
    let path = ledger_path(rules);
    let mut ledger = load_ledger(&path)?;
    let decision = decide_activation(apply_patches, applied, card, &ledger, cfg);
    if decision.activate {
        let previous = ledger.clone();
        ledger.record_activations(applied, card, now);
        save_ledger(&path, &ledger)?;
        match playbook::write_version(rules, root, applied, meta, true) {
            Ok(version_path) => {
                return Ok(ProposalOutcome {
                    version_path,
                    activated: true,
                    gate_reason: None,
                });
            }
            Err(err) => {
                let _ = save_ledger(&path, &previous);
                return Err(err);
            }
        }
    }
    let meta = stamp_gate_reason(meta, decision.gate_reason.as_deref());
    let version_path = playbook::write_version(rules, root, applied, meta, false)?;
    Ok(ProposalOutcome {
        version_path,
        activated: false,
        gate_reason: decision.gate_reason,
    })
}

fn stamp_gate_reason(mut meta: Value, reason: Option<&str>) -> Value {
    let Some(reason) = reason else {
        return meta;
    };
    if let Some(object) = meta.as_object_mut() {
        object.insert("gate_reason".into(), Value::String(reason.to_string()));
        return meta;
    }
    serde_json::json!({ "gate_reason": reason, "detail": meta })
}

/// Judge the newest open ledger entry once, when `at + evaluate_after_days`
/// has passed. An entry that already has `evaluated_at` is left untouched, and
/// the file is not rewritten in that case.
pub fn evaluate_due(ledger: &mut Ledger, now: DateTime<Utc>, after: &Scorecard) -> bool {
    let Some(entry) = ledger.entries.last_mut() else {
        return false;
    };
    if entry.evaluated_at.is_some() {
        return false;
    }
    let window = Duration::days(i64::from(entry.evaluate_after_days));
    if now.signed_duration_since(entry.at) < window {
        return false;
    }
    let (verdict, deltas) = classify(&entry.scorecard_at_activation, after);
    entry.scorecard_after = Some(after.clone());
    entry.evaluated_at = Some(now);
    entry.verdict = Some(verdict);
    entry.deltas = Some(deltas);
    true
}

pub fn evaluate_ledger(rules: &Path, now: DateTime<Utc>, card: &Scorecard) -> Result<bool> {
    let path = ledger_path(rules);
    let mut ledger = load_ledger(&path)?;
    if !evaluate_due(&mut ledger, now, card) {
        return Ok(false);
    }
    save_ledger(&path, &ledger)?;
    Ok(true)
}

fn classify(before: &Scorecard, after: &Scorecard) -> (Verdict, MetricDeltas) {
    let mean_rank_ic = match (before.mean_rank_ic, after.mean_rank_ic) {
        (Some(start), Some(end)) if start.is_finite() && end.is_finite() => Some(end - start),
        _ => None,
    };
    let hit_rate_pct = after.hit_rate_pct - before.hit_rate_pct;
    let mean_excess_pct = after.mean_excess_pct - before.mean_excess_pct;
    let verdict = match mean_rank_ic {
        Some(delta_ic) if delta_ic >= 0.02 && hit_rate_pct >= -2.0 => Verdict::Improved,
        Some(delta_ic) if delta_ic <= -0.02 => Verdict::Degraded,
        _ => Verdict::Inconclusive,
    };
    (
        verdict,
        MetricDeltas {
            mean_rank_ic,
            hit_rate_pct,
            mean_excess_pct,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::Mutable;
    use serde_json::json;

    fn passing_card() -> Scorecard {
        Scorecard {
            ic_days: 10,
            scored_days: 12,
            signals_scored: 40,
            hit_rate_pct: 55.0,
            mean_excess_pct: 0.4,
            mean_rank_ic: Some(0.05),
            ic_t_stat: Some(2.5),
            gate_pass: true,
            gate_reason: "stage B signal criteria met on paper".into(),
            last_scored_date: Some("2026-10-29".into()),
        }
    }

    fn card_with(ic: Option<f64>, hit: f64, excess: f64) -> Scorecard {
        Scorecard {
            mean_rank_ic: ic,
            hit_rate_pct: hit,
            mean_excess_pct: excess,
            ..passing_card()
        }
    }

    fn applied(hypothesis: &str) -> AppliedPatch {
        AppliedPatch {
            path: "forecast.min_confidence".into(),
            old_value: json!(0.55),
            new_value: json!(0.5),
            reason: "loosen selectivity".into(),
            hypothesis: hypothesis.into(),
            expected_effect: "mean_rank_ic up".into(),
            evaluate_after_days: 5,
            kill_criteria: "revert if mean_rank_ic falls by 0.02".into(),
        }
    }

    fn entry_at(at: DateTime<Utc>, evaluated: bool) -> LedgerEntry {
        LedgerEntry {
            id: "live-1".into(),
            at,
            path: "forecast.min_confidence".into(),
            old_value: json!(0.55),
            new_value: json!(0.5),
            hypothesis: "tighter confidence should lift rank IC".into(),
            expected_effect: "mean_rank_ic up".into(),
            evaluate_after_days: 5,
            kill_criteria: "revert if mean_rank_ic falls by 0.02".into(),
            scorecard_at_activation: passing_card(),
            evaluated_at: evaluated.then_some(at),
            scorecard_after: evaluated.then_some(passing_card()),
            verdict: evaluated.then_some(Verdict::Inconclusive),
            deltas: None,
        }
    }

    #[test]
    fn may_activate_denies_short_ic_days() {
        let mut card = passing_card();
        card.ic_days = 9;
        let err =
            may_activate(&card, &Ledger::default(), &AdaptationConfig::default()).unwrap_err();
        assert!(err.contains("ic_days"), "{err}");
        assert!(err.contains("min_ic_days"), "{err}");
    }

    #[test]
    fn may_activate_denies_weak_rank_ic_or_t_stat() {
        let cfg = AdaptationConfig::default();
        let missing = card_with(None, 55.0, 0.4);
        let err = may_activate(&missing, &Ledger::default(), &cfg).unwrap_err();
        assert!(err.contains("mean_rank_ic"), "{err}");

        let weak = card_with(Some(0.02), 55.0, 0.4);
        let err = may_activate(&weak, &Ledger::default(), &cfg).unwrap_err();
        assert!(err.contains("mean_rank_ic"), "{err}");

        let mut low_t = passing_card();
        low_t.ic_t_stat = Some(1.5);
        let err = may_activate(&low_t, &Ledger::default(), &cfg).unwrap_err();
        assert!(err.contains("ic_t_stat"), "{err}");

        let mut no_t = passing_card();
        no_t.ic_t_stat = None;
        assert!(may_activate(&no_t, &Ledger::default(), &cfg).is_ok());
    }

    #[test]
    fn may_activate_denies_inside_dwell() {
        let cfg = AdaptationConfig::default();
        let card = passing_card();
        let recent = Utc::now() - Duration::days(1);
        let mut live = Ledger::default();
        live.entries.push(entry_at(recent, false));
        let err = may_activate(&card, &live, &cfg).unwrap_err();
        assert!(err.contains("live patch"), "{err}");
        assert!(err.contains("min_dwell_days"), "{err}");

        let mut judged = Ledger::default();
        judged.entries.push(entry_at(recent, true));
        let err = may_activate(&card, &judged, &cfg).unwrap_err();
        assert!(
            err.contains("has not elapsed since the last activation"),
            "{err}"
        );
    }

    #[test]
    fn may_activate_allows_when_satisfied() {
        let cfg = AdaptationConfig::default();
        let card = passing_card();
        assert!(may_activate(&card, &Ledger::default(), &cfg).is_ok());

        let mut ledger = Ledger::default();
        let old = Utc::now() - Duration::days(6);
        ledger.entries.push(entry_at(old, true));
        assert!(may_activate(&card, &ledger, &cfg).is_ok());
    }

    #[test]
    fn proposal_without_hypothesis_is_versioned_but_not_activated() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("megacaps.yaml");
        let outcome = consider_proposal(
            &rules,
            &Mutable::default().to_root(),
            &[applied("")],
            json!({"source": "forecast"}),
            true,
            &passing_card(),
            &AdaptationConfig::default(),
            Utc::now(),
        )
        .unwrap();
        assert!(!outcome.activated);
        assert_eq!(
            outcome.gate_reason.as_deref(),
            Some("no hypothesis pre-registered")
        );
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&outcome.version_path).unwrap()).unwrap();
        assert_eq!(doc["activated"], false);
        assert_eq!(doc["meta"]["gate_reason"], "no hypothesis pre-registered");
        assert!(!playbook::versions_dir(&rules).join("active.json").is_file());
        assert!(!ledger_path(&rules).is_file());
    }

    #[test]
    fn denied_gate_versions_without_activating() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("megacaps.yaml");
        let mut card = passing_card();
        card.ic_days = 3;
        let outcome = consider_proposal(
            &rules,
            &Mutable::default().to_root(),
            &[applied("raising the target should lift excess return")],
            json!({"source": "forecast"}),
            true,
            &card,
            &AdaptationConfig::default(),
            Utc::now(),
        )
        .unwrap();
        assert!(!outcome.activated);
        let reason = outcome.gate_reason.unwrap();
        assert!(reason.contains("ic_days"), "{reason}");
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&outcome.version_path).unwrap()).unwrap();
        assert_eq!(doc["activated"], false);
        assert_eq!(doc["meta"]["gate_reason"], reason);
        assert!(!ledger_path(&rules).is_file());
    }

    #[test]
    fn satisfied_proposal_activates_and_appends_a_ledger_entry() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("megacaps.yaml");
        let outcome = consider_proposal(
            &rules,
            &Mutable::default().to_root(),
            &[applied("tighter confidence should lift rank IC")],
            json!({"source": "forecast"}),
            true,
            &passing_card(),
            &AdaptationConfig::default(),
            Utc::now(),
        )
        .unwrap();
        assert!(outcome.activated);
        assert!(outcome.gate_reason.is_none());
        assert!(playbook::versions_dir(&rules).join("active.json").is_file());
        let ledger = load_ledger(&ledger_path(&rules)).unwrap();
        assert_eq!(ledger.entries.len(), 1);
        let entry = &ledger.entries[0];
        assert_eq!(entry.hypothesis, "tighter confidence should lift rank IC");
        assert_eq!(entry.expected_effect, "mean_rank_ic up");
        assert_eq!(entry.evaluate_after_days, 5);
        assert_eq!(entry.kill_criteria, "revert if mean_rank_ic falls by 0.02");
        assert!(entry.evaluated_at.is_none());
        assert!(entry.scorecard_after.is_none());
        assert!(entry.verdict.is_none());
        assert_eq!(entry.path, "forecast.min_confidence");
    }

    #[test]
    fn ledger_entry_is_judged_once_after_the_window() {
        let now = Utc::now();
        let before = card_with(Some(0.05), 50.0, 0.1);
        let improved = card_with(Some(0.08), 49.0, 0.4);
        let mut ledger = Ledger::default();
        ledger.entries.push(LedgerEntry {
            scorecard_at_activation: before,
            at: now - Duration::days(6),
            evaluate_after_days: 5,
            evaluated_at: None,
            scorecard_after: None,
            verdict: None,
            ..entry_at(now, false)
        });

        let mut fresh = Ledger::default();
        fresh.entries.push(entry_at(now, false));
        assert!(!evaluate_due(&mut fresh, now, &improved));
        assert!(fresh.entries[0].evaluated_at.is_none());

        assert!(evaluate_due(&mut ledger, now, &improved));
        let entry = &ledger.entries[0];
        assert_eq!(entry.verdict, Some(Verdict::Improved));
        assert_eq!(
            entry.scorecard_after.as_ref().unwrap().mean_rank_ic,
            Some(0.08)
        );
        let deltas = entry.deltas.as_ref().unwrap();
        assert!(deltas.mean_rank_ic.unwrap() >= 0.02);
        assert!((deltas.hit_rate_pct - (-1.0)).abs() < 1e-9);
        assert!((deltas.mean_excess_pct - 0.3).abs() < 1e-9);
        let evaluated_at = entry.evaluated_at;

        let worse = card_with(Some(0.0), 10.0, -1.0);
        assert!(!evaluate_due(&mut ledger, now, &worse));
        assert_eq!(ledger.entries[0].verdict, Some(Verdict::Improved));
        assert_eq!(
            ledger.entries[0]
                .scorecard_after
                .as_ref()
                .unwrap()
                .mean_rank_ic,
            Some(0.08)
        );
        assert_eq!(ledger.entries[0].evaluated_at, evaluated_at);

        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("megacaps.yaml");
        save_ledger(&ledger_path(&rules), &ledger).unwrap();
        let bytes = std::fs::read(&ledger_path(&rules)).unwrap();
        assert!(!evaluate_ledger(&rules, now, &worse).unwrap());
        assert_eq!(std::fs::read(&ledger_path(&rules)).unwrap(), bytes);
    }

    #[test]
    fn verdict_follows_rank_ic_and_hit_rate() {
        let now = Utc::now();
        let judge = |before: Scorecard, after: Scorecard| -> Verdict {
            let mut ledger = Ledger::default();
            ledger.entries.push(LedgerEntry {
                at: now - Duration::days(6),
                evaluate_after_days: 5,
                scorecard_at_activation: before,
                evaluated_at: None,
                scorecard_after: None,
                verdict: None,
                ..entry_at(now, false)
            });
            assert!(evaluate_due(&mut ledger, now, &after));
            ledger.entries[0].verdict.unwrap()
        };

        let base = card_with(Some(0.05), 50.0, 0.2);
        assert_eq!(
            judge(base.clone(), card_with(Some(0.05 + 0.02), 48.0, 0.2)),
            Verdict::Improved
        );
        assert_eq!(
            judge(base.clone(), card_with(Some(0.05 + 0.02), 47.0, 0.9)),
            Verdict::Inconclusive
        );
        assert_eq!(
            judge(base.clone(), card_with(Some(0.05 - 0.02), 80.0, 1.0)),
            Verdict::Degraded
        );
        assert_eq!(
            judge(base, card_with(Some(0.06), 50.0, -0.4)),
            Verdict::Inconclusive
        );
    }
}
