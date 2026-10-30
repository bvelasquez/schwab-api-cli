//! Read-only HTML view of paper swing and options books.
//!
//! Serves state, recent closes, journal lines, and shadow-arm summaries.
//! It never places orders and never reads credentials.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

fn trader_rules_dir(rules_path: &Path) -> PathBuf {
    rules_path
        .parent()
        .map(Path::to_path_buf)
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn rules_stem(rules_path: &Path) -> String {
    rules_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("trader")
        .to_string()
}

fn state_path(rules_path: &Path) -> PathBuf {
    trader_rules_dir(rules_path).join(format!("trader-state-{}.json", rules_stem(rules_path)))
}

fn journal_path(rules_path: &Path) -> PathBuf {
    trader_rules_dir(rules_path).join(format!("trader-journal-{}.jsonl", rules_stem(rules_path)))
}

fn shadow_state_path(rules_path: &Path, trader_id: &str, arm_id: &str) -> PathBuf {
    trader_rules_dir(rules_path).join(format!("trader-shadow-state-{trader_id}-{arm_id}.json"))
}

const PAGE: &str = include_str!("dashboard.html");
const JOURNAL_CHUNK_BYTES: u64 = 256 * 1024;
const JOURNAL_SEARCH_BYTES: u64 = 8 * 1024 * 1024;
const JOURNAL_LIMIT: usize = 40;
const CLOSE_LIMIT: usize = 12;
/// Ticks shown in the research tab history.
const RESEARCH_HISTORY_LIMIT: usize = 60;
/// A nightly artifact older than this is shown as stale on the dashboard.
const LAB_STALE_HOURS: f64 = 36.0;

pub struct DashboardPaths {
    pub swing: Vec<PathBuf>,
    pub options: Vec<PathBuf>,
    /// rules/arms/families.json declaring which options agents are duplicate
    /// arms of one experiment (their losses stack). Optional.
    pub families: Option<PathBuf>,
}

pub fn serve(binds: &[String], paths: DashboardPaths) -> Result<()> {
    anyhow::ensure!(
        !binds.is_empty(),
        "dashboard needs at least one --bind address"
    );
    let mut listeners = Vec::with_capacity(binds.len());
    for bind in binds {
        let listener = TcpListener::bind(bind).with_context(|| format!("bind {bind}"))?;
        eprintln!("paper dashboard http://{bind}  (read-only, no orders)");
        listeners.push(listener);
    }
    let paths = Arc::new(paths);
    let first = listeners.remove(0);
    for listener in listeners {
        let paths = Arc::clone(&paths);
        std::thread::spawn(move || accept_loop(listener, paths));
    }
    accept_loop(first, paths);
    Ok(())
}

fn accept_loop(listener: TcpListener, paths: Arc<DashboardPaths>) {
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let paths = Arc::clone(&paths);
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = handle(stream, &paths);
        });
    }
}

pub fn snapshot(paths: &DashboardPaths) -> Value {
    let mut books = Vec::new();
    for rules in &paths.swing {
        books.push(swing_book(rules));
    }
    for rules in &paths.options {
        books.push(options_book(rules));
    }
    for book in agent_trader_paper_books(rules_dir(paths)).into_iter().rev() {
        books.insert(0, book);
    }
    let families = families_section(paths, &books);
    stamp_book_roles(&mut books, &families);
    json!({
        "generated_at": Utc::now().to_rfc3339(),
        "books": books,
        "underlyings": underlying_prices(rules_dir(paths)),
        "families": families,
        "lab": lab_section(paths),
        "research": research_section(&research_dir()),
    })
}

fn stamp_book_roles(books: &mut [Value], families: &Value) {
    let Some(roles) = families.get("roles").and_then(Value::as_object) else {
        return;
    };
    for book in books {
        let Some(name) = book.get("name").and_then(Value::as_str) else {
            continue;
        };
        if let Some(role) = roles.get(name) {
            book["role"] = role.clone();
        }
    }
}

fn member_ref(value: &Value) -> Option<(String, Option<String>)> {
    if let Some(id) = value.as_str() {
        if id.is_empty() {
            return None;
        }
        return Some((id.to_string(), None));
    }
    let id = value.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        return None;
    }
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
        .map(str::to_string);
    Some((id.to_string(), role))
}

/// Stack duplicate arm experiments into family rows: member realized P&L and
/// open marks add up, identical open bets across clones counted once. This is the
/// loss view the dashboard must show — a losing clone family must read as one
/// stacked loss, not as n small near-breakeven books. Member identity is the
/// agent_id from the rules file (e.g. options-arm-o0), matched to book.name.
fn families_section(paths: &DashboardPaths, books: &[Value]) -> Value {
    let Some(fp) = &paths.families else {
        return json!({ "families": [] });
    };
    let Ok(doc) = read_json(fp) else {
        return json!({ "families": [] });
    };
    let Some(fams) = doc.get("families").and_then(Value::as_array) else {
        return json!({ "families": [] });
    };
    let arr: Vec<Value> = fams
        .iter()
        .filter_map(|f| {
            let id = f.get("id")?.as_str()?.to_string();
            let label = f
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());
            let stack = f.get("stack").and_then(Value::as_bool).unwrap_or(true);
            let members: Vec<(String, Option<String>)> = f
                .get("members")
                .and_then(Value::as_array)
                .map(|rows| rows.iter().filter_map(member_ref).collect())
                .unwrap_or_default();
            let declared: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
            let mut present: Vec<String> = Vec::new();
            let mut realized = 0.0;
            for book in books {
                let name = book.get("name").and_then(Value::as_str).unwrap_or("");
                if !declared.iter().any(|m| m == name) {
                    continue;
                }
                present.push(name.to_string());
                realized += book
                    .get("clean_realized_pnl_usd")
                    .and_then(Value::as_f64)
                    .or_else(|| book.get("realized_pnl_usd").and_then(Value::as_f64))
                    .unwrap_or(0.0);
            }
            // Same-open-bet dedup: across clones the identical spread on the same
            // underlying/expiry/strikes is one mark, not one per sleeve. Use the
            // bet identity (not a position id, which differs per agent).
            let mut seen_bets: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut close_bets: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut open_mark = 0.0;
            let mut nominal_entries = 0usize;
            let mut mark_suspect = false;
            for book in books {
                let name = book.get("name").and_then(Value::as_str).unwrap_or("");
                if !declared.iter().any(|m| m == name) {
                    continue;
                }
                if book_mark_suspect(book) {
                    mark_suspect = true;
                }
                if let Some(positions) = book.get("positions").and_then(Value::as_array) {
                    nominal_entries += positions.len();
                    for pos in positions {
                        let mark = pos
                            .get("unrealized_usd")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0);
                        if seen_bets.insert(open_bet_key(pos)) {
                            open_mark += mark;
                        }
                    }
                }
                let closes = book_close_keys(book);
                nominal_entries += closes.len();
                close_bets.extend(closes);
            }
            let effective_entries = seen_bets.len() + close_bets.len();
            let replication_count = if effective_entries == 0 {
                0.0
            } else {
                nominal_entries as f64 / effective_entries as f64
            };
            let sleeve = f.get("sleeve_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let net = realized + open_mark;
            let roi = if sleeve > 0.0 {
                100.0 * net / sleeve
            } else {
                0.0
            };
            if present.is_empty() && open_mark == 0.0 && realized == 0.0 {
                return None;
            }
            Some(json!({
                "id": id,
                "label": label,
                "stack": stack,
                "declared_members": declared,
                "present_members": present,
                "member_roles": members.iter().map(|(member_id, role)| json!({
                    "id": member_id,
                    "role": role.clone(),
                })).collect::<Vec<_>>(),
                "sleeve_usd": sleeve,
                "stacked_realized_usd": realized,
                "stacked_open_mark_usd": open_mark,
                "stacked_net_usd": net,
                "roi_pct": roi,
                "nominal_entries": nominal_entries,
                "effective_entries": effective_entries,
                "replication_count": replication_count,
                "mark_suspect": mark_suspect,
            }))
        })
        .collect();
    let mut roles = serde_json::Map::new();
    for fam in fams {
        let Some(rows) = fam.get("members").and_then(Value::as_array) else {
            continue;
        };
        for (id, role) in rows.iter().filter_map(member_ref) {
            if let Some(role) = role {
                roles.insert(id, json!(role));
            }
        }
    }
    json!({ "families": arr, "roles": roles })
}

