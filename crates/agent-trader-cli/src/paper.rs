use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use broker_core::{AccountRef, BrokerKind, DataSource, MarketData};
use chrono::Utc;
use serde_json::{json, Value};

use crate::options::{exit_action, paper_setup, pick_from_market, ExitAction};
use crate::paths::state_dir;
use crate::registry::build_registry;

const STARTING_CASH: f64 = 10_000.0;

/// Public paper book. Market data only: this function never calls `place`.
pub async fn run_public_paper(rules_path: &str, once: bool, interval_seconds: u64) -> Result<()> {
    let setup = paper_setup(rules_path)?;
    let registry = build_registry()?;
    let account = public_account()?;
    let handle = registry.resolve(&account, DataSource::AccountBroker)?;
    if handle.data.broker_name() != "public" {
        anyhow::bail!("the public paper book reads the Public API only");
    }
    loop {
        match tick_once(handle.data.as_ref(), &account, &setup).await {
            Ok(book) => {
                let path = write_book(&book)?;
                println!(
                    "paper book updated {} (tick {}, open {}). No order was sent.",
                    path.display(),
                    book["tick_count"],
                    book["open_count"]
                );
            }
            Err(err) => {
                eprintln!("paper tick skipped: {}. No order was sent.", brief_error(&err));
            }
        }
        if once {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval_seconds.max(30))).await;
    }
}

async fn tick_once(
    data: &dyn MarketData,
    account: &AccountRef,
    setup: &crate::options::PaperSetup,
) -> Result<Value> {
    let path = book_path()?;
    let previous = read_book(&path);
    let tick = previous["tick_count"].as_u64().unwrap_or(0) + 1;
    let mut cash = previous["cash_usd"].as_f64().unwrap_or(STARTING_CASH);
    let mut realized = previous["realized_pnl_usd"].as_f64().unwrap_or(0.0);
    let mut wins = previous["wins"].as_u64().unwrap_or(0);
    let mut closed_total = previous["closed_count"].as_u64().unwrap_or(0);
    let mut positions = previous["positions"].as_array().cloned().unwrap_or_default();
    let mut closed = previous["recent_closes"].as_array().cloned().unwrap_or_default();
    let mut journal = previous["journal"].as_array().cloned().unwrap_or_default();
    let session = data.session_open(account).await?;
    let today = Utc::now().date_naive();

    let mut still_open = Vec::new();
    for mut position in positions.drain(..) {
        let underlying = position["underlying"].as_str().unwrap_or("").to_string();
        let expiry = position["expiry"].as_str().unwrap_or("").to_string();
        if let Ok(chain) = data.option_chain(account, &underlying, &expiry).await {
            if let Some(debit) = close_debit(&chain, &position) {
                let credit = position["entry_credit"].as_f64().unwrap_or(0.0);
                let contracts = position["contracts"].as_f64().unwrap_or(1.0);
                position["debit_to_close"] = json!(debit);
                position["unrealized_usd"] = json!((credit - debit) * 100.0 * contracts);
            }
        }
        let credit = position["entry_credit"].as_f64().unwrap_or(0.0);
        let debit = position["debit_to_close"].as_f64().unwrap_or(credit);
        let dte = expiry_dte(&expiry, today);
        let action = exit_action(credit, debit, dte, 10.0, 0.0, &setup.exits);
        if matches!(action, ExitAction::Hold) {
            still_open.push(position);
            continue;
        }
        let reason = match action {
            ExitAction::Close { reason } | ExitAction::Roll { reason } => reason,
            ExitAction::Hold => "hold".into(),
        };
        let contracts = position["contracts"].as_f64().unwrap_or(1.0);
        let pnl = (credit - debit) * 100.0 * contracts;
        cash += position["max_loss_usd"].as_f64().unwrap_or(0.0) + pnl;
        realized += pnl;
        if pnl > 0.0 {
            wins += 1;
        }
        closed_total += 1;
        closed.push(json!({
            "closed_at": Utc::now().to_rfc3339(),
            "underlying": underlying,
            "pnl_usd": pnl,
            "pnl_pct": if credit > 0.0 { pnl / (credit * 100.0 * contracts) * 100.0 } else { 0.0 },
            "exit_reason": reason,
            "hold_days": 0
        }));
        journal.push(note(&format!(
            "paper close {underlying} {pnl:.2} ({reason}). No order sent."
        )));
    }
    positions = still_open;

    if positions.len() < setup.max_open {
        for symbol in &setup.watchlist {
            if positions.len() >= setup.max_open {
                break;
            }
            if positions.iter().any(|row| row["underlying"].as_str() == Some(symbol)) {
                continue;
            }
            let choice = match pick_from_market(data, account, symbol, &setup.vertical, today).await {
                Ok(choice) => choice,
                Err(err) => {
                    journal.push(note(&format!("{symbol} chain unavailable: {err}")));
                    continue;
                }
            };
            let Some(choice) = choice else {
                journal.push(note(&format!("no {symbol} put-credit in the delta band")));
                continue;
            };
            let max_loss = (choice.width - choice.credit).max(0.0) * 100.0;
            if max_loss <= 0.0 || cash < max_loss {
                journal.push(note(&format!(
                    "{symbol} skipped, paper cash {cash:.0} is below max loss {max_loss:.0}"
                )));
                continue;
            }
            cash -= max_loss;
            positions.push(json!({
                "underlying": choice.underlying,
                "strategy": "put_credit",
                "spread_type": "put credit",
                "expiry": choice.expiration,
                "contracts": 1.0,
                "entry_credit": choice.credit,
                "debit_to_close": choice.credit,
                "unrealized_usd": 0.0,
                "max_loss_usd": max_loss,
                "entry_short_delta": choice.delta,
                "short_symbol": choice.short_symbol,
                "long_symbol": choice.long_symbol,
                "short_strike": choice.short_strike,
                "long_strike": choice.long_strike,
                "opened_at": Utc::now().to_rfc3339()
            }));
            let session_note = if session.open {
                "session open"
            } else {
                "session closed, fill taken from the latest Public quote"
            };
            journal.push(note(&format!(
                "paper open {symbol} {}/{} credit {:.2} ({session_note}). No order sent.",
                choice.short_strike, choice.long_strike, choice.credit
            )));
        }
    }

    if journal.len() > 40 {
        journal = journal.split_off(journal.len() - 40);
    }
    if closed.len() > 12 {
        closed = closed.split_off(closed.len() - 12);
    }
    let mut unrealized: f64 = positions.iter().filter_map(|row| row["unrealized_usd"].as_f64()).sum();
    if unrealized.abs() < 1e-9 {
        unrealized = 0.0;
    }
    let win_rate = if closed_total == 0 {
        0.0
    } else {
        wins as f64 * 100.0 / closed_total as f64
    };
    Ok(json!({
        "stack": "agent-trader",
        "broker": "public",
        "broker_label": "Public API",
        "kind": "options",
        "name": "agent-trader · Public API",
        "rules_file": "rules/agent-trader-public-paper.yaml",
        "mode": "paper",
        "live_orders": false,
        "note": "Simulated fills from the Public API. This book never submits an order.",
        "last_tick": Utc::now().to_rfc3339(),
        "tick_count": tick,
        "session": if session.open { "open" } else { "closed" },
        "profile": "public-paper",
        "sleeve_cap_usd": STARTING_CASH,
        "starting_cash_usd": STARTING_CASH,
        "cash_usd": cash,
        "equity_usd": cash + unrealized,
        "unrealized_pnl_usd": unrealized,
        "realized_pnl_usd": realized,
        "open_count": positions.len(),
        "closed_count": closed_total,
        "wins": wins,
        "win_rate_pct": win_rate,
        "positions": positions,
        "recent_closes": closed,
        "journal": journal
    }))
}

