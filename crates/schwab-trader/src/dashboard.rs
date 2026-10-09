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
use chrono::Utc;
use serde_json::{json, Value};

use crate::agent::paths::{journal_path, shadow_state_path, state_path};

const PAGE: &str = include_str!("dashboard.html");
const JOURNAL_CHUNK_BYTES: u64 = 256 * 1024;
const JOURNAL_SEARCH_BYTES: u64 = 8 * 1024 * 1024;
const JOURNAL_LIMIT: usize = 40;
const CLOSE_LIMIT: usize = 12;

pub struct DashboardPaths {
    pub swing: Vec<PathBuf>,
    pub options: Vec<PathBuf>,
}

pub fn serve(binds: &[String], paths: DashboardPaths) -> Result<()> {
    anyhow::ensure!(!binds.is_empty(), "dashboard needs at least one --bind address");
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
    json!({
        "generated_at": Utc::now().to_rfc3339(),
        "books": books,
    })
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
    book["shadows"] = json!(shadow_rows(rules_path, &yaml));
    book
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
    book["open_count"] = json!(positions.len());
    book["positions"] = json!(positions);
    if book["cash_usd"].is_null() {
        if let Some(start) = state
            .pointer("/sim/starting_budget_usd")
            .and_then(Value::as_f64)
        {
            let realized = state
                .pointer("/sim/realized_pnl_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            book["equity_usd"] = json!(start + realized);
            book["cash_usd"] = json!(start + realized);
            book["starting_cash_usd"] = json!(start);
        }
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
    let deployed: f64 = positions.iter().filter_map(|p| p["market_value_usd"].as_f64()).sum();
    let closed = state
        .pointer("/sim/closed_trades")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (closed_count, wins, realized) = trade_stats(&closed);
    let cash = state.pointer("/sim/cash_usd").and_then(Value::as_f64);
    let start = state.pointer("/sim/starting_cash_usd").and_then(Value::as_f64);
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
        "journal": journal,
    })
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
            let unrealized_pct = match (value, qty > 0.0, entry > 0.0) {
                (Some(value), true, true) => Some(((value / qty) - entry) / entry * 100.0),
                _ => None,
            };
            json!({
                "symbol": p.get("symbol").cloned().unwrap_or(Value::Null),
                "quantity": qty,
                "entry_price": p.get("entry_price").cloned().unwrap_or(Value::Null),
                "stop_price": p.get("stop_price").cloned().unwrap_or(Value::Null),
                "profit_limit": p.get("profit_limit").cloned().unwrap_or(Value::Null),
                "market_value_usd": value,
                "unrealized_pct": unrealized_pct,
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
            json!({
                "underlying": p.get("underlying").cloned().unwrap_or(Value::Null),
                "strategy": p.get("strategy").cloned().unwrap_or(Value::Null),
                "expiry": p.get("expiry").cloned().unwrap_or(Value::Null),
                "contracts": p.get("contracts").cloned().unwrap_or(Value::Null),
                "entry_credit": p.get("entry_credit").cloned().unwrap_or(Value::Null),
                "max_loss_usd": p.get("max_loss_usd").cloned().unwrap_or(Value::Null),
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
    let detail = format!("{session} candidates {candidates}, rejected {rejected} {block}").trim().to_string();
    Some(json!({
        "ts": state.get("last_tick").cloned().unwrap_or(Value::Null),
        "type": "tick",
        "detail": detail,
    }))
}

fn shadow_summary(id: &str, state: &Value) -> Value {
    let state = inner_state(state);
    let positions = swing_positions(state);
    let deployed: f64 = positions.iter().filter_map(|p| p["market_value_usd"].as_f64()).sum();
    let closed = state
        .pointer("/sim/closed_trades")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (closed_count, wins, realized) = trade_stats(&closed);
    let cash = state.pointer("/sim/cash_usd").and_then(Value::as_f64);
    let start = state.pointer("/sim/starting_cash_usd").and_then(Value::as_f64);
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
        "last_tick": state.get("last_tick").cloned().unwrap_or(Value::Null),
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
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn yaml_str(yaml: &Value, path: &[&str]) -> Option<String> {
    pointer(yaml, path).and_then(|v| v.as_str()).map(str::to_string).filter(|s| !s.is_empty())
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
        "/api/status" => (
            "200 OK",
            "application/json",
            snapshot(paths).to_string(),
        ),
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
        assert_eq!(book["journal"].as_array().unwrap().len(), 1);
        assert_eq!(book["journal"][0]["type"], "sim_entry_filled");
        assert_eq!(book["shadows"][0]["id"], "big-base");
        assert_eq!(book["shadows"][0]["starting_cash_usd"], 30000.0);
        assert!(book.get("error").is_none());
    }

    #[test]
    fn request_path_ignores_the_query_string() {
        let headers = "GET /api/status?x=1 HTTP/1.1\r\nHost: jarvis\r\n\r\n";
        assert_eq!(request_path(headers), "/api/status");
        assert_eq!(request_path("GET /favicon.ico HTTP/1.1\r\n\r\n"), "/favicon.ico");
    }

    #[test]
    fn options_book_reads_the_paper_sim_state() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("options-pilot.yaml");
        std::fs::write(&rules, "agent_id: options-pilot\nsimulation:\n  starting_budget_usd: 4000\n").unwrap();
        std::fs::write(
            dir.path().join("agent-state-options-pilot.json"),
            r#"{"agent_id":"options-pilot","last_tick":"2026-08-05T15:00:00Z","open_positions":{},"tick_count":1}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("agent-sim-state-options-pilot.json"),
            r#"{"agent_id":"options-pilot","last_tick":"2026-10-08T18:00:00Z","tick_count":9,"open_positions":{"p":{"underlying":"SPY","strategy":"iron_condor","expiry":"2026-10-16","contracts":1,"entry_credit":1.2,"max_loss_usd":380}},"sim":{"starting_budget_usd":4000,"realized_pnl_usd":-71.6,"closed_trades":[{"underlying":"QQQ","pnl_usd":-71.6,"pnl_pct":-8.0,"exit_reason":"stop","hold_days":4,"closed_at":"2026-10-01T20:00:00Z"}]}}"#,
        )
        .unwrap();
        let snap = snapshot(&DashboardPaths {
            swing: vec![],
            options: vec![rules],
        });
        let book = &snap["books"][0];
        assert_eq!(book["name"], "options-pilot");
        assert_eq!(book["tick_count"], 9);
        assert_eq!(book["open_count"], 1);
        assert_eq!(book["positions"][0]["underlying"], "SPY");
        assert_eq!(book["closed_count"], 1);
        assert_eq!(book["realized_pnl_usd"], -71.6);
        assert_eq!(book["equity_usd"], 3928.4);
    }
}