fn open_bet_key(pos: &Value) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        pos.get("underlying").and_then(Value::as_str).unwrap_or(""),
        pos.get("expiry").and_then(Value::as_str).unwrap_or(""),
        pos.get("spread_type").and_then(Value::as_str).unwrap_or(""),
        pos.get("short_strike")
            .and_then(Value::as_f64)
            .map(|strike| strike.to_string())
            .unwrap_or_default(),
        pos.get("long_strike")
            .and_then(Value::as_f64)
            .map(|strike| strike.to_string())
            .unwrap_or_default()
    )
}

fn close_bet_key(book: &str, trade: &Value) -> String {
    let underlying = trade
        .get("underlying")
        .and_then(Value::as_str)
        .or_else(|| trade.get("symbol").and_then(Value::as_str))
        .unwrap_or("");
    let expiry = trade.get("expiry").and_then(Value::as_str).unwrap_or("");
    let short = trade
        .get("short_strike")
        .and_then(Value::as_f64)
        .map(|strike| strike.to_string())
        .unwrap_or_default();
    let long = trade
        .get("long_strike")
        .and_then(Value::as_f64)
        .map(|strike| strike.to_string())
        .unwrap_or_default();
    if underlying.is_empty() && expiry.is_empty() && short.is_empty() && long.is_empty() {
        return format!(
            "{book}|{}|{}|{}",
            trade.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0),
            trade.get("closed_at").and_then(Value::as_str).unwrap_or(""),
            trade
                .get("exit_reason")
                .and_then(Value::as_str)
                .unwrap_or("")
        );
    }
    format!(
        "{underlying}|{expiry}|{short}|{long}|{}|{}",
        trade.get("closed_at").and_then(Value::as_str).unwrap_or(""),
        trade
            .get("exit_reason")
            .and_then(Value::as_str)
            .unwrap_or("")
    )
}

fn book_close_keys(book: &Value) -> Vec<String> {
    let name = book.get("name").and_then(Value::as_str).unwrap_or("");
    if let Some(keys) = book.get("closed_bet_keys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(|key| key.as_str().map(str::to_string))
            .collect();
    }
    book.get("recent_closes")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(|row| close_bet_key(name, row)).collect())
        .unwrap_or_default()
}

fn book_mark_suspect(book: &Value) -> bool {
    if book
        .get("mark_suspect_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        return true;
    }
    book.get("recent_closes")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter()
                .any(|row| row.get("mark_suspect").and_then(Value::as_bool) == Some(true))
        })
}

/// Directory that holds the options rules files, and beside them `chains/` and `scorecards/`.
/// Books written by `agent-trader options paper`, `agent-trader-swing paper`,
/// and a workflow paper tick. Absent until those loops have ticked.
fn agent_trader_paper_books(dir: &Path) -> Vec<Value> {
    let mut names = vec![
        "agent-trader-public-paper.json".to_string(),
        "agent-trader-public-swing-paper.json".to_string(),
    ];
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut extra: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (name.starts_with("agent-trader-workflow-") && name.ends_with(".json"))
                    .then_some(name)
            })
            .collect();
        extra.sort();
        names.extend(extra);
    }
    names
        .into_iter()
        .filter_map(|name| agent_trader_paper_file(&dir.join(name)))
        .collect()
}

fn agent_trader_paper_file(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    let book: Value = serde_json::from_str(&raw).ok()?;
    if book.get("stack").and_then(|value| value.as_str()) != Some("agent-trader") {
        return None;
    }
    if book.get("broker").and_then(|value| value.as_str()) != Some("public") {
        return None;
    }
    let kind = book.get("kind").and_then(|value| value.as_str());
    if kind != Some("options") && kind != Some("swing") && kind != Some("workflow") {
        return None;
    }
    Some(book)
}

/// Directory holding the research-paper arm journals. Defaults to
/// `<state_dir>/paper-research`; `AGENT_TRADER_RESEARCH_DIR` overrides it.
fn research_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AGENT_TRADER_RESEARCH_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    crate::paths::state_dir()
        .map(|dir| dir.join("paper-research"))
        .unwrap_or_else(|_| PathBuf::from("paper-research"))
}

/// Latest research-arm tick per arm plus a bounded tick history.
/// The arm is paper-only; `orders_placed` and `place_attempts` must be zero.
fn research_section(dir: &Path) -> Value {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("journal-") && name.ends_with(".jsonl"))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    if files.is_empty() {
        return json!({"present": false, "dir": dir.display().to_string(), "arms": [], "history": []});
    }
    let mut records: Vec<Value> = Vec::new();
    for file in &files {
        records.extend(research_tail(file));
    }
    records.sort_by(|a, b| {
        let key = |v: &Value| v.get("at").and_then(Value::as_str).unwrap_or("").to_string();
        key(a).cmp(&key(b))
    });
    if records.is_empty() {
        return json!({"present": false, "dir": dir.display().to_string(), "arms": [], "history": []});
    }
    let start = records.len().saturating_sub(RESEARCH_HISTORY_LIMIT);
    let history: Vec<Value> = records[start..].iter().map(research_history_row).collect();
    let mut latest: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for record in &records {
        let arm = record
            .get("arm_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        latest.insert(arm, research_arm_summary(record));
    }
    json!({
        "present": true,
        "dir": dir.display().to_string(),
        "arms": latest.into_values().collect::<Vec<_>>(),
        "history": history,
    })
}

/// Last `RESEARCH_HISTORY_LIMIT` `research_tick` records from one journal file.
fn research_tail(path: &Path) -> Vec<Value> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    let mut end = len;
    let search_floor = len.saturating_sub(JOURNAL_SEARCH_BYTES);
    while end > search_floor && records.len() < RESEARCH_HISTORY_LIMIT {
        let start = end.saturating_sub(JOURNAL_CHUNK_BYTES).max(search_floor);
        let Ok(text) = read_range(&mut file, start, end) else {
            break;
        };
        end = start;
        for line in text.lines().rev() {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if record.get("kind").and_then(Value::as_str) != Some("research_tick") {
                continue;
            }
            records.push(record);
            if records.len() == RESEARCH_HISTORY_LIMIT {
                break;
            }
        }
        if start == 0 {
            break;
        }
    }
    records.reverse();
    records
}

fn research_arm_summary(record: &Value) -> Value {
    let failures: Vec<Value> = record
        .pointer("/gate/checks")
        .and_then(Value::as_array)
        .map(|checks| {
            checks
                .iter()
                .filter(|check| check.get("passed").and_then(Value::as_bool) != Some(true))
                .filter_map(|check| check.get("name").cloned())
                .collect()
        })
        .unwrap_or_default();
    json!({
        "arm_id": record.get("arm_id").cloned().unwrap_or(Value::Null),
        "phase": record.get("phase").cloned().unwrap_or(Value::Null),
        "last_tick": record.get("at").cloned().unwrap_or(Value::Null),
        "account": record.get("account").cloned().unwrap_or(Value::Null),
        "gate_ok": record.pointer("/gate/ok").and_then(Value::as_bool).unwrap_or(false),
        "gate_failures": failures,
        "quote_count": record.get("quote_count").cloned().unwrap_or(Value::Null),
        "orders_placed": record.get("orders_placed").cloned().unwrap_or(json!(0)),
        "place_attempts": record.get("place_attempts").cloned().unwrap_or(json!(0)),
        "citations": record.get("citations").cloned().unwrap_or(json!([])),
        "forecast_note": record.get("forecast_note").cloned().unwrap_or(Value::Null),
        "applied_patches": record.get("applied_patches").cloned().unwrap_or(json!([])),
        "forecast": record.get("forecast").cloned().unwrap_or(Value::Null),
    })
}