fn close_debit(chain: &broker_core::OptionChain, position: &Value) -> Option<f64> {
    let short = position["short_symbol"].as_str()?;
    let long = position["long_symbol"].as_str()?;
    let short_ask = chain.contracts.iter().find(|contract| contract.symbol == short)?.ask?;
    let long_bid = chain.contracts.iter().find(|contract| contract.symbol == long)?.bid?;
    Some((short_ask - long_bid).max(0.01))
}

fn expiry_dte(expiry: &str, today: chrono::NaiveDate) -> i64 {
    chrono::NaiveDate::parse_from_str(expiry, "%Y-%m-%d")
        .map(|date| (date - today).num_days())
        .unwrap_or(30)
}

fn brief_error(err: &anyhow::Error) -> String {
    let mut text = err.to_string();
    if let Ok(id) = std::env::var("PUBLIC_ACCOUNT_ID") {
        if !id.is_empty() {
            text = text.replace(&id, "<account>");
        }
    }
    let line = text.lines().next().unwrap_or("market data request failed");
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(180).collect()
}

fn note(detail: &str) -> Value {
    json!({"ts": Utc::now().to_rfc3339(), "type": "paper", "detail": detail})
}

pub fn book_path() -> Result<PathBuf> {
    Ok(Path::new("rules").join("agent-trader-public-paper.json"))
}

fn read_book(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| {
            json!({
                "cash_usd": STARTING_CASH,
                "positions": [],
                "journal": [],
                "recent_closes": []
            })
        })
}

fn write_book(book: &Value) -> Result<PathBuf> {
    let path = book_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(book)?;
    std::fs::write(&path, &pretty).with_context(|| format!("write {}", path.display()))?;
    if let Ok(dest) = std::env::var("AGENT_TRADER_PAPER_PUBLISH") {
        if !dest.is_empty() {
            let _ = std::process::Command::new("rsync")
                .args(["-q", &path.display().to_string(), &dest])
                .status();
        }
    }
    if let Ok(dir) = state_dir() {
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("public-options-paper.json"), pretty).ok();
    }
    Ok(path)
}

fn public_account() -> Result<AccountRef> {
    let id = std::env::var("PUBLIC_ACCOUNT_ID")
        .context("PUBLIC_ACCOUNT_ID is required for the Public paper book")?;
    Ok(AccountRef::new(BrokerKind::Public, id))
}
