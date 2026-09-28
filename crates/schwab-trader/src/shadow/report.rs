//! `schwab-trader shadow report`: arm vs production from the active journals.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::agent::paths::{journal_path, shadow_journal_path, shadow_state_path};
use crate::journal::read_events_of_types;
use crate::risk::compute_sleeve_equity;
use crate::rules::{ShadowArmConfig, TraderRules};

use super::arm::{build_arm_rules, ShadowArmState};
use super::stats::{
    bootstrap_mean_ci, ci_excludes_zero, mean, profit_factor, win_rate_pct, BOOTSTRAP_RESAMPLES,
    BOOTSTRAP_SEED,
};

pub const CI_LEVEL: f64 = 0.90;
pub const PROMOTION_MIN_CLOSED: usize = 30;

struct Exit {
    at: DateTime<Utc>,
    pnl_pct: f64,
    pnl_usd: f64,
}

fn event_ts(e: &Value) -> Option<DateTime<Utc>> {
    e.get("ts")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_exits(events: &[Value], event_type: &str) -> Vec<Exit> {
    events
        .iter()
        .filter(|e| e.get("type").and_then(|v| v.as_str()) == Some(event_type))
        .filter_map(|e| {
            let p = e.get("payload")?;
            Some(Exit {
                at: event_ts(e)?,
                pnl_pct: p.get("pnl_pct")?.as_f64()?,
                pnl_usd: p.get("pnl_usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            })
        })
        .collect()
}

fn ci_json(ci: Option<(f64, f64)>) -> Value {
    ci.map(|(lo, hi)| json!([lo, hi])).unwrap_or(Value::Null)
}

fn exit_stats<'a>(exits: impl Iterator<Item = &'a Exit>) -> Value {
    let (pcts, usds): (Vec<f64>, Vec<f64>) = exits.map(|e| (e.pnl_pct, e.pnl_usd)).unzip();
    json!({
        "closed": pcts.len(),
        "win_rate_pct": win_rate_pct(&usds),
        "mean_pnl_pct": mean(&pcts),
        "mean_pnl_pct_ci90": ci_json(bootstrap_mean_ci(&pcts, CI_LEVEL, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED)),
        "profit_factor": profit_factor(&usds),
        "realized_usd": usds.iter().fold(0.0, |acc, x| acc + x),
    })
}

/// Day-paired `(arm change − production change)` from `shadow_day_summary`
/// events (last summary per date wins).
fn day_paired(events: &[Value]) -> Value {
    let mut by_date: BTreeMap<String, f64> = BTreeMap::new();
    for e in events
        .iter()
        .filter(|e| e.get("type").and_then(|v| v.as_str()) == Some("shadow_day_summary"))
    {
        let Some(p) = e.get("payload") else { continue };
        let (Some(date), Some(arm), Some(prod)) = (
            p.get("date").and_then(|v| v.as_str()),
            p.get("arm_equity_change_usd").and_then(|v| v.as_f64()),
            p.get("prod_equity_change_usd").and_then(|v| v.as_f64()),
        ) else {
            continue;
        };
        by_date.insert(date.to_string(), arm - prod);
    }
    let diffs: Vec<f64> = by_date.values().copied().collect();
    let ci = bootstrap_mean_ci(&diffs, CI_LEVEL, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
    json!({
        "days": diffs.len(),
        "first_date": by_date.keys().next(),
        "last_date": by_date.keys().next_back(),
        "mean_diff_usd": mean(&diffs),
        "mean_diff_usd_ci90": ci_json(ci),
        "arm_better_days": diffs.iter().filter(|d| **d > 0.0).count(),
        "ci_excludes_zero": ci_excludes_zero(ci),
    })
}

fn arm_report(
    rules_path: &Path,
    rules: &TraderRules,
    cfg: &ShadowArmConfig,
    prod_exits: &[Exit],
) -> Result<Value> {
    let config_status = match build_arm_rules(rules, rules_path, cfg) {
        Ok(_) => "ok".to_string(),
        Err(err) => format!("error: {err:#}"),
    };
    let state_path = shadow_state_path(rules_path, &rules.trader_id, &cfg.id);
    let journal = shadow_journal_path(rules_path, &rules.trader_id, &cfg.id);
    let state = ShadowArmState::load(&state_path, &cfg.id, &rules.trader_id)?;
    let events = read_events_of_types(&journal, &["shadow_exit_filled", "shadow_day_summary"])?;
    let arm_exits = parse_exits(&events, "shadow_exit_filled");

    let started_at = state
        .started_at
        .or_else(|| events.iter().filter_map(event_ts).min());
    let prod_window = prod_exits
        .iter()
        .filter(|e| started_at.is_some_and(|s| e.at >= s));

    let paired = day_paired(&events);
    let promotion_rule_met = arm_exits.len() >= PROMOTION_MIN_CLOSED
        && paired["ci_excludes_zero"].as_bool().unwrap_or(false);

    Ok(json!({
        "id": cfg.id,
        "config_status": config_status,
        "started_at": started_at,
        "state_path": state_path,
        "journal_path": journal,
        "arm": exit_stats(arm_exits.iter()),
        "production_same_window": exit_stats(prod_window),
        "open_positions": state.trader.open_positions.len(),
        "equity_usd": state.trader.sim.as_ref().map(|_| compute_sleeve_equity(&state.trader)),
        "active_profile": state.trader.active_profile,
        "day_paired": paired,
        "promotion_rule_met": promotion_rule_met,
    }))
}

pub fn build_report(rules_path: &Path, rules: &TraderRules) -> Result<Value> {
    let prod_journal = journal_path(rules_path);
    let prod_events = read_events_of_types(&prod_journal, &["sim_exit_filled"])?;
    let prod_exits = parse_exits(&prod_events, "sim_exit_filled");
    let arms = rules
        .shadow
        .arms
        .iter()
        .map(|cfg| arm_report(rules_path, rules, cfg, &prod_exits))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "trader_id": rules.trader_id,
        "shadow_enabled": rules.shadow.enabled,
        "production_journal": prod_journal,
        "ci_level": CI_LEVEL,
        "bootstrap": { "resamples": BOOTSTRAP_RESAMPLES, "seed": BOOTSTRAP_SEED },
        "promotion_rule": format!(
            "≥{PROMOTION_MIN_CLOSED} closed arm trades AND day-paired mean diff 90% CI excludes 0 → propose to the operator (never auto-promote)"
        ),
        "arms": arms,
    }))
}

fn fmt_opt(v: &Value, digits: usize, suffix: &str) -> String {
    v.as_f64()
        .map(|x| format!("{x:+.digits$}{suffix}"))
        .unwrap_or_else(|| "—".into())
}

fn fmt_ci(v: &Value, digits: usize) -> String {
    match (v.get(0).and_then(|x| x.as_f64()), v.get(1).and_then(|x| x.as_f64())) {
        (Some(lo), Some(hi)) => format!("[{lo:+.digits$}, {hi:+.digits$}]"),
        _ => "[—]".into(),
    }
}

fn stats_line(s: &Value) -> String {
    format!(
        "closed {}  win {}  mean {} 90% CI {}  PF {}  realized ${:+.2}",
        s["closed"],
        s["win_rate_pct"]
            .as_f64()
            .map(|w| format!("{w:.1}%"))
            .unwrap_or_else(|| "—".into()),
        fmt_opt(&s["mean_pnl_pct"], 2, "%"),
        fmt_ci(&s["mean_pnl_pct_ci90"], 2),
        s["profit_factor"]
            .as_f64()
            .map(|p| format!("{p:.2}"))
            .unwrap_or_else(|| "—".into()),
        s["realized_usd"].as_f64().unwrap_or(0.0),
    )
}

pub fn format_text(report: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "shadow report — trader {} (shadow {})",
        report["trader_id"].as_str().unwrap_or("?"),
        if report["shadow_enabled"].as_bool() == Some(true) {
            "enabled"
        } else {
            "disabled"
        }
    );
    let _ = writeln!(out, "rule: {}", report["promotion_rule"].as_str().unwrap_or(""));
    let arms = report["arms"].as_array().cloned().unwrap_or_default();
    if arms.is_empty() {
        let _ = writeln!(out, "no arms configured under `shadow.arms`");
    }
    for a in arms {
        let d = &a["day_paired"];
        let _ = writeln!(
            out,
            "\n{}  [{}]  started {}  open {}  profile {}",
            a["id"].as_str().unwrap_or("?"),
            a["config_status"].as_str().unwrap_or("?"),
            a["started_at"].as_str().unwrap_or("—"),
            a["open_positions"],
            a["active_profile"].as_str().unwrap_or("—"),
        );
        let _ = writeln!(out, "  arm:        {}", stats_line(&a["arm"]));
        let _ = writeln!(out, "  production: {}", stats_line(&a["production_same_window"]));
        let _ = writeln!(
            out,
            "  day-paired arm−prod: {} days  mean {} 90% CI {}  arm>prod {}/{}",
            d["days"],
            fmt_opt(&d["mean_diff_usd"], 2, " $"),
            fmt_ci(&d["mean_diff_usd_ci90"], 2),
            d["arm_better_days"],
            d["days"],
        );
        let _ = writeln!(
            out,
            "  promotion rule met: {}",
            if a["promotion_rule_met"].as_bool() == Some(true) {
                "YES — propose to the operator"
            } else {
                "no"
            }
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::append_event_to_path;

    #[test]
    fn report_pairs_days_and_windows_production() {
        let dir = tempfile::TempDir::new().unwrap();
        let rules_path = dir.path().join("trader-swing.yaml");
        let mut rules = TraderRules::default();
        rules.trader_id = "swing".into();
        rules.accounts = vec![crate::rules::TraderAccount {
            hash: "abc".into(),
            label: None,
            r#type: crate::rules::AccountType::Margin,
            enabled: true,
        }];
        rules.shadow.enabled = true;
        rules.shadow.arms = vec![ShadowArmConfig {
            id: "a1".into(),
            rules_file: None,
            overrides: json!({"playbook": {"entry": {"max_positions": 2}}}),
        }];

        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let prod = journal_path(&rules_path);
        append_event_to_path(&prod, t("2026-09-01T15:00:00Z"), "sim_exit_filled", json!({"pnl_pct": -9.0, "pnl_usd": -90.0})).unwrap();
        append_event_to_path(&prod, t("2026-09-29T15:00:00Z"), "sim_exit_filled", json!({"pnl_pct": 2.0, "pnl_usd": 20.0})).unwrap();

        let arm_journal = shadow_journal_path(&rules_path, "swing", "a1");
        for (pct, usd) in [(3.0, 30.0), (-1.0, -10.0)] {
            append_event_to_path(&arm_journal, t("2026-09-29T16:00:00Z"), "shadow_exit_filled", json!({"pnl_pct": pct, "pnl_usd": usd})).unwrap();
        }
        for (date, arm, prod) in [("2026-09-28", 10.0, 4.0), ("2026-09-29", -2.0, 1.0), ("2026-09-30", 7.0, 2.0)] {
            append_event_to_path(&arm_journal, t("2026-10-01T13:30:00Z"), "shadow_day_summary", json!({
                "date": date, "arm_equity_change_usd": arm, "prod_equity_change_usd": prod,
            })).unwrap();
        }
        let mut state = ShadowArmState::load(&shadow_state_path(&rules_path, "swing", "a1"), "a1", "swing").unwrap();
        state.started_at = Some(t("2026-09-28T13:30:00Z"));
        state.save(&shadow_state_path(&rules_path, "swing", "a1")).unwrap();

        let report = build_report(&rules_path, &rules).unwrap();
        let arm = &report["arms"][0];
        assert_eq!(arm["config_status"], json!("ok"));
        assert_eq!(arm["arm"]["closed"], json!(2));
        assert_eq!(arm["arm"]["profit_factor"], json!(3.0));
        assert_eq!(arm["production_same_window"]["closed"], json!(1), "pre-start prod exit excluded");
        assert_eq!(arm["day_paired"]["days"], json!(3));
        assert_eq!(arm["day_paired"]["arm_better_days"], json!(2));
        assert!((arm["day_paired"]["mean_diff_usd"].as_f64().unwrap() - 8.0 / 3.0).abs() < 1e-9);
        assert_eq!(arm["promotion_rule_met"], json!(false));
        assert!(format_text(&report).contains("a1  [ok]"));
    }
}