fn research_history_row(record: &Value) -> Value {
    json!({
        "at": record.get("at").cloned().unwrap_or(Value::Null),
        "arm_id": record.get("arm_id").cloned().unwrap_or(Value::Null),
        "gate_ok": record.pointer("/gate/ok").and_then(Value::as_bool).unwrap_or(false),
        "regime": record.pointer("/forecast/regime").cloned().unwrap_or(Value::Null),
        "signals": record.pointer("/forecast/signals").and_then(Value::as_array).map(|s| s.len()).unwrap_or(0),
        "citations": record.get("citations").and_then(Value::as_array).map(|c| c.len()).unwrap_or(0),
        "orders_placed": record.get("orders_placed").cloned().unwrap_or(json!(0)),
        "place_attempts": record.get("place_attempts").cloned().unwrap_or(json!(0)),
    })
}

fn rules_dir(paths: &DashboardPaths) -> &Path {
    // Options are paused, so fall back to the swing rules directory when no options file is given.
    paths
        .options
        .first()
        .or_else(|| paths.swing.first())
        .and_then(|p| p.parent())
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .unwrap_or_else(|| Path::new("."))
}

/// Last recorded underlying prices from chain snapshots (`rules/chains/latest-underlyings.json`).
/// Missing file is an empty object. The dashboard never fetches a quote itself.
fn underlying_prices(rules_dir: &Path) -> Value {
    read_json(&rules_dir.join("chains").join("latest-underlyings.json")).unwrap_or(json!({}))
}

