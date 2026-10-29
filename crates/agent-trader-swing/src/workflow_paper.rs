//! Paper book for a workflow file. Quotes and bars only. Never calls `place`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use broker_core::{AccountRef, AssetKind, BrokerKind, DataSource, MarketData, Position};
use chrono::Utc;
use serde_json::{json, Value};

use crate::workflow::{self, PaperLedger};

const STARTING_CASH: f64 = 4_000.0;
const JOURNAL_LIMIT: usize = 40;

pub fn file_has_workflows(path: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_yaml::from_str::<serde_yaml::Value>(&raw)
        .ok()
        .and_then(|value| value.get("workflows").cloned())
        .and_then(|value| value.as_sequence().map(|rows| !rows.is_empty()))
        .unwrap_or(false)
}

pub fn workflow_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.ends_with(".yaml") || name.ends_with(".yml")) {
            continue;
        }
        if file_has_workflows(&path.display().to_string()) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Paper-run every workflow file in `dir`. A new file is picked up on the next pass.
/// This is the workflow agent. It never calls `place`.
pub async fn run_dir(dir: &str, once: bool, interval_seconds: u64) -> Result<()> {
    let dir_path = Path::new(dir);
    std::fs::create_dir_all(dir_path).with_context(|| format!("create {}", dir_path.display()))?;
    let registry = agent_trader::build_registry()?;
    let account = public_account()?;
    let handle = registry.resolve(&account, DataSource::AccountBroker)?;
    if handle.data.broker_name() != "public" {
        anyhow::bail!("the workflow paper book reads the Public API only");
    }
    loop {
        let files = workflow_files(dir_path)?;
        if files.is_empty() {
            eprintln!("no workflow files in {}. No order was sent.", dir_path.display());
        }
        for path in &files {
            match tick(handle.data.as_ref(), &account, &path.display().to_string()).await {
                Ok(book) => println!("workflow paper book updated {}. No order was sent.", book.display()),
                Err(err) => eprintln!("workflow paper tick skipped: {err}. No order was sent."),
            }
        }
        if once {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval_seconds.max(30))).await;
    }
}

pub async fn run(rules_path: &str, once: bool, interval_seconds: u64) -> Result<()> {
    let registry = agent_trader::build_registry()?;
    let account = public_account()?;
    let handle = registry.resolve(&account, DataSource::AccountBroker)?;
    if handle.data.broker_name() != "public" {
        anyhow::bail!("the workflow paper book reads the Public API only");
    }
    loop {
        match tick(handle.data.as_ref(), &account, rules_path).await {
            Ok(path) => println!("workflow paper book updated {}. No order was sent.", path.display()),
            Err(err) => eprintln!("workflow paper tick skipped: {err}. No order was sent."),
        }
        if once {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval_seconds.max(30))).await;
    }
}

async fn tick(data: &dyn MarketData, account: &AccountRef, rules_path: &str) -> Result<PathBuf> {
    let rules = Path::new(rules_path);
    let file = workflow::load_workflow(rules)?;
    let book_path = book_path(rules)?;
    let mut book = read_book(&book_path);
    let symbols = held_symbols(&book);
    let quotes = if symbols.is_empty() {
        Vec::new()
    } else {
        data.quotes(account, &symbols).await.unwrap_or_default()
    };
    mark_positions(&mut book, &quotes);
    if book.get("name").and_then(|value| value.as_str()).is_none() {
        let stem = rules.file_stem().and_then(|stem| stem.to_str()).unwrap_or("workflow");
        book["name"] = json!(stem);
    }
    book["session"] = json!("paper");
    let cash = book["cash_usd"].as_f64().unwrap_or(STARTING_CASH);
    let equity = book["equity_usd"].as_f64().unwrap_or(cash);
    let ledger = PaperLedger {
        cash,
        equity,
        positions: ledger_positions(&book),
    };
    let flags = agent_trader::TradingFlags {
        yes: false,
        trust: false,
        dry_run: false,
        simulate: true,
    };
    let safety = agent_trader::SafetyGate::default();
    let state_path = state_path(rules)?;
    let now = Utc::now();
    let trading = broker_core::MockBroker::default();
    let report = workflow::run_due_with(
        &file,
        &trading,
        data,
        account,
        &safety,
        &flags,
        &state_path,
        rules,
        now,
        false,
        Some(&ledger),
        true,
    )
    .await?;
    let mut prices = price_map(&quotes);
    for symbol in report_symbols(&report) {
        if prices.contains_key(&symbol) {
            continue;
        }
        if let Ok(rows) = data.quotes(account, &[symbol.clone()]).await {
            if let Some(price) = rows.first().and_then(quote_price) {
                prices.insert(symbol, price);
            }
        }
    }
    apply_report(&mut book, &report, &prices, now);
    book["last_tick"] = json!(now.to_rfc3339());
    book["tick_count"] = json!(book["tick_count"].as_u64().unwrap_or(0) + 1);
    book["workflows"] = report["workflows"].clone();
    write_book(&book_path, &book)
}

