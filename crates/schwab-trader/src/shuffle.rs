//! Entry shuffle: group caps, re-entry cooldown after stops, portfolio-adjusted candidate ranking.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use crate::agent::state::TraderState;
use crate::journal;
use crate::rules::TraderRules;
use crate::sim::ClosedSimTrade;

#[derive(Debug, Clone)]
pub struct SymbolExitRecord {
    pub symbol: String,
    pub exit_reason: String,
    pub closed_at: DateTime<Utc>,
    pub pnl_usd: f64,
}

#[derive(Debug, Clone)]
pub struct ShuffleAdjustment {
    pub raw_score: f64,
    pub adjusted_score: f64,
    pub notes: Vec<String>,
    pub overwhelming: bool,
}

/// Recent exits for shuffle lookback (sim ledger + journal fallback).
pub fn collect_recent_exits(
    state: &TraderState,
    rules_path: Option<&Path>,
    lookback_days: u32,
) -> Vec<SymbolExitRecord> {
    let cutoff = Utc::now() - Duration::days(lookback_days.max(1) as i64);
    let mut out: Vec<SymbolExitRecord> = Vec::new();

    if let Some(sim) = &state.sim {
        for t in &sim.closed_trades {
            if t.closed_at >= cutoff {
                out.push(exit_from_closed_trade(t));
            }
        }
    }

    if let Some(path) = rules_path {
        if let Ok(events) = journal::read_recent(path, 300) {
            for e in events {
                let Some(kind) = e.get("type").and_then(|v| v.as_str()) else {
                    continue;
                };
                if kind != "sim_exit_filled" && kind != "exit_filled" {
                    continue;
                }
                let payload = e.get("payload").cloned().unwrap_or(json!({}));
                let Some(closed_at) = parse_closed_at(&payload, e.get("ts")) else {
                    continue;
                };
                if closed_at < cutoff {
                    continue;
                }
                let symbol = payload
                    .get("symbol")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_uppercase();
                if symbol.is_empty() {
                    continue;
                }
                let rec = SymbolExitRecord {
                    symbol,
                    exit_reason: payload
                        .get("exit_reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    closed_at,
                    pnl_usd: payload
                        .get("pnl_usd")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                };
                if !out.iter().any(|x| {
                    x.symbol == rec.symbol
                        && x.closed_at == rec.closed_at
                        && x.exit_reason == rec.exit_reason
                }) {
                    out.push(rec);
                }
            }
        }
    }

    out.sort_by(|a, b| a.closed_at.cmp(&b.closed_at));
    out
}

fn exit_from_closed_trade(t: &ClosedSimTrade) -> SymbolExitRecord {
    SymbolExitRecord {
        symbol: t.symbol.trim().to_uppercase(),
        exit_reason: t.exit_reason.clone(),
        closed_at: t.closed_at,
        pnl_usd: t.pnl_usd,
    }
}

fn parse_closed_at(payload: &Value, ts: Option<&Value>) -> Option<DateTime<Utc>> {
    if let Some(s) = payload.get("closed_at").and_then(|v| v.as_str()) {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    ts.and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

pub fn open_group_counts(rules: &TraderRules, state: &TraderState) -> HashMap<String, u32> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for pos in state.open_positions.values() {
        if let Some(name) = rules.symbol_group_name(&pos.symbol) {
            *counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

/// Hard block when group `max_open` would be exceeded.
pub fn symbol_group_cap_reason(rules: &TraderRules, state: &TraderState, symbol: &str) -> Option<String> {
    let group = rules.symbol_group_name(symbol)?;
    let max_open = rules.symbol_group_max_open(group);
    if max_open == 0 {
        return None;
    }
    let open = open_group_counts(rules, state)
        .get(group)
        .copied()
        .unwrap_or(0);
    if open >= max_open {
        return Some(format!(
            "symbol_group_cap: {group} has {open}/{max_open} open"
        ));
    }
    None
}

fn recent_stops_on_symbol(exits: &[SymbolExitRecord], symbol: &str) -> u32 {
    let sym = symbol.trim().to_uppercase();
    exits
        .iter()
        .filter(|e| e.symbol == sym && e.exit_reason == "stop_loss")
        .count() as u32
}

fn days_since_last_stop(exits: &[SymbolExitRecord], symbol: &str) -> Option<i64> {
    let sym = symbol.trim().to_uppercase();
    exits
        .iter()
        .filter(|e| e.symbol == sym && e.exit_reason == "stop_loss")
        .map(|e| e.closed_at)
        .max()
        .map(|at| (Utc::now() - at).num_days())
}

pub fn compute_shuffle_adjustment(
    rules: &TraderRules,
    state: &TraderState,
    symbol: &str,
    raw_score: f64,
    _rs_vs_benchmark_30d: Option<f64>,
    recent_exits: &[SymbolExitRecord],
) -> ShuffleAdjustment {
    let shuffle = &rules.playbook.filters.shuffle;
    let mut notes = Vec::new();
    let mut adjusted = raw_score;

    if !shuffle.enabled {
        return ShuffleAdjustment {
            raw_score,
            adjusted_score: raw_score,
            notes,
            overwhelming: false,
        };
    }

    let stops = recent_stops_on_symbol(recent_exits, symbol);
    if stops > 0 {
        let pen = shuffle.stop_loss_penalty + shuffle.repeat_stop_penalty * (stops.saturating_sub(1)) as f64;
        adjusted -= pen;
        notes.push(format!("recent_stop_loss x{stops} (-{pen:.2})"));
    }

    if let Some(group) = rules.symbol_group_name(symbol) {
        let open_counts = open_group_counts(rules, state);
        let open_here = open_counts.get(group).copied().unwrap_or(0);
        if open_here > 0 {
            adjusted -= shuffle.same_group_open_penalty;
            notes.push(format!(
                "same_group_open:{group} (-{:.2})",
                shuffle.same_group_open_penalty
            ));
        } else if !open_counts.is_empty() {
            adjusted += shuffle.underrepresented_group_bonus;
            notes.push(format!(
                "underrepresented_group:{group} (+{:.2})",
                shuffle.underrepresented_group_bonus
            ));
        }
    }

    ShuffleAdjustment {
        raw_score,
        adjusted_score: adjusted,
        notes,
        overwhelming: false,
    }
}

/// Mark overwhelming candidates and return sorted (symbol, adjustment) pairs.
pub fn finalize_shuffle_ranking(
    rules: &TraderRules,
    mut ranked: Vec<(String, ShuffleAdjustment, Option<f64>)>,
) -> Vec<(String, ShuffleAdjustment, Option<f64>)> {
    let shuffle = &rules.playbook.filters.shuffle;
    if !shuffle.enabled || ranked.is_empty() {
        return ranked;
    }

    ranked.sort_by(|a, b| {
        b.1.adjusted_score
            .partial_cmp(&a.1.adjusted_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let second_best = ranked.get(1).map(|r| r.1.adjusted_score).unwrap_or(0.0);

    for (_sym, adj, rs) in ranked.iter_mut() {
        let margin = adj.adjusted_score - second_best;
        let rs_ok = shuffle
            .overwhelming_min_rs_vs_benchmark_30d
            .map(|min| rs.unwrap_or(0.0) >= min)
            .unwrap_or(true);
        if margin >= shuffle.overwhelming_margin && rs_ok {
            adj.overwhelming = true;
            adj.notes.push(format!("overwhelming (+{margin:.2} vs #2)"));
        }
    }

    ranked
}

/// Hard block for re-entry during stop-loss cooldown (unless overwhelming).
pub fn re_entry_cooldown_reason(
    rules: &TraderRules,
    symbol: &str,
    adjustment: &ShuffleAdjustment,
    recent_exits: &[SymbolExitRecord],
) -> Option<String> {
    let shuffle = &rules.playbook.filters.shuffle;
    if !shuffle.enabled || shuffle.re_entry_cooldown_days == 0 {
        return None;
    }
    if adjustment.overwhelming && shuffle.bypass_cooldown_on_overwhelming {
        return None;
    }
    let days = days_since_last_stop(recent_exits, symbol)?;
    if days < shuffle.re_entry_cooldown_days as i64 {
        return Some(format!(
            "re_entry_cooldown: stop_loss {days}d ago (< {}d)",
            shuffle.re_entry_cooldown_days
        ));
    }
    None
}

pub fn entry_shuffle_block_reason(
    rules: &TraderRules,
    state: &TraderState,
    symbol: &str,
    adjustment: &ShuffleAdjustment,
    recent_exits: &[SymbolExitRecord],
) -> Option<String> {
    if let Some(reason) = symbol_group_cap_reason(rules, state, symbol) {
        return Some(reason);
    }
    re_entry_cooldown_reason(rules, symbol, adjustment, recent_exits)
}

/// Entry guard using scan shuffle metadata when available (post-ranking).
pub fn entry_shuffle_block_from_scan(
    rules: &TraderRules,
    state: &TraderState,
    symbol: &str,
    scan: &Value,
    rules_path: Option<&Path>,
) -> Option<String> {
    if let Some(reason) = symbol_group_cap_reason(rules, state, symbol) {
        return Some(reason);
    }
    let shuffle = &rules.playbook.filters.shuffle;
    if !shuffle.enabled {
        return None;
    }
    let sym_u = symbol.trim().to_uppercase();
    let recent_exits = collect_recent_exits(state, rules_path, shuffle.lookback_days);
    let overwhelming = scan
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|c| {
                c.get("symbol")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(&sym_u))
            })
        })
        .and_then(|c| c.get("shuffle_overwhelming"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let raw = scan
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|c| {
                c.get("symbol")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(&sym_u))
            })
        })
        .and_then(|c| c.get("score"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let mut adj = compute_shuffle_adjustment(&rules, state, symbol, raw, None, &recent_exits);
    adj.overwhelming = overwhelming;
    re_entry_cooldown_reason(rules, symbol, &adj, &recent_exits)
}

/// Apply shuffle scoring to scan candidates (mutates and re-sorts).
pub fn apply_shuffle_to_scan(
    rules: &TraderRules,
    state: &TraderState,
    rules_path: Option<&Path>,
    candidates: &mut [Value],
) {
    let shuffle = &rules.playbook.filters.shuffle;
    if !shuffle.enabled {
        return;
    }

    let recent_exits = collect_recent_exits(state, rules_path, shuffle.lookback_days);

    let mut ranked: Vec<(String, ShuffleAdjustment, Option<f64>)> = Vec::new();
    for c in candidates.iter() {
        let Some(sym) = c.get("symbol").and_then(|v| v.as_str()) else {
            continue;
        };
        let raw = c.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let rs = c
            .get("technical_context")
            .and_then(|tc| tc.get("history_features"))
            .and_then(|hf| hf.get("rs_vs_benchmark_30d_pct"))
            .and_then(|v| v.as_f64());
        let adj = compute_shuffle_adjustment(rules, state, sym, raw, rs, &recent_exits);
        ranked.push((sym.to_string(), adj, rs));
    }

    ranked = finalize_shuffle_ranking(rules, ranked);

    let order: HashMap<String, usize> = ranked
        .iter()
        .enumerate()
        .map(|(i, (s, _, _))| (s.clone(), i))
        .collect();
    let adj_map: HashMap<String, ShuffleAdjustment> = ranked
        .into_iter()
        .map(|(s, a, _)| (s, a))
        .collect();

    for c in candidates.iter_mut() {
        let Some(sym) = c.get("symbol").and_then(|v| v.as_str()) else {
            continue;
        };
        let sym_u = sym.trim().to_uppercase();
        if let Some(adj) = adj_map.get(&sym_u) {
            if let Some(obj) = c.as_object_mut() {
                obj.insert("adjusted_score".into(), json!(adj.adjusted_score));
                obj.insert("shuffle_notes".into(), json!(adj.notes));
                obj.insert("shuffle_overwhelming".into(), json!(adj.overwhelming));
            }
        }
    }

    candidates.sort_by(|a, b| {
        let sa = a
            .get("symbol")
            .and_then(|v| v.as_str())
            .map(|s| order.get(&s.trim().to_uppercase()).copied().unwrap_or(usize::MAX))
            .unwrap_or(usize::MAX);
        let sb = b
            .get("symbol")
            .and_then(|v| v.as_str())
            .map(|s| order.get(&s.trim().to_uppercase()).copied().unwrap_or(usize::MAX))
            .unwrap_or(usize::MAX);
        sa.cmp(&sb)
    });
}

pub fn build_entry_shuffle_context(
    rules: &TraderRules,
    state: &TraderState,
    rules_path: Option<&Path>,
    scan: &Value,
) -> Value {
    let shuffle = &rules.playbook.filters.shuffle;
    if !shuffle.enabled {
        return json!({ "enabled": false });
    }

    let recent_exits = collect_recent_exits(state, rules_path, shuffle.lookback_days);
    let mut by_symbol: HashMap<String, Value> = HashMap::new();
    for e in &recent_exits {
        let entry = by_symbol.entry(e.symbol.clone()).or_insert_with(|| {
            json!({
                "symbol": e.symbol,
                "stops_in_lookback": 0,
                "last_exit_reason": null,
                "days_since_last_stop": null,
            })
        });
        if e.exit_reason == "stop_loss" {
            if let Some(n) = entry.get_mut("stops_in_lookback") {
                *n = json!(n.as_u64().unwrap_or(0) + 1);
            }
            if let Some(d) = days_since_last_stop(&recent_exits, &e.symbol) {
                entry["days_since_last_stop"] = json!(d);
            }
        }
        entry["last_exit_reason"] = json!(e.exit_reason);
        entry["last_exit_pnl_usd"] = json!(e.pnl_usd);
    }

    let top_adjusted: Vec<Value> = scan
        .get("candidates")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(8)
                .map(|c| {
                    json!({
                        "symbol": c.get("symbol"),
                        "score": c.get("score"),
                        "adjusted_score": c.get("adjusted_score"),
                        "shuffle_notes": c.get("shuffle_notes"),
                        "shuffle_overwhelming": c.get("shuffle_overwhelming"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "enabled": true,
        "open_groups": open_group_counts(rules, state),
        "symbol_groups": rules.playbook.filters.symbol_groups,
        "recent_exits_by_symbol": by_symbol,
        "top_adjusted_candidates": top_adjusted,
        "policy": "Prefer underrepresented groups; defer repeat symbols after recent stop_loss unless shuffle_overwhelming is true.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::state::SwingPosition;
    use chrono::TimeZone;

    fn rules_with_groups() -> TraderRules {
        let mut rules = TraderRules::default();
        rules.playbook.filters.symbol_groups = vec![
            crate::rules::SymbolGroupConfig {
                name: "semicap".into(),
                symbols: vec!["AMD".into(), "ASML".into(), "SMH".into()],
                max_open: 1,
            },
            crate::rules::SymbolGroupConfig {
                name: "industrial".into(),
                symbols: vec!["CAT".into(), "RTX".into(), "XLI".into()],
                max_open: 1,
            },
        ];
        rules.playbook.filters.shuffle.enabled = true;
        rules.playbook.filters.shuffle.re_entry_cooldown_days = 2;
        rules
    }

    #[test]
    fn group_cap_blocks_second_semicap() {
        let rules = rules_with_groups();
        let mut state = TraderState::default();
        state.open_positions.insert(
            "AMD|2026-07-01".into(),
            SwingPosition {
                symbol: "AMD".into(),
                ..Default::default()
            },
        );
        let reason = symbol_group_cap_reason(&rules, &state, "ASML");
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("semicap"));
    }

    #[test]
    fn stop_penalty_lowers_adjusted_score() {
        let rules = rules_with_groups();
        let _state = TraderState::default();
        let at = Utc.with_ymd_and_hms(2026, 7, 9, 12, 0, 0).unwrap();
        let exits = vec![SymbolExitRecord {
            symbol: "AMD".into(),
            exit_reason: "stop_loss".into(),
            closed_at: at,
            pnl_usd: -10.0,
        }];
        let adj = compute_shuffle_adjustment(&rules, &_state, "AMD", 0.8, Some(8.0), &exits);
        assert!(adj.adjusted_score < adj.raw_score);
    }

    #[test]
    fn cooldown_blocks_recent_stop_reentry() {
        let rules = rules_with_groups();
        let _state = TraderState::default();
        let at = Utc::now() - Duration::hours(12);
        let exits = vec![SymbolExitRecord {
            symbol: "AMD".into(),
            exit_reason: "stop_loss".into(),
            closed_at: at,
            pnl_usd: -5.0,
        }];
        let adj = ShuffleAdjustment {
            raw_score: 0.8,
            adjusted_score: 0.5,
            notes: vec![],
            overwhelming: false,
        };
        let reason = re_entry_cooldown_reason(&rules, "AMD", &adj, &exits);
        assert!(reason.is_some());
    }

    #[test]
    fn overwhelming_bypasses_cooldown() {
        let rules = rules_with_groups();
        let _state = TraderState::default();
        let at = Utc::now() - Duration::hours(6);
        let exits = vec![SymbolExitRecord {
            symbol: "AMD".into(),
            exit_reason: "stop_loss".into(),
            closed_at: at,
            pnl_usd: -5.0,
        }];
        let adj = ShuffleAdjustment {
            raw_score: 0.9,
            adjusted_score: 0.85,
            notes: vec![],
            overwhelming: true,
        };
        assert!(re_entry_cooldown_reason(&rules, "AMD", &adj, &exits).is_none());
    }
}
