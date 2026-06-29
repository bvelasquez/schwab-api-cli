use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use crate::agent::paths::journal_path;
use crate::agent::state::load_state;
use crate::rules::TraderRules;
use crate::sim::compute_stats;

pub fn append_event(rules_path: &Path, event_type: &str, payload: Value) -> Result<()> {
    let path = journal_path(rules_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "type": event_type,
        "payload": payload,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open journal {}", path.display()))?;
    writeln!(file, "{}", line)?;
    Ok(())
}

pub fn read_recent(rules_path: &Path, limit: usize) -> Result<Vec<Value>> {
    read_all(rules_path).map(|mut events| {
        if events.len() > limit {
            events = events.split_off(events.len() - limit);
        }
        events
    })
}

pub fn read_all(rules_path: &Path) -> Result<Vec<Value>> {
    let path = journal_path(rules_path);
    if !path.is_file() {
        return Ok(vec![]);
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

pub fn stats_from_journal(rules_path: &Path) -> Result<Value> {
    let events = read_all(rules_path)?;
    let mut counts: HashMap<String, u32> = HashMap::new();
    for e in &events {
        if let Some(t) = e.get("type").and_then(|v| v.as_str()) {
            *counts.entry(t.to_string()).or_insert(0) += 1;
        }
    }
    Ok(serde_json::json!({
        "events_total": events.len(),
        "event_counts": counts,
        "entries_filled": counts.get("entry_filled").copied().unwrap_or(0)
            + counts.get("sim_entry_filled").copied().unwrap_or(0),
        "exits_filled": counts.get("exit_filled").copied().unwrap_or(0)
            + counts.get("sim_exit_filled").copied().unwrap_or(0),
        "rule_adaptations": counts.get("rule_auto_applied").copied().unwrap_or(0),
        "sim_tick_summaries": counts.get("sim_tick_summary").copied().unwrap_or(0),
        "profile_changes": counts.get("profile_changed").copied().unwrap_or(0),
        "journal_path": journal_path(rules_path),
    }))
}

/// Full simulation analysis report for post-run review (e.g. after a week of --simulate).
pub fn build_sim_analysis_report(rules_path: &Path, rules: &TraderRules) -> Result<Value> {
    let state = load_state(rules_path, &rules.trader_id)?;
    let events = read_all(rules_path)?;
    let stats = compute_stats(&state);

    let mut event_counts: HashMap<String, u32> = HashMap::new();
    let mut sim_trades = Vec::new();
    let mut profile_timeline = Vec::new();
    let mut regime_timeline = Vec::new();
    let mut adaptations = Vec::new();
    let mut llm_summaries = Vec::new();
    let mut trailing_updates = Vec::new();
    let mut entry_vetoes = 0u32;

    for e in &events {
        let Some(event_type) = e.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        *event_counts.entry(event_type.to_string()).or_insert(0) += 1;
        let payload = e.get("payload").cloned().unwrap_or(json!({}));
        let ts = e.get("ts").cloned();

        match event_type {
            "sim_entry_filled" => {
                sim_trades.push(json!({
                    "ts": ts,
                    "kind": "entry",
                    "payload": payload,
                }));
            }
            "sim_exit_filled" => {
                sim_trades.push(json!({
                    "ts": ts,
                    "kind": "exit",
                    "payload": payload,
                }));
            }
            "profile_changed" => profile_timeline.push(json!({ "ts": ts, "payload": payload })),
            "sim_tick_summary" => {
                if let Some(regime) = payload.get("regime") {
                    regime_timeline.push(json!({ "ts": ts, "regime": regime }));
                }
            }
            "rule_auto_applied" | "rule_patch_proposed" => {
                adaptations.push(json!({ "ts": ts, "type": event_type, "payload": payload }));
            }
            "sim_trailing_stop_updated" => {
                trailing_updates.push(json!({ "ts": ts, "payload": payload }));
            }
            _ => {}
        }

        if event_type == "sim_tick_summary" {
            if payload
                .get("entry_attempts")
                .and_then(|v| v.as_array())
                .is_some_and(|a| {
                    a.iter().any(|x| {
                        x.get("reason")
                            .and_then(|r| r.as_str())
                            == Some("llm_veto_or_missing_review")
                    })
                })
            {
                entry_vetoes += 1;
            }
            if payload.get("llm").is_some() {
                llm_summaries.push(json!({ "ts": ts, "llm": payload.get("llm") }));
            }
        }
    }

    let ledger_trades = state
        .sim
        .as_ref()
        .map(|l| serde_json::to_value(&l.closed_trades).unwrap_or(json!([])))
        .unwrap_or(json!([]));

    let equity_curve = state
        .sim
        .as_ref()
        .map(|l| serde_json::to_value(&l.equity_snapshots).unwrap_or(json!([])))
        .unwrap_or(json!([]));

    let first_ts = events.first().and_then(|e| e.get("ts"));
    let last_ts = events.last().and_then(|e| e.get("ts"));

    Ok(json!({
        "generated_at": Utc::now().to_rfc3339(),
        "rules_file": rules_path,
        "trader_id": rules.trader_id,
        "journal_path": journal_path(rules_path),
        "period": { "first_event": first_ts, "last_event": last_ts },
        "ledger_stats": stats,
        "event_counts": event_counts,
        "closed_trades_ledger": ledger_trades,
        "equity_curve": equity_curve,
        "trade_journal": sim_trades,
        "profile_timeline": profile_timeline,
        "regime_timeline": regime_timeline,
        "adaptations": adaptations,
        "trailing_stop_updates": trailing_updates,
        "llm_review_count": llm_summaries.len(),
        "llm_entry_veto_ticks": entry_vetoes,
        "active_profile": state.active_profile,
        "last_regime": state.last_regime,
        "tick_count": state.tick_count,
        "closed_trades_since_learn": state.closed_trades_since_learn,
    }))
}

pub fn journal_path_display(rules_path: &Path) -> PathBuf {
    journal_path(rules_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn analysis_report_aggregates_sim_events() {
        let dir = TempDir::new().unwrap();
        let rules_path = dir.path().join("trader-test.yaml");
        std::fs::write(&rules_path, "version: 1\ntrader_id: test\n").unwrap();

        append_event(
            &rules_path,
            "sim_entry_filled",
            json!({ "symbol": "AAPL", "fill_price": 100.0 }),
        )
        .unwrap();
        append_event(
            &rules_path,
            "sim_exit_filled",
            json!({ "symbol": "AAPL", "exit_reason": "profit_target", "pnl_usd": 50.0 }),
        )
        .unwrap();

        let mut rules = TraderRules::default();
        rules.trader_id = "test".into();
        rules.accounts = vec![crate::rules::TraderAccount {
            hash: "abc".into(),
            label: None,
            r#type: crate::rules::AccountType::Margin,
            enabled: true,
        }];

        let report = build_sim_analysis_report(&rules_path, &rules).unwrap();
        assert_eq!(
            report
                .pointer("/event_counts/sim_entry_filled")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            report
                .pointer("/event_counts/sim_exit_filled")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
    }
}