pub(crate) fn apply_report(book: &mut Value, report: &Value, prices: &std::collections::HashMap<String, f64>, now: chrono::DateTime<Utc>) {
    let mut cash = book["cash_usd"].as_f64().unwrap_or(STARTING_CASH);
    let mut realized = book["realized_pnl_usd"].as_f64().unwrap_or(0.0);
    let mut wins = book["wins"].as_u64().unwrap_or(0);
    let mut closed_total = book["closed_count"].as_u64().unwrap_or(0);
    let mut positions = book["positions"].as_array().cloned().unwrap_or_default();
    let mut closed = book["recent_closes"].as_array().cloned().unwrap_or_default();
    let mut journal = book["journal"].as_array().cloned().unwrap_or_default();
    let mut series = book["pnl_series"].as_array().cloned().unwrap_or_default();
    let workflows = report["workflows"].as_array().cloned().unwrap_or_default();
    for workflow in &workflows {
        let id = workflow["id"].as_str().unwrap_or("workflow");
        let status = workflow["status"].as_str().unwrap_or("");
        if status == "already_ran" || status == "waiting" {
            continue;
        }
        let steps = workflow["steps"].as_array().cloned().unwrap_or_default();
        let mut filled = false;
        for step in &steps {
            if step["status"].as_str() != Some("simulated") {
                if step["status"].as_str() == Some("skipped") {
                    let reason = step["reason"].as_str().unwrap_or("skipped");
                    journal.push(note(&format!("{id} skipped: {reason}. No order sent.")));
                }
                continue;
            }
            let symbol = step["symbol"].as_str().unwrap_or("").to_uppercase();
            let side = step["side"].as_str().unwrap_or("");
            let Some(price) = prices.get(&symbol).copied().filter(|price| *price > 0.0) else {
                journal.push(note(&format!("{id} {symbol} quote missing. No paper fill.")));
                continue;
            };
            if side == "buy" {
                let notional = step["notional_usd"].as_f64().or_else(|| {
                    step["quantity"].as_f64().map(|qty| qty * price)
                });
                let Some(notional) = notional.filter(|amount| *amount > 0.0 && *amount <= cash + 1e-6) else {
                    journal.push(note(&format!("{id} buy {symbol} does not fit the paper cash. No order sent.")));
                    continue;
                };
                let qty = notional / price;
                cash -= notional;
                if let Some(row) = positions.iter_mut().find(|row| row["symbol"].as_str() == Some(symbol.as_str())) {
                    let held = row["quantity"].as_f64().unwrap_or(0.0);
                    let entry = row["entry_price"].as_f64().unwrap_or(price);
                    let next = held + qty;
                    row["quantity"] = json!(next);
                    row["entry_price"] = json!(if next > 0.0 { (entry * held + price * qty) / next } else { price });
                    row["last_price"] = json!(price);
                    row["unrealized_usd"] = json!(0.0);
                    row["workflow"] = json!(id);
                } else {
                    positions.push(json!({
                        "symbol": symbol,
                        "quantity": qty,
                        "entry_price": price,
                        "last_price": price,
                        "unrealized_usd": 0.0,
                        "opened_at": now.to_rfc3339(),
                        "workflow": id,
                    }));
                }
                journal.push(note(&format!(
                    "paper buy {symbol} {qty:.4} @ {price:.2} (${notional:.2}). No order sent."
                )));
                filled = true;
            } else if side == "sell" {
                let qty = step["quantity"].as_f64().unwrap_or(0.0);
                if let Some(position) = positions.iter().position(|row| row["symbol"].as_str() == Some(symbol.as_str())) {
                    let held = positions[position]["quantity"].as_f64().unwrap_or(qty);
                    let sell_qty = if qty > 0.0 { qty.min(held) } else { held };
                    let entry = positions[position]["entry_price"].as_f64().unwrap_or(price);
                    let pnl = (price - entry) * sell_qty;
                    cash += price * sell_qty;
                    realized += pnl;
                    if pnl > 0.0 {
                        wins += 1;
                    }
                    closed_total += 1;
                    let left = held - sell_qty;
                    let opened = positions[position]["opened_at"].as_str().unwrap_or("").to_string();
                    if left <= 1e-8 {
                        positions.remove(position);
                    } else {
                        positions[position]["quantity"] = json!(left);
                    }
                    closed.push(json!({
                        "closed_at": now.to_rfc3339(),
                        "symbol": symbol,
                        "pnl_usd": pnl,
                        "pnl_pct": if entry > 0.0 { (price - entry) / entry * 100.0 } else { 0.0 },
                        "exit_reason": "workflow",
                        "hold_days": held_days(&opened, now),
                    }));
                    journal.push(note(&format!(
                        "paper sell {symbol} {sell_qty:.4} @ {price:.2} ({pnl:.2}). No order sent."
                    )));
                    filled = true;
                }
            }
        }
        if !filled && status != "skipped" && status != "simulated" && status != "submitted" {
            if let Some(reason) = steps.iter().find_map(|step| step["reason"].as_str()) {
                journal.push(note(&format!("{id} {status}: {reason}. No order sent.")));
            }
        }
    }
    let unrealized: f64 = positions.iter().filter_map(|row| row["unrealized_usd"].as_f64()).sum();
    series.push(json!({ "at": now.to_rfc3339(), "realized_usd": realized }));
    if series.len() > 180 {
        series.drain(0..series.len() - 180);
    }
    if journal.len() > JOURNAL_LIMIT {
        journal.drain(0..journal.len() - JOURNAL_LIMIT);
    }
    if closed.len() > 12 {
        closed.drain(0..closed.len() - 12);
    }
    let win_rate = if closed_total == 0 { 0.0 } else { wins as f64 * 100.0 / closed_total as f64 };
    book["stack"] = json!("agent-trader");
    book["broker"] = json!("public");
    book["broker_label"] = json!("Public API");
    book["kind"] = json!("workflow");
    book["name"] = json!(book["name"].as_str().unwrap_or("workflow"));
    book["live_orders"] = json!(false);
    book["note"] = json!("Paper workflow. Quotes only. No order was sent.");
    book["cash_usd"] = json!(cash);
    book["starting_cash_usd"] = json!(STARTING_CASH);
    // Equity is cash plus the market value of open shares, not cash plus unrealized P/L alone.
    let market_value: f64 = positions
        .iter()
        .map(|row| {
            let qty = row["quantity"].as_f64().unwrap_or(0.0);
            let px = row["last_price"].as_f64().or_else(|| row["entry_price"].as_f64()).unwrap_or(0.0);
            qty * px
        })
        .sum();
    book["equity_usd"] = json!(cash + market_value);
    book["unrealized_pnl_usd"] = json!(unrealized);
    book["realized_pnl_usd"] = json!(realized);
    book["open_count"] = json!(positions.len());
    book["closed_count"] = json!(closed_total);
    book["wins"] = json!(wins);
    book["win_rate_pct"] = json!(win_rate);
    book["positions"] = json!(positions);
    book["recent_closes"] = json!(closed);
    book["journal"] = json!(journal);
    book["pnl_series"] = json!(series);
}

