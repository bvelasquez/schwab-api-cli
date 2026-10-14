use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use broker_core::{AccountRef, BrokerKind, DataSource, MarketData};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::symbols_in;

const STARTING_CASH: f64 = 4_000.0;

/// Public swing paper book. Quotes only: this function never calls `place`.
pub async fn run_public_paper(rules_path: &str, once: bool, interval_seconds: u64) -> Result<()> {
    let setup = load_setup(rules_path)?;
    let registry = agent_trader::build_registry()?;
    let account = public_account()?;
    let handle = registry.resolve(&account, DataSource::AccountBroker)?;
    if handle.data.broker_name() != "public" {
        anyhow::bail!("the public swing paper book reads the Public API only");
    }
    loop {
        match tick_once(handle.data.as_ref(), &account, &setup).await {
            Ok(book) => {
                let path = write_book(&book)?;
                println!(
                    "swing paper book updated {} (tick {}, open {}). No order was sent.",
                    path.display(),
                    book["tick_count"],
                    book["open_count"]
                );
            }
            Err(err) => {
                eprintln!(
                    "swing paper tick skipped: {}. No order was sent.",
                    brief_error(&err)
                );
            }
        }
        if once {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval_seconds.max(30))).await;
    }
}

async fn tick_once(data: &dyn MarketData, account: &AccountRef, setup: &Setup) -> Result<Value> {
    let previous = read_book(&book_path()?);
    let tick = previous["tick_count"].as_u64().unwrap_or(0) + 1;
    let mut cash = previous["cash_usd"].as_f64().unwrap_or(setup.sleeve);
    let mut realized = previous["realized_pnl_usd"].as_f64().unwrap_or(0.0);
    let mut wins = previous["wins"].as_u64().unwrap_or(0);
    let mut closed_total = previous["closed_count"].as_u64().unwrap_or(0);
    let mut positions = previous["positions"].as_array().cloned().unwrap_or_default();
    let mut closed = previous["recent_closes"].as_array().cloned().unwrap_or_default();
    let mut journal = previous["journal"].as_array().cloned().unwrap_or_default();
    let mut series = previous["pnl_series"].as_array().cloned().unwrap_or_default();
    let session = data.session_open(account).await?;
    let quotes = data.quotes(account, &setup.watchlist).await?;
    let price_of = |symbol: &str| -> Option<f64> {
        quotes.iter().find(|quote| quote.symbol.eq_ignore_ascii_case(symbol)).and_then(|quote| {
            quote.last.filter(|price| *price > 0.0).or_else(|| {
                match (quote.bid, quote.ask) {
                    (Some(bid), Some(ask)) if bid > 0.0 && ask > 0.0 => Some((bid + ask) / 2.0),
                    _ => quote.bid.filter(|price| *price > 0.0),
                }
            })
        })
    };

    let mut still_open = Vec::new();
    for mut position in positions.drain(..) {
        let symbol = position["symbol"].as_str().unwrap_or("").to_string();
        let entry = position["entry_price"].as_f64().unwrap_or(0.0);
        let qty = position["quantity"].as_f64().unwrap_or(0.0);
        let Some(last) = price_of(&symbol) else {
            still_open.push(position);
            journal.push(note(&format!("{symbol} quote missing. Position left open. No order sent.")));
            continue;
        };
        let peak = position["peak_profit_pct"].as_f64().unwrap_or(0.0).max(if entry > 0.0 {
            (last - entry) / entry * 100.0
        } else {
            0.0
        });
        position["last_price"] = json!(last);
        position["peak_profit_pct"] = json!(peak);
        position["unrealized_usd"] = json!((last - entry) * qty);
        let opened = position["opened_at"].as_str().unwrap_or("");
        let held = held_days(opened);
        let stop = position["stop_price"].as_f64().unwrap_or(0.0);
        let target = position["profit_limit"].as_f64().unwrap_or(0.0);
        let reason = exit_reason(entry, last, stop, target, held, setup.max_hold_days, setup.invalidation_pct);
        if reason.is_none() {
            still_open.push(position);
            continue;
        }
        let reason = reason.unwrap_or("exit");
        let pnl = (last - entry) * qty;
        cash += last * qty;
        realized += pnl;
        if pnl > 0.0 {
            wins += 1;
        }
        closed_total += 1;
        closed.push(json!({
            "closed_at": Utc::now().to_rfc3339(),
            "symbol": symbol,
            "pnl_usd": pnl,
            "pnl_pct": if entry > 0.0 { (last - entry) / entry * 100.0 } else { 0.0 },
            "exit_reason": reason,
            "hold_days": held
        }));
        journal.push(note(&format!(
            "paper close {symbol} {qty:.0} @ {last:.2} {pnl:.2} ({reason}). No order sent."
        )));
    }
    positions = still_open;

    let slot = setup.sleeve / setup.max_open as f64;
    if positions.len() < setup.max_open {
        for symbol in &setup.watchlist {
            if positions.len() >= setup.max_open {
                break;
            }
            if positions.iter().any(|row| row["symbol"].as_str() == Some(symbol.as_str())) {
                continue;
            }
            let Some(last) = price_of(symbol) else {
                journal.push(note(&format!("{symbol} has no Public quote")));
                continue;
            };
            let qty = (slot / last).floor();
            if qty < 1.0 || cash < last * qty {
                journal.push(note(&format!(
                    "{symbol} skipped, paper cash {cash:.0} cannot buy a share at {last:.2}"
                )));
                continue;
            }
            let stop = last * (1.0 - setup.stop_pct / 100.0);
            let target = last * (1.0 + setup.profit_pct / 100.0);
            cash -= last * qty;
            let session_note = if session.open {
                "session open"
            } else {
                "session closed, fill taken from the latest Public quote"
            };
            positions.push(json!({
                "symbol": symbol,
                "quantity": qty,
                "entry_price": last,
                "last_price": last,
                "stop_price": stop,
                "profit_limit": target,
                "unrealized_usd": 0.0,
                "stop_risk_usd": (last - stop) * qty,
                "peak_profit_pct": 0.0,
                "opened_at": Utc::now().to_rfc3339()
            }));
            journal.push(note(&format!(
                "paper open {symbol} {qty:.0} @ {last:.2} stop {stop:.2} target {target:.2} ({session_note}). No order sent."
            )));
        }
    }

    if journal.len() > 40 {
        journal = journal.split_off(journal.len() - 40);
    }
    if closed.len() > 12 {
        closed = closed.split_off(closed.len() - 12);
    }
    series.push(json!({"at": Utc::now().to_rfc3339(), "realized_usd": realized}));
    if series.len() > 80 {
        series = series.split_off(series.len() - 80);
    }
    let market_value: f64 = positions
        .iter()
        .map(|row| row["last_price"].as_f64().unwrap_or(0.0) * row["quantity"].as_f64().unwrap_or(0.0))
        .sum();
    let unrealized: f64 = positions.iter().filter_map(|row| row["unrealized_usd"].as_f64()).sum();
    let win_rate = if closed_total == 0 { 0.0 } else { wins as f64 * 100.0 / closed_total as f64 };
    Ok(json!({
        "stack": "agent-trader",
        "broker": "public",
        "broker_label": "Public API",
        "kind": "swing",
        "name": "agent-trader · Public API",
        "rules_file": "rules/agent-trader-public-swing-paper.yaml",
        "mode": "paper",
        "live_orders": false,
        "note": "Simulated share fills from the Public API. This book never submits an order.",
        "last_tick": Utc::now().to_rfc3339(),
        "tick_count": tick,
        "session": if session.open { "open" } else { "closed" },
        "profile": "public-swing-paper",
        "sleeve_cap_usd": setup.sleeve,
        "starting_cash_usd": setup.sleeve,
        "cash_usd": cash,
        "equity_usd": cash + market_value,
        "unrealized_pnl_usd": unrealized,
        "realized_pnl_usd": realized,
        "open_count": positions.len(),
        "closed_count": closed_total,
        "wins": wins,
        "win_rate_pct": win_rate,
        "positions": positions,
        "recent_closes": closed,
        "journal": journal,
        "pnl_series": series,
        "plan": {
            "stop_loss_pct": setup.stop_pct,
            "profit_target_pct": setup.profit_pct,
            "time_stop_days": setup.max_hold_days,
            "max_hold_days": setup.max_hold_days,
            "target_hold_days": setup.max_hold_days
        }
    }))
}

