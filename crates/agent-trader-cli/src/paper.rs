use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use broker_core::{AccountRef, BrokerKind, DataSource, MarketData, Quote};
use chrono::Utc;
use serde_json::{json, Value};

use crate::options::{close_intent, exit_action, paper_setup, pick_from_market, ExitAction};
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
    let use_strategy_quote = handle.trading.capabilities().strategy_quote;
    let path = book_path()?;
    loop {
        match tick_once(
            handle.data.as_ref(),
            &account,
            &setup,
            &path,
            use_strategy_quote,
        )
        .await
        {
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
                eprintln!(
                    "paper tick skipped: {}. No order was sent.",
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

async fn tick_once(
    data: &dyn MarketData,
    account: &AccountRef,
    setup: &crate::options::PaperSetup,
    book_file: &Path,
    use_strategy_quote: bool,
) -> Result<Value> {
    let previous = read_book(book_file);
    let tick = previous["tick_count"].as_u64().unwrap_or(0) + 1;
    let mut cash = previous["cash_usd"].as_f64().unwrap_or(STARTING_CASH);
    let mut realized = previous["realized_pnl_usd"].as_f64().unwrap_or(0.0);
    let mut wins = previous["wins"].as_u64().unwrap_or(0);
    let mut closed_total = previous["closed_count"].as_u64().unwrap_or(0);
    let mut positions = previous["positions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut closed = previous["recent_closes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut journal = previous["journal"].as_array().cloned().unwrap_or_default();
    let session = data.session_open(account).await?;
    let today = Utc::now().date_naive();

    let mut still_open = Vec::new();
    for mut position in positions.drain(..) {
        let underlying = position["underlying"].as_str().unwrap_or("").to_string();
        let expiry = position["expiry"].as_str().unwrap_or("").to_string();
        let width = position_width(&position);
        let mark = match data.option_chain(account, &underlying, &expiry).await {
            Ok(chain) => {
                let legs = leg_quotes(&chain, &position);
                let strategy = if use_strategy_quote {
                    strategy_quote_debit(data, account, &position).await
                } else {
                    None
                };
                resolve_close_mark(width, legs, strategy, use_strategy_quote)
            }
            Err(_) => CloseMark::Degraded,
        };
        match mark {
            CloseMark::Usable(debit) => {
                let credit = position["entry_credit"].as_f64().unwrap_or(0.0);
                let contracts = position["contracts"].as_f64().unwrap_or(1.0);
                position["debit_to_close"] = json!(debit);
                position["quote_degraded"] = json!(false);
                position["unrealized_usd"] = json!((credit - debit) * 100.0 * contracts);
            }
            CloseMark::Degraded => {
                position["quote_degraded"] = json!(true);
                still_open.push(position);
                continue;
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
            "hold_days": 0,
            "debit": debit,
            "width": width,
            "contracts": contracts,
            "entry_credit": credit,
            "short_strike": position["short_strike"].clone(),
            "long_strike": position["long_strike"].clone(),
            "mark_suspect": false
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
            if positions
                .iter()
                .any(|row| row["underlying"].as_str() == Some(symbol))
            {
                continue;
            }
            let choice = match pick_from_market(data, account, symbol, &setup.vertical, today).await
            {
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
                "opened_at": Utc::now().to_rfc3339(),
                "quote_degraded": false
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
    let (mark_suspect_count, clean_delta) = restate_closes(&mut closed);
    let mut unrealized: f64 = positions
        .iter()
        .filter_map(|row| row["unrealized_usd"].as_f64())
        .sum();
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
        "clean_realized_pnl_usd": realized + clean_delta,
        "mark_suspect_count": mark_suspect_count,
        "open_count": positions.len(),
        "closed_count": closed_total,
        "wins": wins,
        "win_rate_pct": win_rate,
        "positions": positions,
        "recent_closes": closed,
        "journal": journal
    }))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LegQuotes {
    short_ask: Option<f64>,
    long_bid: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CloseMark {
    Usable(f64),
    Degraded,
}

/// Prefer a strategy-quote debit. A non-positive leg, a missing strategy quote,
/// or a debit wider than the spread is degraded: keep the last good mark and do
/// not book the close. A usable debit is capped at the spread width.
fn resolve_close_mark(
    width: Option<f64>,
    legs: LegQuotes,
    strategy_debit: Option<f64>,
    use_strategy_quote: bool,
) -> CloseMark {
    let legs_bad = legs.short_ask.unwrap_or(0.0) <= 0.0 || legs.long_bid.unwrap_or(0.0) <= 0.0;
    if legs_bad {
        return CloseMark::Degraded;
    }
    let raw = if use_strategy_quote {
        match strategy_debit {
            Some(debit) if debit > 0.0 => debit,
            _ => return CloseMark::Degraded,
        }
    } else {
        (legs.short_ask.unwrap_or(0.0) - legs.long_bid.unwrap_or(0.0)).max(0.01)
    };
    if width.is_some_and(|limit| raw > limit + 1e-6) {
        return CloseMark::Degraded;
    }
    let debit = match width {
        Some(limit) if limit > 0.0 => raw.min(limit),
        _ => raw,
    };
    CloseMark::Usable(debit)
}

fn position_width(position: &Value) -> Option<f64> {
    let short = position.get("short_strike").and_then(Value::as_f64)?;
    let long = position.get("long_strike").and_then(Value::as_f64)?;
    let width = (short - long).abs();
    (width > 0.0).then_some(width)
}

fn leg_quotes(chain: &broker_core::OptionChain, position: &Value) -> LegQuotes {
    let short = position["short_symbol"].as_str().unwrap_or("");
    let long = position["long_symbol"].as_str().unwrap_or("");
    let short_ask = chain
        .contracts
        .iter()
        .find(|contract| contract.symbol == short)
        .and_then(|contract| contract.ask);
    let long_bid = chain
        .contracts
        .iter()
        .find(|contract| contract.symbol == long)
        .and_then(|contract| contract.bid);
    LegQuotes {
        short_ask,
        long_bid,
    }
}

async fn strategy_quote_debit(
    data: &dyn MarketData,
    account: &AccountRef,
    position: &Value,
) -> Option<f64> {
    let short = position["short_symbol"].as_str()?;
    let long = position["long_symbol"].as_str()?;
    let legs = match close_intent(short, long, 1.0, 0.01) {
        broker_core::OrderIntent::Spread { legs, .. } => legs,
        _ => return None,
    };
    let quote = data.strategy_quote(account, &legs).await.ok()?;
    strategy_debit(&quote)
}

fn strategy_debit(quote: &Quote) -> Option<f64> {
    let raw = quote
        .last
        .filter(|value| value.abs() > 0.0)
        .or_else(|| quote.ask.filter(|value| *value > 0.0))
        .or_else(|| quote.bid.filter(|value| *value > 0.0))?;
    Some(raw.abs())
}

/// Flag booked closes whose debit or loss exceeds spread width. `pnl_usd` stays
/// the booked number. The returned delta turns booked realized P&L into the
/// width-capped (or excluded) clean total.
fn restate_closes(closes: &mut [Value]) -> (u64, f64) {
    let mut count = 0u64;
    let mut delta = 0.0;
    for close in closes.iter_mut() {
        if !close_is_suspect(close) {
            if close.get("mark_suspect").is_none() {
                close["mark_suspect"] = json!(false);
            }
            continue;
        }
        count += 1;
        let booked = close.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0);
        let replacement = width_capped_pnl(close).unwrap_or(0.0);
        close["mark_suspect"] = json!(true);
        close["clean_pnl_usd"] = json!(replacement);
        delta += replacement - booked;
    }
    (count, delta)
}

fn close_is_suspect(close: &Value) -> bool {
    if suspect_by_width(close) {
        return true;
    }
    close.get("mark_suspect").and_then(Value::as_bool) == Some(true)
}

fn suspect_by_width(close: &Value) -> bool {
    let Some(width) = close_width(close) else {
        return false;
    };
    let debit = close
        .get("debit")
        .and_then(Value::as_f64)
        .or_else(|| close.get("debit_to_close").and_then(Value::as_f64));
    if debit.is_some_and(|debit| debit > width + 1e-6) {
        return true;
    }
    let pnl = close.get("pnl_usd").and_then(Value::as_f64).unwrap_or(0.0);
    let contracts = close
        .get("contracts")
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
        .max(0.0);
    -pnl > width * 100.0 * contracts + 0.01
}

fn close_width(close: &Value) -> Option<f64> {
    if let Some(width) = close.get("width").and_then(Value::as_f64) {
        if width > 0.0 {
            return Some(width);
        }
    }
    position_width(close)
}

fn width_capped_pnl(close: &Value) -> Option<f64> {
    let width = close_width(close)?;
    let credit = close.get("entry_credit").and_then(Value::as_f64)?;
    let contracts = close
        .get("contracts")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    Some((credit - width) * 100.0 * contracts)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{ExitRules, PaperSetup, VerticalRules};
    use async_trait::async_trait;
    use broker_core::{Bar, Greeks, OptionChain, OptionContract, SessionStatus, SpreadLeg};

    struct Stub {
        short_ask: f64,
        long_bid: f64,
        strategy: Option<f64>,
    }

    #[async_trait]
    impl MarketData for Stub {
        fn broker_name(&self) -> &'static str {
            "public"
        }

        async fn quotes(&self, _: &AccountRef, _: &[String]) -> Result<Vec<Quote>> {
            Ok(vec![])
        }

        async fn option_expirations(&self, _: &AccountRef, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn option_chain(
            &self,
            _: &AccountRef,
            underlying: &str,
            expiration: &str,
        ) -> Result<OptionChain> {
            Ok(OptionChain {
                underlying: underlying.into(),
                expirations: vec![expiration.into()],
                contracts: vec![
                    OptionContract {
                        symbol: "SHORT".into(),
                        expiration: expiration.into(),
                        strike: 500.0,
                        put_call: "P".into(),
                        bid: Some(1.0),
                        ask: Some(self.short_ask),
                        open_interest: Some(1.0),
                        greeks: Greeks::default(),
                    },
                    OptionContract {
                        symbol: "LONG".into(),
                        expiration: expiration.into(),
                        strike: 495.0,
                        put_call: "P".into(),
                        bid: Some(self.long_bid),
                        ask: Some(0.2),
                        open_interest: Some(1.0),
                        greeks: Greeks::default(),
                    },
                ],
            })
        }

        async fn greeks(&self, _: &AccountRef, _: &str) -> Result<Greeks> {
            Ok(Greeks::default())
        }

        async fn strategy_quote(&self, _: &AccountRef, _: &[SpreadLeg]) -> Result<Quote> {
            let debit = self.strategy.context("strategy quote missing")?;
            Ok(Quote {
                symbol: "strategy".into(),
                bid: Some(debit),
                ask: Some(debit),
                last: Some(debit),
                bid_size: None,
                ask_size: None,
                volume: None,
            })
        }

        async fn bars(&self, _: &AccountRef, _: &str, _: &str) -> Result<Vec<Bar>> {
            Ok(vec![])
        }

        async fn session_open(&self, _: &AccountRef) -> Result<SessionStatus> {
            Ok(SessionStatus {
                open: true,
                source: "test".into(),
            })
        }
    }

    fn setup() -> PaperSetup {
        PaperSetup {
            watchlist: vec![],
            vertical: VerticalRules::default(),
            exits: ExitRules::default(),
            max_open: 1,
        }
    }

    fn position() -> Value {
        let expiry = (Utc::now().date_naive() + chrono::Duration::days(40))
            .format("%Y-%m-%d")
            .to_string();
        json!({
            "underlying": "QQQ",
            "expiry": expiry,
            "contracts": 1.0,
            "entry_credit": 1.0,
            "debit_to_close": 1.0,
            "unrealized_usd": 0.0,
            "max_loss_usd": 400.0,
            "short_symbol": "SHORT",
            "long_symbol": "LONG",
            "short_strike": 500.0,
            "long_strike": 495.0
        })
    }

    async fn tick_position(stub: Stub, book: Value) -> Value {
        let dir = std::env::temp_dir().join(format!(
            "agent-trader-paper-{}-{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("book.json");
        std::fs::write(&path, book.to_string()).unwrap();
        let account = AccountRef::new(BrokerKind::Public, "paper-test");
        tick_once(&stub, &account, &setup(), &path, true)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn debit_above_width_is_not_booked() {
        let book = tick_position(
            Stub {
                short_ask: 7.0,
                long_bid: 0.16,
                strategy: Some(6.84),
            },
            json!({
                "cash_usd": 9600.0,
                "realized_pnl_usd": 0.0,
                "positions": [position()],
                "recent_closes": [],
                "journal": []
            }),
        )
        .await;
        assert_eq!(book["open_count"], 1);
        assert_eq!(book["closed_count"], 0);
        assert_eq!(book["positions"][0]["quote_degraded"], true);
        assert_eq!(book["positions"][0]["debit_to_close"], 1.0);
        assert_eq!(book["realized_pnl_usd"], 0.0);
    }

    #[tokio::test]
    async fn zero_bid_holds() {
        let book = tick_position(
            Stub {
                short_ask: 3.0,
                long_bid: 0.0,
                strategy: None,
            },
            json!({
                "cash_usd": 9600.0,
                "realized_pnl_usd": 0.0,
                "positions": [position()],
                "recent_closes": [],
                "journal": []
            }),
        )
        .await;
        assert_eq!(book["open_count"], 1);
        assert_eq!(book["closed_count"], 0);
        assert_eq!(book["positions"][0]["quote_degraded"], true);
        assert_eq!(book["positions"][0]["debit_to_close"], 1.0);
    }

    #[tokio::test]
    async fn strategy_quote_inside_width_is_the_debit_used() {
        let book = tick_position(
            Stub {
                short_ask: 2.2,
                long_bid: 0.2,
                strategy: Some(0.55),
            },
            json!({
                "cash_usd": 9600.0,
                "realized_pnl_usd": 0.0,
                "positions": [position()],
                "recent_closes": [],
                "journal": []
            }),
        )
        .await;
        assert_eq!(book["open_count"], 1);
        assert_eq!(book["positions"][0]["quote_degraded"], false);
        assert_eq!(book["positions"][0]["debit_to_close"], 0.55);
    }

    #[test]
    fn historical_close_wider_than_the_spread_is_flagged_and_excluded_from_clean_pnl() {
        let mut closes = vec![json!({
            "underlying": "QQQ",
            "pnl_usd": -584.0,
            "entry_credit": 1.0,
            "debit": 6.84,
            "width": 5.0,
            "contracts": 1.0
        })];
        let (count, delta) = restate_closes(&mut closes);
        assert_eq!(count, 1);
        assert_eq!(closes[0]["mark_suspect"], true);
        assert_eq!(closes[0]["pnl_usd"], -584.0);
        assert_eq!(closes[0]["clean_pnl_usd"], -400.0);
        let realized = -1542.0;
        let clean = realized + delta;
        assert!((clean - (-1358.0)).abs() < 1e-9);
    }
}