fn mark_positions(book: &mut Value, quotes: &[broker_core::Quote]) {
    let Some(positions) = book["positions"].as_array_mut() else {
        return;
    };
    for position in positions {
        let symbol = position["symbol"].as_str().unwrap_or("");
        let Some(last) = quotes.iter().find(|quote| quote.symbol.eq_ignore_ascii_case(symbol)).and_then(quote_price) else {
            continue;
        };
        let entry = position["entry_price"].as_f64().unwrap_or(last);
        let qty = position["quantity"].as_f64().unwrap_or(0.0);
        position["last_price"] = json!(last);
        position["unrealized_usd"] = json!((last - entry) * qty);
    }
}

fn price_map(quotes: &[broker_core::Quote]) -> std::collections::HashMap<String, f64> {
    quotes.iter().filter_map(|quote| quote_price(quote).map(|price| (quote.symbol.to_uppercase(), price))).collect()
}

fn quote_price(quote: &broker_core::Quote) -> Option<f64> {
    quote.last.filter(|price| *price > 0.0).or(quote.bid.filter(|price| *price > 0.0))
}

fn held_symbols(book: &Value) -> Vec<String> {
    book["positions"]
        .as_array()
        .map(|rows| {
            rows.iter().filter_map(|row| row["symbol"].as_str().map(|symbol| symbol.to_uppercase())).collect()
        })
        .unwrap_or_default()
}

fn report_symbols(report: &Value) -> Vec<String> {
    let mut symbols = Vec::new();
    for workflow in report["workflows"].as_array().into_iter().flatten() {
        for step in workflow["steps"].as_array().into_iter().flatten() {
            if step["status"].as_str() == Some("simulated") {
                if let Some(symbol) = step["symbol"].as_str() {
                    symbols.push(symbol.to_uppercase());
                }
            }
        }
    }
    symbols
}

