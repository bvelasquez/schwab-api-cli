//! LLM entry-decision scorecard: journal decisions, honor narrow vetoes, resolve vs P/L.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::journal;
use super::llm::LlmReview;
use super::state::{AgentState, TrackedPosition};

/// Paths the learn phase may suggest (human applies; engine never auto-mutates).
pub const SUGGESTION_ALLOWLIST: &[&str] = &[
    "exit_rules.thesis.min_short_otm_pct",
    "exit_rules.thesis.min_hold_minutes",
    "exit_rules.thesis.min_pop_pct_exit",
    "exit_rules.thesis.max_short_delta_exit",
    "entry_rules.vertical.short_delta_min",
    "entry_rules.vertical.short_delta_max",
    "entry_rules.vertical.min_iv_rv_ratio",
    "entry_rules.vertical.min_short_otm_pct",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LlmScorecardSummary {
    pub decisions_total: u64,
    pub proceed_raw: u64,
    pub defer_raw: u64,
    pub skip_raw: u64,
    pub honored_vetoes: u64,
    pub ignored_defers: u64,
    pub linked_trades: u64,
    pub linked_wins: u64,
    pub linked_losses: u64,
    pub linked_pnl_usd: f64,
    pub last_updated: Option<DateTime<Utc>>,
}

impl LlmScorecardSummary {
    pub fn glance_line(&self) -> String {
        let wl = if self.linked_trades == 0 {
            "—".into()
        } else {
            format!("{}/{}", self.linked_wins, self.linked_losses)
        };
        format!(
            "vetoes {} · ignored defer {} · linked W/L {}",
            self.honored_vetoes, self.ignored_defers, wl
        )
    }

    pub fn to_json(&self) -> Value {
        let linked_win_rate = if self.linked_trades == 0 {
            Value::Null
        } else {
            json!(self.linked_wins as f64 / self.linked_trades as f64)
        };
        json!({
            "decisions_total": self.decisions_total,
            "proceed_raw": self.proceed_raw,
            "defer_raw": self.defer_raw,
            "skip_raw": self.skip_raw,
            "honored_vetoes": self.honored_vetoes,
            "ignored_defers": self.ignored_defers,
            "ignored_defer_rate": if self.decisions_total == 0 {
                0.0
            } else {
                self.ignored_defers as f64 / self.decisions_total as f64
            },
            "linked_trades": self.linked_trades,
            "linked_wins": self.linked_wins,
            "linked_losses": self.linked_losses,
            "linked_pnl_usd": self.linked_pnl_usd,
            "linked_win_rate": linked_win_rate,
            "last_updated": self.last_updated.map(|t| t.to_rfc3339()),
            "glance": self.glance_line(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmEntryDecisionRecord {
    pub decision_id: String,
    pub at: DateTime<Utc>,
    pub raw_recommendation: String,
    pub veto_category: String,
    pub evidence: String,
    pub reasoning: String,
    pub honored: bool,
    pub effective_action: String,
    pub candidate_fingerprints: Vec<String>,
    pub model: String,
    pub used_web: bool,
}

/// True only for unexpected_catalyst + non-empty evidence on defer/skip.
pub fn should_honor_entry_veto(review: &LlmReview) -> bool {
    let raw = review.entry_recommendation.to_ascii_lowercase();
    if !matches!(raw.as_str(), "skip" | "defer" | "hold") {
        return false;
    }
    review.veto_category.eq_ignore_ascii_case("unexpected_catalyst")
        && !review.evidence.trim().is_empty()
}

pub fn effective_entry_action(review: &LlmReview) -> (&'static str, bool) {
    let raw = review.entry_recommendation.to_ascii_lowercase();
    if matches!(raw.as_str(), "proceed") {
        return ("proceed", false);
    }
    if should_honor_entry_veto(review) {
        return ("veto", true);
    }
    // Fail-open: treat vague/calendar/math defer as proceed for execution.
    ("proceed_fail_open", false)
}

pub fn record_selection_decision(
    rules_path: &Path,
    simulate: bool,
    state: &mut AgentState,
    review: &LlmReview,
    fingerprints: Vec<String>,
) -> LlmEntryDecisionRecord {
    let (effective, honored) = effective_entry_action(review);
    let ignored_defer = matches!(
        review.entry_recommendation.to_ascii_lowercase().as_str(),
        "defer" | "skip" | "hold"
    ) && !honored
        && effective.starts_with("proceed");

    let now = Utc::now();
    let record = LlmEntryDecisionRecord {
        decision_id: format!("llm-{}", now.timestamp_nanos_opt().unwrap_or(now.timestamp())),
        at: now,
        raw_recommendation: review.entry_recommendation.clone(),
        veto_category: review.veto_category.clone(),
        evidence: review.evidence.clone(),
        reasoning: review.entry_reasoning.clone(),
        honored,
        effective_action: effective.to_string(),
        candidate_fingerprints: fingerprints,
        model: review.model.clone(),
        used_web: review.used_web,
    };

    let sc = &mut state.llm_scorecard;
    sc.decisions_total += 1;
    match record.raw_recommendation.to_ascii_lowercase().as_str() {
        "proceed" => sc.proceed_raw += 1,
        "defer" => sc.defer_raw += 1,
        "skip" | "hold" => sc.skip_raw += 1,
        _ => {}
    }
    if honored {
        sc.honored_vetoes += 1;
    }
    if ignored_defer {
        sc.ignored_defers += 1;
    }
    sc.last_updated = Some(Utc::now());

    let payload = json!({
        "decision_id": record.decision_id,
        "raw_recommendation": record.raw_recommendation,
        "veto_category": record.veto_category,
        "evidence": record.evidence,
        "reasoning": record.reasoning,
        "honored": record.honored,
        "effective_action": record.effective_action,
        "ignored_defer": ignored_defer,
        "candidate_fingerprints": record.candidate_fingerprints,
        "model": record.model,
        "used_web": record.used_web,
    });
    let _ = journal::append_event(rules_path, simulate, "llm_entry_decision", payload);

    state.last_llm_entry_decision = Some(record.clone());
    record
}

pub fn attach_decision_to_position(tracked: &mut TrackedPosition, state: &AgentState) {
    if tracked.llm_decision_id.is_some() {
        return;
    }
    let Some(dec) = state.last_llm_entry_decision.as_ref() else {
        return;
    };
    if !dec.effective_action.starts_with("proceed") {
        return;
    }
    if dec
        .candidate_fingerprints
        .iter()
        .any(|fp| fp == &tracked.position_id)
    {
        tracked.llm_decision_id = Some(dec.decision_id.clone());
    }
}

pub fn resolve_on_exit(
    rules_path: &Path,
    simulate: bool,
    state: &mut AgentState,
    tracked: &TrackedPosition,
    exit_reason: &str,
    pnl_usd: f64,
    pnl_pct_of_credit: f64,
) {
    let hold_days = Utc::now()
        .signed_duration_since(tracked.opened_at)
        .num_days()
        .max(0);
    let decision_id = tracked.llm_decision_id.clone();
    let linked = decision_id.is_some();

    if linked {
        let sc = &mut state.llm_scorecard;
        sc.linked_trades += 1;
        sc.linked_pnl_usd += pnl_usd;
        if pnl_usd >= 0.0 {
            sc.linked_wins += 1;
        } else {
            sc.linked_losses += 1;
        }
        sc.last_updated = Some(Utc::now());
    }

    let payload = json!({
        "position_id": tracked.position_id,
        "underlying": tracked.underlying,
        "expiry": tracked.expiry,
        "exit_reason": exit_reason,
        "pnl_usd": pnl_usd,
        "pnl_pct_of_credit": pnl_pct_of_credit,
        "hold_days": hold_days,
        "llm_decision_id": decision_id,
        "linked": linked,
        "entry_credit": tracked.entry_credit,
        "contracts": tracked.contracts,
    });
    let _ = journal::append_event(rules_path, simulate, "llm_scorecard_resolve", payload);
}

/// Aggregate scorecard from journal (authoritative) + merge into a fresh summary.
pub fn aggregate_from_journal(rules_path: &Path, simulate: bool) -> anyhow::Result<LlmScorecardSummary> {
    let events = journal::read_all(rules_path, simulate)?;
    let mut sc = LlmScorecardSummary::default();
    for ev in events {
        let ty = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = ev.get("payload").cloned().unwrap_or(Value::Null);
        let ts = ev
            .get("ts")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        match ty {
            "llm_entry_decision" => {
                sc.decisions_total += 1;
                match payload
                    .get("raw_recommendation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "proceed" => sc.proceed_raw += 1,
                    "defer" => sc.defer_raw += 1,
                    "skip" | "hold" => sc.skip_raw += 1,
                    _ => {}
                }
                if payload
                    .get("honored")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    sc.honored_vetoes += 1;
                }
                if payload
                    .get("ignored_defer")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    sc.ignored_defers += 1;
                }
                if let Some(t) = ts {
                    sc.last_updated = Some(t);
                }
            }
            "llm_scorecard_resolve" => {
                if payload
                    .get("linked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    sc.linked_trades += 1;
                    let pnl = payload.get("pnl_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    sc.linked_pnl_usd += pnl;
                    if pnl >= 0.0 {
                        sc.linked_wins += 1;
                    } else {
                        sc.linked_losses += 1;
                    }
                }
                if let Some(t) = ts {
                    sc.last_updated = Some(t);
                }
            }
            _ => {}
        }
    }
    Ok(sc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{LlmReview, PositionReview};
    use serde_json::json;

    fn review(rec: &str, category: &str, evidence: &str) -> LlmReview {
        LlmReview {
            phase: "selection".into(),
            model: "test".into(),
            used_web: false,
            raw: json!({}),
            market_commentary: String::new(),
            web_insights: vec![],
            position_reviews: Vec::<PositionReview>::new(),
            entry_recommendation: rec.into(),
            entry_reasoning: "because".into(),
            veto_category: category.into(),
            evidence: evidence.into(),
            risk_alerts: vec![],
        }
    }

    #[test]
    fn honors_only_catalyst_with_evidence() {
        assert!(should_honor_entry_veto(&review(
            "defer",
            "unexpected_catalyst",
            "FDA decision tomorrow on underlying"
        )));
        assert!(!should_honor_entry_veto(&review(
            "defer",
            "unexpected_catalyst",
            ""
        )));
        assert!(!should_honor_entry_veto(&review("defer", "other", "FOMC was yesterday")));
        assert!(!should_honor_entry_veto(&review("defer", "none", "")));
        assert!(!should_honor_entry_veto(&review("proceed", "none", "")));
    }

    #[test]
    fn fail_open_on_calendar_defer() {
        let r = review("defer", "other", "FOMC is scheduled for 2026-07-29");
        let (action, honored) = effective_entry_action(&r);
        assert_eq!(action, "proceed_fail_open");
        assert!(!honored);
    }
}