/// Nightly evidence files (scorecard, benchmarks, slippage calibration, promotion gate).
/// They live under the rules directory that holds the options rules files. Missing
/// files are reported as `present: false`; they are never an error.
fn lab_section(paths: &DashboardPaths) -> Value {
    let rules_dir = rules_dir(paths);
    json!({
        "scorecard": lab_artifact(&rules_dir.join("scorecards").join("latest.json"), |doc| {
            json!({
                "date": doc.get("date").cloned().unwrap_or(Value::Null),
                "options_arms": doc.get("options_arms").cloned().unwrap_or(Value::Null),
                "drift_alerts": doc.get("drift_alerts").cloned().unwrap_or(Value::Null),
            })
        }),
        "benchmarks": lab_artifact(&rules_dir.join("benchmarks").join("latest.json"), |doc| {
            let mut doc = doc.clone();
            // Per-trade rows are bulky and not shown; keep the totals.
            if let Some(agents) = doc.get_mut("agents").and_then(Value::as_object_mut) {
                for agent in agents.values_mut() {
                    if let Some(obj) = agent.as_object_mut() {
                        obj.remove("trades");
                    }
                }
            }
            doc
        }),
        "slippage": lab_artifact(&rules_dir.join("analysis").join("slippage-calibration.json"), |doc| {
            json!({
                "snapshots": doc.get("snapshots").cloned().unwrap_or(Value::Null),
                "current_8709_fill_slippage_pct": doc.get("current_8709_fill_slippage_pct").cloned().unwrap_or(Value::Null),
                "recommended": doc.get("recommended").cloned().unwrap_or(Value::Null),
                "by_symbol_and_dte": doc.get("by_symbol_and_dte").cloned().unwrap_or(Value::Null),
                "paper_fills_vs_quotes": doc.get("paper_fills_vs_quotes").cloned().unwrap_or(Value::Null),
            })
        }),
        "swing_lab": lab_artifact(&rules_dir.join("analysis").join("swing-lab.json"), |doc| {
            let rows: Vec<Value> = doc
                .get("results")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .map(|r| {
                            json!({
                                "id": r.get("id"), "label": r.get("label"), "family": r.get("family"),
                                "biased_universe": r.get("biased_universe"), "paper_target": r.get("paper_target"),
                                "full": r.get("full"), "oos": r.get("oos"), "trade": r.get("trade"),
                                "excess_cagr_vs_spy_pct": r.get("excess_cagr_vs_spy_pct"),
                                "cagr_pct_at_15bps": r.get("cagr_pct_at_15bps"),
                                "checks_passed": r.get("checks_passed"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({ "meta": doc.get("meta").cloned().unwrap_or(Value::Null), "results": rows })
        }),
        "promotion_gate": lab_artifact(&rules_dir.join("analysis").join("options-promotion-gate.json"), |doc| doc.clone()),
    })
}

fn lab_artifact(path: &Path, shape: impl Fn(&Value) -> Value) -> Value {
    let Ok(doc) = read_json(path) else {
        return json!({ "present": false });
    };
    let generated = doc
        .get("generated_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    let age_hours = generated
        .as_deref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|ts| (Utc::now() - ts.with_timezone(&Utc)).num_seconds() as f64 / 3600.0);
    json!({
        "present": true,
        "generated_at": generated,
        "age_hours": age_hours,
        "stale": age_hours.map(|h| h > LAB_STALE_HOURS).unwrap_or(true),
        "data": shape(&doc),
    })
}

/// `# HYPOTHESIS:` and `# FALSIFIER:` lines from the comment block at the top of an
/// arm's rules file. Continuation comment lines are joined. Both are `None` when absent.
fn arm_notes(rules_path: &Path) -> (Option<String>, Option<String>) {
    let Ok(raw) = std::fs::read_to_string(rules_path) else {
        return (None, None);
    };
    let mut hypothesis = String::new();
    let mut falsifier = String::new();
    let mut target = 0_u8; // 0 none, 1 hypothesis, 2 falsifier
    for line in raw.lines() {
        let Some(comment) = line.strip_prefix('#') else {
            if !line.trim().is_empty() && target != 0 {
                break;
            }
            continue;
        };
        let comment = comment.trim();
        if let Some(rest) = comment.strip_prefix("HYPOTHESIS:") {
            target = 1;
            hypothesis.push_str(rest.trim());
        } else if let Some(rest) = comment.strip_prefix("FALSIFIER:") {
            target = 2;
            falsifier.push_str(rest.trim());
        } else if comment.is_empty() {
            target = 0;
        } else if target == 1 {
            hypothesis.push(' ');
            hypothesis.push_str(comment);
        } else if target == 2 {
            falsifier.push(' ');
            falsifier.push_str(comment);
        }
    }
    let wrap = |s: String| if s.is_empty() { None } else { Some(s) };
    (wrap(hypothesis), wrap(falsifier))
}

fn swing_book(rules_path: &Path) -> Value {
    let yaml = read_yaml(rules_path);
    let name = yaml_str(&yaml, &["trader_id"]).unwrap_or_else(|| rules_path.display().to_string());
    let sleeve = yaml_f64(&yaml, &["capital", "fixed_sleeve_cap_usd"]);
    let state_file = state_path(rules_path);
    let state = match read_json(&state_file) {
        Ok(state) => state,
        Err(err) => {
            return book_error("swing", &name, rules_path, err.to_string());
        }
    };
    let state = inner_state(&state).clone();
    let mut book = book_from_equity_state("swing", &name, rules_path, &state, sleeve);
    book["plan"] = swing_plan(&yaml);
    book["shadows"] = json!(shadow_rows(rules_path, &yaml));
    book
}

fn swing_plan(yaml: &Value) -> Value {
    json!({
        "stop_loss_pct": yaml_f64(yaml, &["playbook", "exit", "stop_loss_pct"]),
        "profit_target_pct": yaml_f64(yaml, &["playbook", "exit", "profit_target_pct"]),
        "trail_activate_pct": yaml_f64(yaml, &["playbook", "exit", "trailing", "activate_after_profit_pct"]),
        "time_stop_days": yaml_f64(yaml, &["playbook", "exit", "time_stop_days"]),
        "max_hold_days": yaml_f64(yaml, &["playbook", "holding_period", "max_days"]),
        "target_hold_days": yaml_f64(yaml, &["playbook", "holding_period", "target_days"]),
    })
}

fn options_book(rules_path: &Path) -> Value {
    let yaml = read_yaml(rules_path);
    let name = yaml_str(&yaml, &["agent_id"]).unwrap_or_else(|| rules_path.display().to_string());
    let sleeve = yaml_f64(&yaml, &["simulation", "starting_budget_usd"])
        .or_else(|| yaml_f64(&yaml, &["risk", "max_portfolio_risk_usd"]));
    let state_file = options_state_path(rules_path);
    let state = match read_json(&state_file) {
        Ok(state) => state,
        Err(err) => {
            return book_error("options", &name, rules_path, err.to_string());
        }
    };
    let state = inner_state(&state).clone();
    let mut book = book_from_equity_state("options", &name, rules_path, &state, sleeve);
    let positions = options_positions(&state);
    let unrealized: f64 = positions
        .iter()
        .filter_map(|p| p.get("unrealized_usd").and_then(Value::as_f64))
        .sum();
    book["open_count"] = json!(positions.len());
    book["positions"] = json!(positions);
    book["unrealized_pnl_usd"] = json!(unrealized);
    book["entry_funnel"] = state.get("entry_funnel").cloned().unwrap_or(Value::Null);
    book["experiment"] = options_experiment(&yaml);
    let (hypothesis, falsifier) = arm_notes(rules_path);
    book["hypothesis"] = json!(hypothesis);
    book["falsifier"] = json!(falsifier);
    if book["cash_usd"].is_null() {
        if let Some(start) = state
            .pointer("/sim/starting_budget_usd")
            .and_then(Value::as_f64)
        {
            let realized = state
                .pointer("/sim/realized_pnl_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            book["equity_usd"] = json!(start + realized + unrealized);
            book["cash_usd"] = json!(start + realized);
            book["starting_cash_usd"] = json!(start);
        }
    } else if let Some(equity) = book["equity_usd"].as_f64() {
        book["equity_usd"] = json!(equity + unrealized);
    }
    book["journal"] = json!(journal_tail(&options_journal_path(rules_path)));
    book
}

fn book_from_equity_state(
    kind: &str,
    name: &str,
    rules_path: &Path,
    state: &Value,
    sleeve: Option<f64>,
) -> Value {
    let positions = swing_positions(state);
    let deployed: f64 = positions
        .iter()
        .filter_map(|p| p["market_value_usd"].as_f64())
        .sum();
    let closed = state
        .pointer("/sim/closed_trades")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (closed_count, wins, realized) = trade_stats(&closed);
    let cash = state.pointer("/sim/cash_usd").and_then(Value::as_f64);
    let start = state
        .pointer("/sim/starting_cash_usd")
        .and_then(Value::as_f64);
    let equity = cash.map(|c| c + deployed);
    let win_rate = if closed_count == 0 {
        0.0
    } else {
        (wins as f64) * 100.0 / (closed_count as f64)
    };
    let deployed_pct = sleeve
        .filter(|cap| *cap > 0.0)
        .map(|cap| (deployed / cap) * 100.0);
    let mut journal = if kind == "swing" {
        journal_tail(&journal_path(rules_path))
    } else {
        Vec::new()
    };
    if let Some(note) = tick_note(state) {
        journal.push(note);
        if journal.len() > JOURNAL_LIMIT {
            journal = journal.split_off(journal.len() - JOURNAL_LIMIT);
        }
    }
    json!({
        "kind": kind,
        "name": name,
        "rules_file": rules_path.display().to_string(),
        "mode": "paper",
        "sleeve_cap_usd": sleeve,
        "last_tick": state.get("last_tick").cloned().unwrap_or(Value::Null),
        "tick_count": state.get("tick_count").cloned().unwrap_or(Value::Null),
        "session": state.get("last_session").cloned().unwrap_or(Value::Null),
        "profile": state.get("active_profile").cloned().unwrap_or(Value::Null),
        "halted": state.get("trading_halted_reason").cloned().unwrap_or(Value::Null),
        "cash_usd": cash,
        "starting_cash_usd": start,
        "equity_usd": equity,
        "deployed_pct": deployed_pct,
        "open_count": positions.len(),
        "closed_count": closed_count,
        "wins": wins,
        "win_rate_pct": win_rate,
        "realized_pnl_usd": realized,
        "positions": positions,
        "recent_closes": recent_closes(&closed),
        "closed_bet_keys": closed.iter().map(|trade| json!(close_bet_key(name, trade))).collect::<Vec<_>>(),
        "profile_pnl": profile_pnl(&closed),
        "pnl_series": pnl_series(&closed),
        "journal": journal,
    })
}

fn profile_pnl(closed: &[Value]) -> Vec<Value> {
    let mut groups: std::collections::BTreeMap<String, (u64, u64, f64)> =
        std::collections::BTreeMap::new();
    for trade in closed {
        let Some(profile) = trade.get("active_profile").and_then(Value::as_str) else {
            continue;
        };
        if profile.is_empty() {
            continue;
        }
        let pnl = trade.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0);
        let entry = groups.entry(profile.to_string()).or_insert((0, 0, 0.0));
        entry.0 += 1;
        if pnl > 0.0 {
            entry.1 += 1;
        }
        entry.2 += pnl;
    }
    groups
        .into_iter()
        .map(|(profile, (n, wins, net))| {
            json!({
                "profile": profile,
                "n": n,
                "wins": wins,
                "win_rate_pct": if n == 0 { 0.0 } else { wins as f64 * 100.0 / n as f64 },
                "net_usd": net,
            })
        })
        .collect()
}

fn book_error(kind: &str, name: &str, rules_path: &Path, error: String) -> Value {
    json!({
        "kind": kind,
        "name": name,
        "rules_file": rules_path.display().to_string(),
        "mode": "paper",
        "error": error,
    })
}

fn swing_positions(state: &Value) -> Vec<Value> {
    let Some(map) = state.get("open_positions").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rows: Vec<Value> = map
        .values()
        .map(|p| {
            let qty = p.get("quantity").and_then(Value::as_f64).unwrap_or(0.0);
            let entry = p.get("entry_price").and_then(Value::as_f64).unwrap_or(0.0);
            let value = p.get("market_value_usd").and_then(Value::as_f64);
            let last = value.filter(|_| qty > 0.0).map(|v| v / qty);
            let unrealized_pct = last
                .filter(|_| entry > 0.0)
                .map(|last| (last - entry) / entry * 100.0);
            let unrealized_usd = value.map(|v| v - entry * qty);
            json!({
                "symbol": p.get("symbol").cloned().unwrap_or(Value::Null),
                "quantity": qty,
                "entry_price": p.get("entry_price").cloned().unwrap_or(Value::Null),
                "stop_price": p.get("stop_price").cloned().unwrap_or(Value::Null),
                "profit_limit": p.get("profit_limit").cloned().unwrap_or(Value::Null),
                "market_value_usd": value,
                "last_price": last,
                "unrealized_usd": unrealized_usd,
                "unrealized_pct": unrealized_pct,
                "opened_at": p.get("opened_at").cloned().unwrap_or(Value::Null),
                "peak_profit_pct": p.get("peak_profit_pct").cloned().unwrap_or(Value::Null),
                "trough_profit_pct": p.get("trough_profit_pct").cloned().unwrap_or(Value::Null),
                "stop_risk_usd": p.get("stop_risk_usd").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("symbol")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("symbol").and_then(Value::as_str).unwrap_or(""))
    });
    rows
}

fn options_positions(state: &Value) -> Vec<Value> {
    let Some(map) = state.get("open_positions").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rows: Vec<Value> = map
        .values()
        .map(|p| {
            let params = p.get("entry_params");
            let credit = p.get("entry_credit").and_then(Value::as_f64);
            let debit = p.get("last_good_debit_to_close").and_then(Value::as_f64);
            let contracts = p
                .get("contracts")
                .and_then(Value::as_f64)
                .unwrap_or(1.0)
                .max(0.0);
            let unrealized = credit.zip(debit).map(|(c, d)| (c - d) * 100.0 * contracts);
            let unrealized_pct = credit
                .zip(debit)
                .filter(|(c, _)| *c > 0.0)
                .map(|(c, d)| (c - d) / c * 100.0);
            let short = params
                .and_then(|v| v.get("short_strike"))
                .and_then(Value::as_f64);
            let long = params
                .and_then(|v| v.get("long_strike"))
                .and_then(Value::as_f64);
            let width = short.zip(long).map(|(s, l)| (s - l).abs());
            let credit_to_width_pct = credit
                .zip(width)
                .filter(|(_, w)| *w > 0.0)
                .map(|(c, w)| c / w * 100.0);
            json!({
                "underlying": p.get("underlying").cloned().unwrap_or(Value::Null),
                "strategy": p.get("strategy").cloned().unwrap_or(Value::Null),
                "spread_type": params.and_then(|v| v.get("type")).cloned().unwrap_or(Value::Null),
                "expiry": p.get("expiry").cloned().unwrap_or(Value::Null),
                "opened_at": p.get("opened_at").cloned().unwrap_or(Value::Null),
                "contracts": p.get("contracts").cloned().unwrap_or(Value::Null),
                "short_strike": short,
                "long_strike": long,
                "width": width,
                "entry_credit": credit,
                "debit_to_close": debit,
                "credit_to_width_pct": credit_to_width_pct,
                "unrealized_usd": unrealized,
                "unrealized_pct": unrealized_pct,
                "entry_pop_pct": p.get("entry_pop_pct").cloned().unwrap_or(Value::Null),
                "entry_short_delta": p.get("entry_short_delta").cloned().unwrap_or(Value::Null),
                "max_loss_usd": p.get("max_loss_usd").cloned().unwrap_or(Value::Null),
                "last_good_streak": p.get("last_good_streak").cloned().unwrap_or(json!(0)),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("underlying")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("underlying").and_then(Value::as_str).unwrap_or(""))
    });
    rows
}

fn shadow_rows(rules_path: &Path, yaml: &Value) -> Vec<Value> {
    let trader_id = yaml_str(yaml, &["trader_id"]).unwrap_or_default();
    let Some(arms) = yaml.pointer("/shadow/arms").and_then(Value::as_array) else {
        return Vec::new();
    };
    arms.iter()
        .filter_map(|arm| arm.get("id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(|id| {
            let path = shadow_state_path(rules_path, &trader_id, id);
            match read_json(&path) {
                Ok(state) => shadow_summary(id, &state),
                Err(_) => json!({
                    "id": id,
                    "starting_cash_usd": Value::Null,
                    "equity_usd": Value::Null,
                    "open_count": 0,
                    "closed_count": 0,
                    "win_rate_pct": 0.0,
                    "realized_pnl_usd": 0.0,
                    "last_tick": Value::Null,
                }),
            }
        })
        .collect()
}

fn inner_state(state: &Value) -> &Value {
    match state.get("trader") {
        Some(trader) if trader.is_object() => trader,
        _ => state,
    }
}

fn tick_note(state: &Value) -> Option<Value> {
    let tick = state.get("last_tick_result")?;
    let session = tick.get("session").and_then(Value::as_str).unwrap_or("");
    let candidates = tick
        .pointer("/scan/candidate_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let rejected = tick
        .pointer("/scan/rejected")
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    let block = tick
        .get("entry_block_reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    let detail = format!("{session} candidates {candidates}, rejected {rejected} {block}")
        .trim()
        .to_string();
    Some(json!({
        "ts": state.get("last_tick").cloned().unwrap_or(Value::Null),
        "type": "tick",
        "detail": detail,
    }))
}

fn shadow_summary(id: &str, state: &Value) -> Value {
    let state = inner_state(state);
    let positions = swing_positions(state);
    let deployed: f64 = positions
        .iter()
        .filter_map(|p| p["market_value_usd"].as_f64())
        .sum();
    let closed = state
        .pointer("/sim/closed_trades")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (closed_count, wins, realized) = trade_stats(&closed);
    let cash = state.pointer("/sim/cash_usd").and_then(Value::as_f64);
    let start = state
        .pointer("/sim/starting_cash_usd")
        .and_then(Value::as_f64);
    let win_rate = if closed_count == 0 {
        0.0
    } else {
        (wins as f64) * 100.0 / (closed_count as f64)
    };
    json!({
        "id": id,
        "starting_cash_usd": start,
        "equity_usd": cash.map(|c| c + deployed),
        "open_count": positions.len(),
        "closed_count": closed_count,
        "win_rate_pct": win_rate,
        "realized_pnl_usd": realized,
        "worst_close": worst_close(&closed),
        "last_tick": state.get("last_tick").cloned().unwrap_or(Value::Null),
    })
}

fn worst_close(closed: &[Value]) -> Value {
    let worst = closed.iter().min_by(|a, b| {
        let left = a.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0);
        let right = b.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0);
        left.partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let Some(trade) = worst else {
        return Value::Null;
    };
    let qty = trade.get("quantity").and_then(Value::as_f64).unwrap_or(0.0);
    let entry = trade
        .get("entry_price")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    json!({
        "symbol": trade.get("symbol").cloned().unwrap_or(Value::Null),
        "pnl_usd": trade.get("pnl_usd").cloned().unwrap_or(Value::Null),
        "pnl_pct": trade.get("pnl_pct").cloned().unwrap_or(Value::Null),
        "exit_reason": trade.get("exit_reason").cloned().unwrap_or(Value::Null),
        "notional_usd": if qty > 0.0 && entry > 0.0 { json!(qty * entry) } else { Value::Null },
    })
}

fn trade_stats(closed: &[Value]) -> (usize, usize, f64) {
    let mut wins = 0;
    let mut realized = 0.0;
    for trade in closed {
        let pnl = trade.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0);
        realized += pnl;
        if pnl > 0.0 {
            wins += 1;
        }
    }
    (closed.len(), wins, realized)
}

fn pnl_series(closed: &[Value]) -> Vec<Value> {
    let mut rows: Vec<&Value> = closed.iter().collect();
    rows.sort_by(|a, b| {
        a.get("closed_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("closed_at").and_then(Value::as_str).unwrap_or(""))
    });
    let mut cumulative = 0.0;
    rows.into_iter()
        .map(|t| {
            let pnl = t.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0);
            cumulative += pnl;
            json!({
                "at": t.get("closed_at").cloned().unwrap_or(Value::Null),
                "pnl_usd": pnl,
                "realized_usd": cumulative,
            })
        })
        .collect()
}

fn options_experiment(yaml: &Value) -> Value {
    let widths = pointer(yaml, &["entry_rules", "vertical", "widths"])
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|n| n.as_f64().or_else(|| n.as_i64().map(|i| i as f64)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "profit_target_pct": yaml_f64(yaml, &["exit_rules", "profit_target_pct"]),
        "stop_loss_pct": yaml_f64(yaml, &["exit_rules", "stop_loss_pct"]),
        "dte_close": yaml_f64(yaml, &["exit_rules", "dte_close"]),
        "min_credit": yaml_f64(yaml, &["entry_rules", "vertical", "min_credit"]),
        "min_credit_to_width_pct": yaml_f64(yaml, &["entry_rules", "vertical", "min_credit_to_width_pct"]),
        "min_distance_to_be_pct": yaml_f64(yaml, &["entry_rules", "vertical", "min_distance_to_be_pct"]),
        "min_short_otm_pct": yaml_f64(yaml, &["entry_rules", "vertical", "min_short_otm_pct"]),
        "short_delta_min": yaml_f64(yaml, &["entry_rules", "vertical", "short_delta_min"]),
        "short_delta_max": yaml_f64(yaml, &["entry_rules", "vertical", "short_delta_max"]),
        "max_width": yaml_f64(yaml, &["entry_rules", "vertical", "max_width"]),
        "widths": widths,
        "dte_min": yaml_f64(yaml, &["entry_rules", "vertical", "dte_min"]),
        "dte_max": yaml_f64(yaml, &["entry_rules", "vertical", "dte_max"]),
        "max_open_positions": yaml_f64(yaml, &["entry_rules", "vertical", "max_open_positions"]),
        "max_trades_per_day": yaml_f64(yaml, &["risk", "max_trades_per_day"]),
    })
}

fn recent_closes(closed: &[Value]) -> Vec<Value> {
    let mut rows: Vec<&Value> = closed.iter().collect();
    rows.sort_by(|a, b| {
        a.get("closed_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("closed_at").and_then(Value::as_str).unwrap_or(""))
    });
    rows.into_iter()
        .rev()
        .take(CLOSE_LIMIT)
        .map(|t| {
            json!({
                "closed_at": t.get("closed_at").cloned().unwrap_or(Value::Null),
                "symbol": t.get("symbol").cloned().or_else(|| t.get("underlying").cloned()).unwrap_or(Value::Null),
                "underlying": t.get("underlying").cloned().unwrap_or(Value::Null),
                "pnl_usd": t.get("pnl_usd").cloned().unwrap_or(Value::Null),
                "pnl_pct": t.get("pnl_pct").cloned().unwrap_or(Value::Null),
                "exit_reason": t.get("exit_reason").cloned().unwrap_or(Value::Null),
                "hold_days": t.get("hold_days").cloned().unwrap_or(Value::Null),
                "mark_suspect": t.get("mark_suspect").cloned().unwrap_or(json!(false)),
            })
        })
        .collect()
}

fn journal_tail(path: &Path) -> Vec<Value> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    let mut end = len;
    let search_floor = len.saturating_sub(JOURNAL_SEARCH_BYTES);
    while end > search_floor && events.len() < JOURNAL_LIMIT {
        let start = end.saturating_sub(JOURNAL_CHUNK_BYTES).max(search_floor);
        let Ok(text) = read_range(&mut file, start, end) else {
            break;
        };
        end = start;
        let lines: Vec<&str> = text.lines().collect();
        for line in lines.iter().rev() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
            if kind.is_empty() || kind.contains("tick_summary") || kind.contains("scan") {
                continue;
            }
            events.push(json!({
                "ts": event.get("ts").cloned().unwrap_or(Value::Null),
                "type": kind,
                "detail": event_detail(&event),
            }));
            if events.len() == JOURNAL_LIMIT {
                break;
            }
        }
        if start == 0 {
            break;
        }
    }
    events.reverse();
    events
}

fn read_range(file: &mut File, start: u64, end: u64) -> Result<String> {
    file.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0_u8; (end - start) as usize];
    file.read_exact(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        if let Some(idx) = text.find('\n') {
            text.drain(..=idx);
        }
    }
    Ok(text)
}

fn event_detail(event: &Value) -> String {
    let payload = event.get("payload").cloned().unwrap_or(Value::Null);
    let symbol = payload
        .get("symbol")
        .or_else(|| payload.get("underlying"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let reason = payload
        .get("exit_reason")
        .or_else(|| payload.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let pnl = payload.get("pnl_usd").and_then(Value::as_f64);
    let mut parts = Vec::new();
    if !symbol.is_empty() {
        parts.push(symbol.to_string());
    }
    if let Some(pnl) = pnl {
        parts.push(format!("{pnl:+.2}"));
    }
    if !reason.is_empty() {
        parts.push(reason.to_string());
    }
    parts.join(" ")
}

fn options_state_path(rules_path: &Path) -> PathBuf {
    let sim = options_runtime_path(rules_path, "agent-sim-state");
    if sim.is_file() {
        sim
    } else {
        options_runtime_path(rules_path, "agent-state")
    }
}

fn options_runtime_path(rules_path: &Path, prefix: &str) -> PathBuf {
    let stem = rules_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("agent");
    rules_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{prefix}-{stem}.json"))
}

fn options_journal_path(rules_path: &Path) -> PathBuf {
    let stem = rules_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("agent");
    rules_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("agent-sim-journal-{stem}.jsonl"))
}

fn read_yaml(path: &Path) -> Value {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Value::Null;
    };
    serde_yaml::from_str(&raw).unwrap_or(Value::Null)
}

fn read_json(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn yaml_str(yaml: &Value, path: &[&str]) -> Option<String> {
    pointer(yaml, path)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn yaml_f64(yaml: &Value, path: &[&str]) -> Option<f64> {
    let value = pointer(yaml, path)?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
}

fn pointer<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    Some(cur)
}

fn handle(mut stream: TcpStream, paths: &DashboardPaths) -> Result<()> {
    // Read the whole header block before answering. A browser request is often
    // larger than one 1KB read; closing with unread bytes makes the kernel RST
    // the connection, which shows up as ERR_CONNECTION_RESET.
    let headers = read_headers(&mut stream)?;
    let path = request_path(&headers);
    let (status, content_type, body) = match path {
        "/" => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        "/api/status" => ("200 OK", "application/json", snapshot(paths).to_string()),
        "/favicon.ico" => ("204 No Content", "text/plain", String::new()),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes())?;
    }
    let _ = stream.shutdown(Shutdown::Write);
    Ok(())
}

fn read_headers(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 2048];
    while buf.windows(4).all(|w| w != b"\r\n\r\n") && buf.len() < 64 * 1024 {
        let n = stream.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn request_path(headers: &str) -> &str {
    headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_summarizes_swing_book_and_skips_tick_noise() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("trader-demo.yaml");
        std::fs::write(
            &rules,
            "trader_id: swing-demo\ncapital:\n  fixed_sleeve_cap_usd: 4000\nshadow:\n  enabled: true\n  arms:\n    - id: big-base\n",
        )
        .unwrap();
        let state = json!({
            "trader_id": "swing-demo",
            "last_tick": "2026-10-08T18:00:00Z",
            "tick_count": 12,
            "last_session": "regular",
            "active_profile": "low_vol_trend",
            "open_positions": {
                "a": {
                    "symbol": "AAPL",
                    "quantity": 10.0,
                    "entry_price": 100.0,
                    "stop_price": 95.0,
                    "profit_limit": 110.0,
                    "market_value_usd": 1050.0
                }
            },
            "sim": {
                "starting_cash_usd": 4000.0,
                "cash_usd": 2900.0,
                "closed_trades": [
                    {"symbol": "MSFT", "pnl_usd": 40.0, "pnl_pct": 2.0, "exit_reason": "target", "hold_days": 3, "closed_at": "2026-10-07T20:00:00Z"},
                    {"symbol": "NVDA", "pnl_usd": -10.0, "pnl_pct": -1.0, "exit_reason": "stop", "hold_days": 2, "closed_at": "2026-10-08T19:00:00Z"}
                ]
            }
        });
        std::fs::write(state_path(&rules), serde_json::to_string(&state).unwrap()).unwrap();
        let shadow = json!({
            "arm_id": "big-base",
            "trader": {
                "last_tick": "2026-10-08T18:00:01Z",
                "open_positions": {},
                "sim": {"starting_cash_usd": 30000.0, "cash_usd": 30000.0, "closed_trades": []}
            }
        });
        std::fs::write(
            shadow_state_path(&rules, "swing-demo", "big-base"),
            serde_json::to_string(&shadow).unwrap(),
        )
        .unwrap();
        let journal = dir.path().join("trader-journal-trader-demo.jsonl");
        std::fs::write(
            &journal,
            "{\"ts\":\"2026-10-08T18:00:00Z\",\"type\":\"sim_tick_summary\",\"payload\":{}}\n{\"ts\":\"2026-10-08T18:01:00Z\",\"type\":\"sim_entry_filled\",\"payload\":{\"symbol\":\"AAPL\",\"pnl_usd\":1.5}}\n",
        )
        .unwrap();

        let snap = snapshot(&DashboardPaths {
            swing: vec![rules],
            options: vec![],
            families: None,
        });
        let book = &snap["books"][0];
        assert_eq!(book["name"], "swing-demo");
        assert_eq!(book["equity_usd"], 3950.0);
        assert_eq!(book["open_count"], 1);
        assert_eq!(book["closed_count"], 2);
        assert_eq!(book["wins"], 1);
        assert_eq!(book["realized_pnl_usd"], 30.0);
        assert_eq!(book["recent_closes"][0]["symbol"], "NVDA");
        assert_eq!(book["positions"][0]["unrealized_pct"], 5.0);
        assert_eq!(book["positions"][0]["last_price"], 105.0);
        assert!(book["plan"]["stop_loss_pct"].is_null());
        assert_eq!(book["journal"].as_array().unwrap().len(), 1);
        assert_eq!(book["journal"][0]["type"], "sim_entry_filled");
        assert_eq!(book["shadows"][0]["id"], "big-base");
        assert_eq!(book["shadows"][0]["starting_cash_usd"], 30000.0);
        assert!(book.get("error").is_none());
    }

    #[test]
    fn arm_notes_join_continuation_lines_and_stop_at_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("arm.yaml");
        std::fs::write(
            &rules,
            "version: 1\n# Paper arm\n# HYPOTHESIS: longer trades keep\n# more edge after costs.\n# FALSIFIER: no better than O0.\nagent_id: x\n# HYPOTHESIS: ignored because yaml already started\n",
        )
        .unwrap();
        let (h, f) = arm_notes(&rules);
        assert_eq!(
            h.as_deref(),
            Some("longer trades keep more edge after costs.")
        );
        assert_eq!(f.as_deref(), Some("no better than O0."));
        assert_eq!(arm_notes(&dir.path().join("missing.yaml")), (None, None));
    }

    #[test]
    fn lab_section_reports_missing_files_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("options-a.yaml");
        std::fs::write(&rules, "agent_id: a\n").unwrap();
        let lab = lab_section(&DashboardPaths {
            swing: vec![],
            options: vec![rules.clone()],
            families: None,
        });
        for key in ["scorecard", "benchmarks", "slippage", "promotion_gate"] {
            assert_eq!(lab[key]["present"], false, "{key}");
        }
        assert!(snapshot(&DashboardPaths {
            swing: vec![],
            options: vec![rules],
            families: None
        })["underlyings"]
            .as_object()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn underlying_prices_come_from_the_chain_summary() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("options-a.yaml");
        std::fs::write(&rules, "agent_id: a\n").unwrap();
        std::fs::create_dir_all(dir.path().join("chains")).unwrap();
        std::fs::write(
            dir.path().join("chains/latest-underlyings.json"),
            r#"{"QQQ":{"price":743.3,"as_of":"2026-10-13 10:00 ET"}}"#,
        )
        .unwrap();
        let snap = snapshot(&DashboardPaths {
            swing: vec![],
            options: vec![rules],
            families: None,
        });
        assert_eq!(snap["underlyings"]["QQQ"]["price"], 743.3);
        assert_eq!(snap["underlyings"]["QQQ"]["as_of"], "2026-10-13 10:00 ET");
    }

    #[test]
    fn lab_section_reads_artifacts_and_flags_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("options-a.yaml");
        std::fs::write(&rules, "agent_id: a\n").unwrap();
        std::fs::create_dir_all(dir.path().join("scorecards")).unwrap();
        std::fs::create_dir_all(dir.path().join("benchmarks")).unwrap();
        let fresh = Utc::now().to_rfc3339();
        std::fs::write(
            dir.path().join("scorecards/latest.json"),
            json!({"date": "2026-10-12", "generated_at": fresh, "options_arms": {"nominal_entries": 11, "effective_entries": 5}, "drift_alerts": ["x"], "swing": {"big": 1}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("benchmarks/latest.json"),
            json!({"generated_at": "2020-01-01T00:00:00+00:00", "agents": {"a": {"total_usd": 1.0, "trades": [1, 2, 3]}}}).to_string(),
        )
        .unwrap();
        let lab = lab_section(&DashboardPaths {
            swing: vec![],
            options: vec![rules],
            families: None,
        });
        assert_eq!(lab["scorecard"]["present"], true);
        assert_eq!(lab["scorecard"]["stale"], false);
        assert_eq!(
            lab["scorecard"]["data"]["options_arms"]["effective_entries"],
            5
        );
        assert!(lab["scorecard"]["data"].get("swing").is_none());
        assert_eq!(lab["benchmarks"]["stale"], true);
        assert!(lab["benchmarks"]["data"]["agents"]["a"]
            .get("trades")
            .is_none());
        assert_eq!(lab["benchmarks"]["data"]["agents"]["a"]["total_usd"], 1.0);
    }

    #[test]
    fn request_path_ignores_the_query_string() {
        let headers = "GET /api/status?x=1 HTTP/1.1\r\nHost: jarvis\r\n\r\n";
        assert_eq!(request_path(headers), "/api/status");
        assert_eq!(
            request_path("GET /favicon.ico HTTP/1.1\r\n\r\n"),
            "/favicon.ico"
        );
    }

    #[test]
    fn options_book_reads_the_paper_sim_state() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("options-pilot.yaml");
        std::fs::write(
            &rules,
            "agent_id: options-pilot\nsimulation:\n  starting_budget_usd: 4000\nexit_rules:\n  profit_target_pct: 60\n  stop_loss_pct: 200\n  dte_close: 21\nentry_rules:\n  vertical:\n    min_credit: 0.50\n    short_delta_min: 0.12\n    short_delta_max: 0.20\n    max_open_positions: 1\nrisk:\n  max_trades_per_day: 1\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("agent-state-options-pilot.json"),
            r#"{"agent_id":"options-pilot","last_tick":"2026-08-05T15:00:00Z","open_positions":{},"tick_count":1}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("agent-sim-state-options-pilot.json"),
            r#"{"agent_id":"options-pilot","last_tick":"2026-10-08T18:00:00Z","tick_count":9,"entry_funnel":{"day":"2026-10-08","candidates_seen":4,"admitted":0,"rejects":{"distance_to_be":4}},"open_positions":{"p":{"underlying":"SPY","strategy":"iron_condor","expiry":"2026-10-16","contracts":1,"entry_credit":1.5,"last_good_debit_to_close":1.0,"max_loss_usd":380,"entry_params":{"short_strike":440,"long_strike":435,"type":"put_credit"},"entry_short_delta":0.16,"entry_pop_pct":80}},"sim":{"starting_budget_usd":4000,"realized_pnl_usd":-71.6,"closed_trades":[{"underlying":"QQQ","pnl_usd":-71.6,"pnl_pct":-8.0,"exit_reason":"stop","hold_days":4,"closed_at":"2026-10-01T20:00:00Z"}]}}"#,
        )
        .unwrap();
        let snap = snapshot(&DashboardPaths {
            swing: vec![],
            options: vec![rules],
            families: None,
        });
        let book = &snap["books"][0];
        assert_eq!(book["name"], "options-pilot");
        assert_eq!(book["tick_count"], 9);
        assert_eq!(book["open_count"], 1);
        assert_eq!(book["positions"][0]["underlying"], "SPY");
        assert_eq!(book["closed_count"], 1);
        assert_eq!(book["realized_pnl_usd"], -71.6);
        assert_eq!(book["unrealized_pnl_usd"], 50.0);
        assert_eq!(book["equity_usd"], 3978.4);
        assert_eq!(book["positions"][0]["width"], 5.0);
        assert_eq!(book["entry_funnel"]["rejects"]["distance_to_be"], 4);
        assert_eq!(book["experiment"]["profit_target_pct"], 60.0);
        assert_eq!(book["experiment"]["stop_loss_pct"], 200.0);
        assert_eq!(book["experiment"]["short_delta_max"], 0.20);
        assert_eq!(book["pnl_series"][0]["realized_usd"], -71.6);
        assert!(book["positions"][0].get("position_id").is_none());
    }

    #[test]
    fn snapshot_includes_a_workflow_paper_book() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("options-a.yaml");
        std::fs::write(&rules, "agent_id: options-a\n").unwrap();
        std::fs::write(
            dir.path().join("agent-trader-workflow-msft-dip.json"),
            r#"{"stack":"agent-trader","broker":"public","kind":"workflow","name":"msft-dip","live_orders":false,"equity_usd":4000,"cash_usd":3750,"workflows":[{"id":"msft-dip","status":"simulated","steps":[{"id":"buy","status":"simulated"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("agent-trader-workflow-noise.json"),
            r#"{"stack":"agent-trader","broker":"public","kind":"crypto","name":"nope"}"#,
        )
        .unwrap();
        let snap = snapshot(&DashboardPaths {
            swing: vec![],
            options: vec![rules],
            families: None,
        });
        let books = snap["books"].as_array().unwrap();
        assert!(books
            .iter()
            .any(|book| book["kind"] == "workflow" && book["name"] == "msft-dip"));
        assert!(books.iter().all(|book| book["kind"] != "crypto"));
    }

    #[test]
    fn families_section_stacks_clone_loss_and_dedups_open_bets() {
        let dir = tempfile::tempdir().unwrap();
        for (arm, realized, suspect) in [
            ("options-arm-o0", -300.0, true),
            ("options-arm-o1", -300.0, false),
        ] {
            let rules = dir.path().join(format!("{arm}.yaml"));
            std::fs::write(&rules, format!("agent_id: {arm}\n")).unwrap();
            // identical open spread (same underlying/expiry/strikes) in both clones
            let state = format!(
                r#"{{"sim":{{"starting_budget_usd":30000,"realized_pnl_usd":{realized},"closed_trades":[{{"pnl_usd":{realized},"mark_suspect":{suspect}}}]}},"open_positions":{{"p0":{{"underlying":"QQQ","expiry":"2026-11-20","spread_type":"put_credit","short_strike":745,"long_strike":740,"contracts":1,"entry_credit":1.5,"last_good_debit_to_close":1.0}}}}}}"#
            );
            std::fs::write(
                dir.path().join(format!("agent-sim-state-{arm}.json")),
                state,
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("swing.yaml"), "trader_id: swing-9947\n").unwrap();
        std::fs::write(
            dir.path().join("trader-state-swing.json"),
            r#"{"active_profile":"elevated_vol","open_positions":{},"sim":{"starting_cash_usd":4000,"cash_usd":4000,"closed_trades":[{"symbol":"AAA","pnl_usd":73.99,"active_profile":"low_vol_trend"},{"symbol":"BBB","pnl_usd":-50.0,"active_profile":"elevated_vol"},{"symbol":"CCC","pnl_usd":-10.0,"active_profile":"elevated_vol"}]}}"#,
        )
        .unwrap();
        let fams = json!({"families": [{
            "id": "dup", "label": "Dup clones", "stack": true,
            "sleeve_usd": 60000,
            "members": [
                {"id": "options-arm-o0", "role": "control"},
                {"id": "options-arm-o1", "role": "entry_clone"}
            ],
        }]});
        std::fs::write(dir.path().join("families.json"), fams.to_string()).unwrap();
        let paths = DashboardPaths {
            swing: vec![dir.path().join("swing.yaml")],
            options: vec![
                dir.path().join("options-arm-o0.yaml"),
                dir.path().join("options-arm-o1.yaml"),
            ],
            families: Some(dir.path().join("families.json")),
        };
        let snap = snapshot(&paths);
        let fam = &snap["families"]["families"][0];
        // member losses stack onto the family sleeve: -600 realized, not -300
        assert_eq!(fam["stacked_realized_usd"], -600.0);
        // the identical open spread is one mark ($50), not two ($100)
        assert_eq!(fam["stacked_open_mark_usd"], 50.0);
        assert_eq!(fam["stacked_net_usd"], -550.0);
        assert_eq!(fam["sleeve_usd"], 60000.0);
        assert_eq!(fam["present_members"].as_array().unwrap().len(), 2);
        // two identical opens count once; two closes without a shared spread identity stay distinct
        assert_eq!(fam["nominal_entries"], 4);
        assert_eq!(fam["effective_entries"], 3);
        assert!((fam["replication_count"].as_f64().unwrap() - 4.0 / 3.0).abs() < 1e-9);
        assert_eq!(fam["mark_suspect"], true);
        assert_eq!(snap["families"]["roles"]["options-arm-o0"], "control");
        assert_eq!(snap["families"]["roles"]["options-arm-o1"], "entry_clone");
        let books = snap["books"].as_array().unwrap();
        let o0 = books
            .iter()
            .find(|book| book["name"] == "options-arm-o0")
            .unwrap();
        let o1 = books
            .iter()
            .find(|book| book["name"] == "options-arm-o1")
            .unwrap();
        assert_eq!(o0["role"], "control");
        assert_eq!(o1["role"], "entry_clone");
        assert_eq!(o0["recent_closes"][0]["mark_suspect"], true);
        let swing = books
            .iter()
            .find(|book| book["name"] == "swing-9947")
            .unwrap();
        let profiles = swing["profile_pnl"].as_array().unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0]["profile"], "elevated_vol");
        assert_eq!(profiles[0]["n"], 2);
        assert_eq!(profiles[0]["net_usd"], -60.0);
        assert_eq!(profiles[1]["profile"], "low_vol_trend");
        assert_eq!(profiles[1]["n"], 1);
        assert_eq!(profiles[1]["net_usd"], 73.99);
    }

    #[test]
    fn research_section_reports_missing_dir_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let section = research_section(&dir.path().join("missing"));
        assert_eq!(section["present"], false);
        assert!(section["arms"].as_array().unwrap().is_empty());
        assert!(section["history"].as_array().unwrap().is_empty());
    }

    #[test]
    fn research_section_summarizes_latest_tick_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal-research-megacaps.jsonl");
        let rec = |at: &str, regime: &str| {
            json!({
                "kind": "research_tick", "phase": 1, "arm_id": "research-megacaps",
                "at": at, "account": "public:ACC", "quote_count": 5,
                "orders_placed": 0, "place_attempts": 0,
                "citations": [{"url": "https://x.test/a", "title": "A"}],
                "gate": {"ok": true, "checks": [{"name": "mode_paper", "passed": true}]},
                "forecast": {
                    "target": "forward_5d_excess_return_sign", "benchmark": "SPY",
                    "horizon_days": 5, "regime": regime,
                    "signals": [{"symbol": "MSFT", "direction": "up", "confidence": 0.6,
                                 "expected_return_pct": 0.8, "rank": 1, "citations": []}],
                    "risk_alerts": [], "plan_patch": [], "notes": "n"
                },
                "forecast_note": ""
            })
        };
        std::fs::write(
            &journal,
            format!(
                "{}\n{}\nnot json\n",
                rec("2026-10-29T20:00:00Z", "chop"),
                rec("2026-10-29T21:00:00Z", "risk_on")
            ),
        )
        .unwrap();
        let section = research_section(dir.path());
        assert_eq!(section["present"], true);
        assert_eq!(section["arms"].as_array().unwrap().len(), 1);
        let arm = &section["arms"][0];
        assert_eq!(arm["arm_id"], "research-megacaps");
        assert_eq!(arm["gate_ok"], true);
        assert_eq!(arm["orders_placed"], 0);
        assert_eq!(arm["place_attempts"], 0);
        assert_eq!(arm["forecast"]["regime"], "risk_on");
        assert_eq!(arm["forecast"]["signals"].as_array().unwrap().len(), 1);
        assert_eq!(section["history"].as_array().unwrap().len(), 2);
        assert_eq!(section["history"][1]["regime"], "risk_on");
    }

    #[test]
    fn research_section_surfaces_gate_failure() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal-arm.jsonl");
        let record = json!({
            "kind": "research_tick", "arm_id": "arm", "at": "2026-10-29T21:00:00Z",
            "orders_placed": 0, "place_attempts": 0,
            "gate": {"ok": false, "checks": [
                {"name": "no_schwab_backend", "passed": true},
                {"name": "mode_paper", "passed": false}
            ]},
            "forecast": Value::Null
        });
        std::fs::write(&journal, format!("{record}\n")).unwrap();
        let section = research_section(dir.path());
        let arm = &section["arms"][0];
        assert_eq!(arm["gate_ok"], false);
        assert_eq!(arm["gate_failures"][0], "mode_paper");
    }
}