fn ledger_positions(book: &Value) -> Vec<Position> {
    book["positions"]
        .as_array()
        .map(|rows| {
            rows.iter().filter_map(|row| {
                let symbol = row["symbol"].as_str()?.to_string();
                Some(Position {
                    symbol,
                    asset: AssetKind::Equity,
                    quantity: row["quantity"].as_f64().unwrap_or(0.0),
                    average_price: row["entry_price"].as_f64(),
                    market_value: row["last_price"].as_f64().zip(row["quantity"].as_f64()).map(|(price, qty)| price * qty),
                })
            }).collect()
        })
        .unwrap_or_default()
}

fn held_days(opened_at: &str, now: chrono::DateTime<Utc>) -> i64 {
    let Ok(opened) = chrono::DateTime::parse_from_rfc3339(opened_at) else {
        return 0;
    };
    (now - opened.with_timezone(&Utc)).num_days().max(0)
}

fn note(detail: &str) -> Value {
    json!({ "ts": Utc::now().to_rfc3339(), "type": "paper", "detail": detail })
}

fn read_book(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({
            "cash_usd": STARTING_CASH,
            "equity_usd": STARTING_CASH,
            "positions": [],
            "journal": [],
            "recent_closes": [],
            "pnl_series": [],
        }))
}

fn book_path(rules: &Path) -> Result<PathBuf> {
    let stem = rules.file_stem().and_then(|stem| stem.to_str()).unwrap_or("workflow");
    Ok(Path::new("rules").join(format!("agent-trader-workflow-{stem}.json")))
}

fn state_path(rules: &Path) -> Result<PathBuf> {
    let stem = rules.file_stem().and_then(|stem| stem.to_str()).unwrap_or("workflow");
    Ok(agent_trader::state_dir()?.join(format!("workflow-paper-{stem}.json")))
}

fn write_book(path: &Path, book: &Value) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(book)?;
    std::fs::write(path, &pretty).with_context(|| format!("write {}", path.display()))?;
    if let Ok(dest) = std::env::var("AGENT_TRADER_WORKFLOW_PAPER_PUBLISH") {
        if !dest.is_empty() {
            let _ = std::process::Command::new("rsync").args(["-q", &path.display().to_string(), &dest]).status();
        }
    }
    Ok(path.to_path_buf())
}

fn public_account() -> Result<AccountRef> {
    let id = std::env::var("PUBLIC_ACCOUNT_ID").context("PUBLIC_ACCOUNT_ID is required for the workflow paper book")?;
    Ok(AccountRef::new(BrokerKind::Public, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn workflow_directory_lists_only_workflow_files() {
        let dir = std::env::temp_dir().join(format!("workflow-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("msft.yaml"), "version: 1\nworkflows:\n  - id: msft-dip\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "workflows:\n").unwrap();
        std::fs::write(dir.join("swing.yaml"), "watchlist:\n  - AAPL\n").unwrap();
        let files = workflow_files(&dir).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("msft.yaml"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn paper_buy_debits_cash_once_and_a_later_tick_does_not_buy_again() {
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 15, 0, 0).unwrap();
        let mut book = json!({
            "name": "msft-dip",
            "cash_usd": 4000.0,
            "positions": [],
            "journal": [],
            "recent_closes": [],
            "pnl_series": [],
        });
        let mut prices = std::collections::HashMap::new();
        prices.insert("MSFT".into(), 100.0);
        let report = json!({
            "workflows": [{
                "id": "msft-dip",
                "status": "simulated",
                "steps": [{
                    "id": "buy",
                    "status": "simulated",
                    "side": "buy",
                    "symbol": "MSFT",
                    "notional_usd": 250.0
                }]
            }]
        });
        apply_report(&mut book, &report, &prices, now);
        assert!((book["cash_usd"].as_f64().unwrap() - 3750.0).abs() < 1e-6);
        assert_eq!(book["positions"][0]["quantity"], 2.5);
        // equity counts the shares at market, not just cash plus unrealized P/L
        assert!((book["equity_usd"].as_f64().unwrap() - 4000.0).abs() < 1e-6);
        assert_eq!(book["kind"], "workflow");
        assert_eq!(book["live_orders"], false);
        let again = json!({
            "workflows": [{ "id": "msft-dip", "status": "already_ran", "steps": [] }]
        });
        apply_report(&mut book, &again, &prices, now);
        assert_eq!(book["positions"].as_array().unwrap().len(), 1);
        assert!((book["cash_usd"].as_f64().unwrap() - 3750.0).abs() < 1e-6);
    }
}