pub(crate) fn exit_reason(
    entry: f64,
    last: f64,
    stop: f64,
    target: f64,
    held_days: i64,
    max_hold_days: i64,
    invalidation_pct: f64,
) -> Option<&'static str> {
    if entry <= 0.0 || last <= 0.0 {
        return None;
    }
    if stop > 0.0 && last <= stop {
        return Some("stop");
    }
    if target > 0.0 && last >= target {
        return Some("target");
    }
    if (entry - last) / entry * 100.0 >= invalidation_pct {
        return Some("thesis");
    }
    if held_days >= max_hold_days {
        return Some("time");
    }
    None
}

fn held_days(opened_at: &str) -> i64 {
    let Ok(opened) = chrono::DateTime::parse_from_rfc3339(opened_at) else {
        return 0;
    };
    (Utc::now() - opened.with_timezone(&Utc)).num_days().max(0)
}

fn brief_error(err: &anyhow::Error) -> String {
    let mut text = err.to_string();
    if let Ok(id) = std::env::var("PUBLIC_ACCOUNT_ID") {
        if !id.is_empty() {
            text = text.replace(&id, "<account>");
        }
    }
    let line = text.lines().next().unwrap_or("quote request failed");
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(180).collect()
}

fn note(detail: &str) -> Value {
    json!({"ts": Utc::now().to_rfc3339(), "type": "paper", "detail": detail})
}

