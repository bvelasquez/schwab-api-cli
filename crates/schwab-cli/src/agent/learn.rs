//! Post-trade LLM learn: write human-applied suggestions (no YAML mutate).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use super::journal;
use super::llm::OpenRouterClient;
use super::paths::suggestions_path;
use super::scorecard::{LlmScorecardSummary, SUGGESTION_ALLOWLIST};
use super::state::TrackedPosition;
use crate::rules::LlmConfig;

/// Append a mechanical postmortem + optional LLM patches after a closed trade.
pub async fn write_postmortem_suggestions(
    rules_path: &Path,
    llm: &LlmConfig,
    client: Option<&OpenRouterClient>,
    scorecard: &LlmScorecardSummary,
    tracked: &TrackedPosition,
    exit_reason: &str,
    pnl_usd: f64,
    pnl_pct: f64,
) -> Result<Option<std::path::PathBuf>> {
    if !llm.enabled || !llm.allow_rule_suggestions {
        return Ok(None);
    }

    let path = suggestions_path(rules_path);
    let recent = journal::read_recent(rules_path, false, 40).unwrap_or_default();
    let mut patches: Vec<Value> = Vec::new();
    let mut lessons = vec![format!(
        "Closed {} {} exit={} pnl=${:.2} ({:.1}% of credit) hold since {}",
        tracked.underlying,
        tracked.position_id,
        exit_reason,
        pnl_usd,
        pnl_pct,
        tracked.opened_at.to_rfc3339()
    )];

    if let Some(client) = client {
        match client
            .suggest_rule_patches(llm, &build_learn_context(scorecard, tracked, exit_reason, pnl_usd, pnl_pct, &recent))
            .await
        {
            Ok((llm_lessons, llm_patches)) => {
                lessons.extend(llm_lessons);
                patches = filter_allowlisted_patches(llm_patches);
            }
            Err(e) => {
                lessons.push(format!("LLM learn call failed (mechanical postmortem kept): {e:#}"));
            }
        }
    }

    let mut body = String::new();
    if path.exists() {
        body = fs::read_to_string(&path).unwrap_or_default();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("\n---\n\n");
    } else {
        body.push_str("# Options agent LLM suggestions (human apply only)\n\n");
        body.push_str("Engine does **not** auto-apply these. Review and edit rules YAML yourself.\n\n");
        body.push_str(&format!(
            "Allowlisted paths:\n{}\n\n",
            SUGGESTION_ALLOWLIST
                .iter()
                .map(|p| format!("- `{p}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    body.push_str(&format!("## {}\n\n", Utc::now().to_rfc3339()));
    body.push_str("### Lessons\n\n");
    for l in &lessons {
        body.push_str(&format!("- {l}\n"));
    }
    body.push_str("\n### Suggested patches\n\n");
    if patches.is_empty() {
        body.push_str("_None (or not allowlisted)._\n");
    } else {
        body.push_str("```json\n");
        body.push_str(&serde_json::to_string_pretty(&patches)?);
        body.push_str("\n```\n");
    }
    body.push_str(&format!(
        "\n### Scorecard glance\n\n`{}`\n",
        scorecard.glance_line()
    ));

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(Some(path))
}

fn build_learn_context(
    scorecard: &LlmScorecardSummary,
    tracked: &TrackedPosition,
    exit_reason: &str,
    pnl_usd: f64,
    pnl_pct: f64,
    recent: &[Value],
) -> Value {
    json!({
        "phase": "learn",
        "closed_trade": {
            "position_id": tracked.position_id,
            "underlying": tracked.underlying,
            "expiry": tracked.expiry,
            "strategy": tracked.strategy,
            "entry_credit": tracked.entry_credit,
            "llm_decision_id": tracked.llm_decision_id,
            "exit_reason": exit_reason,
            "pnl_usd": pnl_usd,
            "pnl_pct_of_credit": pnl_pct,
            "opened_at": tracked.opened_at.to_rfc3339(),
        },
        "scorecard": scorecard.to_json(),
        "allowlisted_paths": SUGGESTION_ALLOWLIST,
        "recent_journal": recent,
        "instructions": "Propose at most 3 patches using only allowlisted paths. Prefer no change if sample is thin. Return JSON: {\"lessons\":[\"...\"],\"patches\":[{\"path\":\"...\",\"value\":...,\"reason\":\"...\"}]}"
    })
}

pub fn filter_allowlisted_patches(patches: Vec<Value>) -> Vec<Value> {
    patches
        .into_iter()
        .filter(|p| {
            p.get("path")
                .and_then(|v| v.as_str())
                .is_some_and(|path| SUGGESTION_ALLOWLIST.iter().any(|a| *a == path))
                && p.get("value").is_some()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_non_allowlisted_paths() {
        let patches = vec![
            json!({"path": "exit_rules.thesis.min_short_otm_pct", "value": 3.5, "reason": "ok"}),
            json!({"path": "risk.max_portfolio_risk_usd", "value": 999, "reason": "no"}),
        ];
        let kept = filter_allowlisted_patches(patches);
        assert_eq!(kept.len(), 1);
    }
}