fn book_path() -> Result<PathBuf> {
    Ok(Path::new("rules").join("agent-trader-public-swing-paper.json"))
}

fn read_book(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({"cash_usd": STARTING_CASH, "positions": [], "journal": [], "recent_closes": []}))
}

fn write_book(book: &Value) -> Result<PathBuf> {
    let path = book_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(book)?;
    std::fs::write(&path, &pretty).with_context(|| format!("write {}", path.display()))?;
    if let Ok(dest) = std::env::var("AGENT_TRADER_SWING_PAPER_PUBLISH") {
        if !dest.is_empty() {
            let _ = std::process::Command::new("rsync")
                .args(["-q", &path.display().to_string(), &dest])
                .status();
        }
    }
    if let Ok(dir) = agent_trader::state_dir() {
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("public-swing-paper.json"), pretty).ok();
    }
    Ok(path)
}

fn public_account() -> Result<AccountRef> {
    let id = std::env::var("PUBLIC_ACCOUNT_ID")
        .context("PUBLIC_ACCOUNT_ID is required for the Public swing paper book")?;
    Ok(AccountRef::new(BrokerKind::Public, id))
}

struct Setup {
    watchlist: Vec<String>,
    sleeve: f64,
    stop_pct: f64,
    profit_pct: f64,
    invalidation_pct: f64,
    max_open: usize,
    max_hold_days: i64,
}

#[derive(Debug, Deserialize)]
struct RulesFile {
    #[serde(default)]
    capital: CapitalYaml,
    #[serde(default)]
    exits: ExitYaml,
    #[serde(default)]
    watchlists: Value,
    #[serde(default)]
    max_open_positions: Option<usize>,
    #[serde(default)]
    max_hold_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CapitalYaml {
    #[serde(default = "sleeve_default")]
    fixed_sleeve_cap_usd: f64,
}

impl Default for CapitalYaml {
    fn default() -> Self {
        Self { fixed_sleeve_cap_usd: STARTING_CASH }
    }
}

fn sleeve_default() -> f64 {
    STARTING_CASH
}

#[derive(Debug, Deserialize)]
struct ExitYaml {
    #[serde(default = "profit_default")]
    profit_pct: f64,
    #[serde(default = "stop_default")]
    stop_pct: f64,
    #[serde(default = "thesis_default")]
    invalidation_pct: f64,
}

impl Default for ExitYaml {
    fn default() -> Self {
        Self {
            profit_pct: 8.0,
            stop_pct: 4.0,
            invalidation_pct: 6.0,
        }
    }
}

fn profit_default() -> f64 { 8.0 }
fn stop_default() -> f64 { 4.0 }
fn thesis_default() -> f64 { 6.0 }

fn load_setup(path: &str) -> Result<Setup> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    let rules: RulesFile = serde_yaml::from_str(&raw)?;
    let watchlist = symbols_in(&rules.watchlists);
    if watchlist.is_empty() {
        anyhow::bail!("{path} has no watchlist symbols");
    }
    Ok(Setup {
        watchlist,
        sleeve: rules.capital.fixed_sleeve_cap_usd,
        stop_pct: rules.exits.stop_pct,
        profit_pct: rules.exits.profit_pct,
        invalidation_pct: rules.exits.invalidation_pct,
        max_open: rules.max_open_positions.unwrap_or(3).max(1),
        max_hold_days: rules.max_hold_days.unwrap_or(30).max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::exit_reason;

    #[test]
    fn quote_timeout_message_drops_the_account_id() {
        std::env::set_var("PUBLIC_ACCOUNT_ID", "paper-account");
        let err = anyhow::anyhow!(
            "Public /userapigateway/marketdata/paper-account/quotes failed (HTTP 504 Gateway Timeout): <html>"
        );
        let text = super::brief_error(&err);
        assert!(!text.contains("paper-account"));
        assert!(text.contains("<account>"));
        assert!(text.contains("504"));
    }

    #[test]
    fn stop_hits_before_the_thesis_band() {
        assert_eq!(exit_reason(100.0, 96.0, 96.0, 108.0, 1, 30, 6.0), Some("stop"));
        assert_eq!(exit_reason(100.0, 108.0, 96.0, 108.0, 1, 30, 6.0), Some("target"));
        assert_eq!(exit_reason(100.0, 101.0, 96.0, 108.0, 30, 30, 6.0), Some("time"));
        assert_eq!(exit_reason(100.0, 101.0, 96.0, 108.0, 2, 30, 6.0), None);
    }
}
