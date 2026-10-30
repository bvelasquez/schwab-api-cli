//! The paper book for the research arm: signals → positions → marks → P&L.
//!
//! Plan §7 (fill model). Everything here is local simulation; the broker handle
//! this arm holds is a [`crate::PaperOnlyBroker`], so no order can leave.
//!
//! Rules:
//! - Long only. A `down`/`flat`/missing signal is not a trade, it is a reason to
//!   drop a held position ("thesis" exit). Shorts stay out of scope (plan §1).
//! - Entry fills use the ask (or last), exits use the bid (or last), and
//!   `slippage_bps` is applied against us both ways.
//! - Indicators see only bars strictly before the decision date
//!   ([`crate::indicators::before`]); no future bars.
//! - Stops/targets/trailing/RSI all come from the playbook, which is the only
//!   thing the patch engine may change.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use broker_core::{Bar, Quote};
use chrono::{Datelike, NaiveDate, Utc, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::ArmConfig;
use crate::forecast::Forecast;
use crate::indicators;
use crate::playbook::Playbook;

pub const MAX_JOURNAL: usize = 60;
pub const MAX_CLOSES: usize = 40;
pub const MAX_SERIES: usize = 240;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PaperBook {
    pub arm_id: String,
    pub started: bool,
    pub starting_equity_usd: f64,
    pub cash_usd: f64,
    pub realized_pnl_usd: f64,
    pub wins: u64,
    pub closed_count: u64,
    pub tick_count: u64,
    pub seq: u64,
    pub positions: Vec<PaperPosition>,
    pub recent_closes: Vec<ClosedTrade>,
    pub journal: Vec<JournalNote>,
    pub pnl_series: Vec<PnlPoint>,
    pub equity_peak_usd: f64,
    pub halted: Option<Halt>,
    pub last_tick: Option<String>,
    pub benchmark_symbol: String,
    pub benchmark_first: Option<f64>,
    pub benchmark_last: Option<f64>,
    pub last_regime: Option<String>,
    /// Symbol -> date before which re-entry is blocked after a stop/thesis exit.
    pub entry_cooldowns: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PaperPosition {
    pub id: String,
    pub symbol: String,
    pub quantity: f64,
    pub entry_price: f64,
    pub last_price: f64,
    pub stop_price: f64,
    pub profit_limit: f64,
    pub peak_price: f64,
    pub peak_profit_pct: f64,
    pub atr_at_entry: Option<f64>,
    pub trail_active: bool,
    pub opened_at: String,
    pub exit_due_date: String,
    pub entry_confidence: f64,
    pub entry_expected_return_pct: f64,
    pub entry_rank: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClosedTrade {
    pub closed_at: String,
    pub symbol: String,
    pub quantity: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl_usd: f64,
    pub pnl_pct: f64,
    pub exit_reason: String,
    pub hold_days: i64,
    pub entry_confidence: f64,
    /// Exchange date of the close, used for the daily-loss bucket.
    #[serde(default)]
    pub closed_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JournalNote {
    pub ts: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PnlPoint {
    pub at: String,
    pub equity_usd: f64,
    pub realized_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Halt {
    /// US Eastern exchange date the halt applies to. Cleared the next day.
    pub date: String,
    pub reason: String,
    pub realized_today_usd: f64,
}

pub struct TickInputs<'a> {
    pub config: &'a ArmConfig,
    pub playbook: &'a Playbook,
    pub quotes: &'a [Quote],
    pub bars: &'a BTreeMap<String, Vec<Bar>>,
    pub forecast: Option<&'a Forecast>,
    pub benchmark_symbol: &'a str,
    pub benchmark_last: Option<f64>,
    pub now: chrono::DateTime<Utc>,
    pub session_open: bool,
}

#[derive(Debug, Default)]
pub struct TickOutcome {
    pub entries: Vec<String>,
    pub exits: Vec<String>,
    pub notes: Vec<String>,
    pub halted: bool,
}

fn note(kind: &str, detail: impl Into<String>) -> JournalNote {
    JournalNote {
        ts: Utc::now().to_rfc3339(),
        kind: kind.into(),
        detail: detail.into(),
    }
}

/// Add `days` business days. Weekends only: the research arm has no exchange
/// holiday calendar, and every consumer is a paper threshold, not a settlement.
pub fn add_trading_days(start: NaiveDate, days: u32) -> NaiveDate {
    let mut date = start;
    let mut added = 0;
    while added < days {
        date = date.succ_opt().unwrap_or(date);
        if date.weekday().num_days_from_monday() < Weekday::Sat.num_days_from_monday() {
            added += 1;
        }
    }
    date
}

fn mid(quote: &Quote) -> Option<f64> {
    match (quote.bid, quote.ask) {
        (Some(bid), Some(ask)) if bid > 0.0 && ask > 0.0 => Some((bid + ask) / 2.0),
        _ => None,
    }
}

/// Price to pay when buying: ask, else last, else mid, plus slippage.
pub fn buy_price(quote: &Quote, slippage_bps: f64) -> Option<f64> {
    let price = quote.ask.or(quote.last).or_else(|| mid(quote))?;
    (price > 0.0).then(|| price * (1.0 + slippage_bps / 10_000.0))
}

/// Price received when selling: bid, else last, else mid, minus slippage.
pub fn sell_price(quote: &Quote, slippage_bps: f64) -> Option<f64> {
    let price = quote.bid.or(quote.last).or_else(|| mid(quote))?;
    (price > 0.0).then(|| price * (1.0 - slippage_bps / 10_000.0))
}

/// Mark price for a held position: last, else mid, else bid.
pub fn mark_price(quote: &Quote) -> Option<f64> {
    quote
        .last
        .or_else(|| mid(quote))
        .or(quote.bid)
        .filter(|price| *price > 0.0)
}

impl PaperBook {
    pub fn new(arm_id: &str, starting_equity_usd: f64) -> Self {
        Self {
            arm_id: arm_id.to_string(),
            started: true,
            starting_equity_usd,
            cash_usd: starting_equity_usd,
            ..Default::default()
        }
    }

    /// Missing or unreadable book files start a fresh book. A corrupt file is
    /// reported by the caller (the export path is versioned by the repo).
    pub fn load(path: &Path, arm_id: &str, starting_equity_usd: f64) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            let mut book = Self::new(arm_id, starting_equity_usd);
            book.started = false;
            return book;
        };
        let Ok(book) = serde_json::from_str::<PaperBook>(&raw) else {
            let mut book = Self::new(arm_id, starting_equity_usd);
            book.started = false;
            return book;
        };
        book
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let pretty = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &pretty).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
        Ok(())
    }

    pub fn market_value(&self) -> f64 {
        self.positions
            .iter()
            .map(|position| position.last_price * position.quantity)
            .sum()
    }

    pub fn unrealized(&self) -> f64 {
        self.positions
            .iter()
            .map(|position| (position.last_price - position.entry_price) * position.quantity)
            .sum()
    }

    pub fn equity(&self) -> f64 {
        self.cash_usd + self.market_value()
    }

    pub fn return_pct(&self) -> f64 {
        if self.starting_equity_usd <= 0.0 {
            return 0.0;
        }
        (self.equity() / self.starting_equity_usd - 1.0) * 100.0
    }

    pub fn benchmark_return_pct(&self) -> Option<f64> {
        match (self.benchmark_first, self.benchmark_last) {
            (Some(first), Some(last)) if first > 0.0 => Some((last / first - 1.0) * 100.0),
            _ => None,
        }
    }

    pub fn excess_return_pct(&self) -> Option<f64> {
        self.benchmark_return_pct()
            .map(|bench| self.return_pct() - bench)
    }

    /// One paper tick. Never places an order; only reads quotes and bars.
    pub fn tick(&mut self, input: &TickInputs<'_>) -> TickOutcome {
        let mut outcome = TickOutcome::default();
        // Exchange date, so buckets and cooldowns line up with daily bar dates.
        let today = indicators::exchange_date(input.now);
        let today_key = today.to_string();
        let slippage = input.config.paper.slippage_bps.clamp(0.0, 100.0);
        let max_open = input.config.paper.max_open_positions.max(1) as usize;

        if !self.started || self.starting_equity_usd <= 0.0 {
            self.arm_id = input.config.arm_id.clone();
            self.starting_equity_usd = input.config.paper.starting_equity_usd.max(1.0);
            self.cash_usd = self.starting_equity_usd;
            self.started = true;
        }
        if let Some(halt) = &self.halted {
            if halt.date != today_key {
                self.halted = None;
            }
        }
        self.entry_cooldowns.retain(|_, until| *until > today_key);

        let quote_map: BTreeMap<String, &Quote> = input
            .quotes
            .iter()
            .map(|quote| (quote.symbol.to_ascii_uppercase(), quote))
            .collect();

        // --- marks, trailing stops, exits ---
        let mut remaining = Vec::with_capacity(self.positions.len());
        for mut position in std::mem::take(&mut self.positions) {
            let Some(quote) = quote_map.get(&position.symbol.to_ascii_uppercase()) else {
                self.journal.push(note(
                    "paper",
                    format!(
                        "{} has no Public quote; position left open",
                        position.symbol
                    ),
                ));
                remaining.push(position);
                continue;
            };
            let Some(mark) = mark_price(quote) else {
                self.journal.push(note(
                    "paper",
                    format!(
                        "{} quote has no usable price; position left open",
                        position.symbol
                    ),
                ));
                remaining.push(position);
                continue;
            };
            position.last_price = mark;
            if mark > position.peak_price {
                position.peak_price = mark;
            }
            position.peak_profit_pct = if position.entry_price > 0.0 {
                (position.peak_price - position.entry_price) / position.entry_price * 100.0
            } else {
                0.0
            };

            // ATR trailing ratchet: only ever moves the stop up.
            let trailing = &input.playbook.exit.trailing;
            if position.peak_profit_pct >= trailing.activate_after_profit_pct {
                let atr = input
                    .bars
                    .get(&position.symbol.to_ascii_uppercase())
                    .and_then(|bars| indicators::atr_before(bars, today, 14))
                    .or(position.atr_at_entry);
                if let Some(atr) = atr.filter(|value| *value > 0.0) {
                    let candidate = position.peak_price - trailing.trail_atr_multiple * atr;
                    if candidate > position.stop_price {
                        position.stop_price = candidate;
                        position.trail_active = true;
                    }
                }
            }

            let reason = self.exit_reason(&position, input, today);
            let Some(reason) = reason else {
                remaining.push(position);
                continue;
            };
            let Some(exit) = sell_price(quote, slippage) else {
                remaining.push(position);
                continue;
            };
            let pnl = (exit - position.entry_price) * position.quantity;
            self.cash_usd += exit * position.quantity;
            self.realized_pnl_usd += pnl;
            self.closed_count += 1;
            if pnl > 0.0 {
                self.wins += 1;
            }
            let hold_days = chrono::DateTime::parse_from_rfc3339(&position.opened_at)
                .map(|opened| (input.now - opened.with_timezone(&Utc)).num_days().max(0))
                .unwrap_or(0);
            self.recent_closes.push(ClosedTrade {
                closed_at: input.now.to_rfc3339(),
                symbol: position.symbol.clone(),
                quantity: position.quantity,
                entry_price: position.entry_price,
                exit_price: exit,
                pnl_usd: pnl,
                pnl_pct: if position.entry_price > 0.0 {
                    (exit - position.entry_price) / position.entry_price * 100.0
                } else {
                    0.0
                },
                exit_reason: reason.to_string(),
                hold_days,
                entry_confidence: position.entry_confidence,
                closed_date: today_key.clone(),
            });
            if reason == "stop" || reason == "thesis" {
                // Do not immediately re-buy a name that just broke the plan.
                self.entry_cooldowns.insert(
                    position.symbol.to_ascii_uppercase(),
                    add_trading_days(today, 1).to_string(),
                );
            }
            outcome.exits.push(position.symbol.clone());
            let detail = format!(
                "paper close {} {:.0} @ {:.2} vs {:.2} entry: {:+.2} ({reason}). No order sent.",
                position.symbol, position.quantity, exit, position.entry_price, pnl
            );
            self.journal.push(note("paper", detail.clone()));
            outcome.notes.push(detail);
        }
        self.positions = remaining;

        // --- daily loss kill switch ---
        let realized_today: f64 = self
            .recent_closes
            .iter()
            .filter(|trade| trade.closed_date == today_key)
            .map(|trade| trade.pnl_usd)
            .sum();
        let max_daily_loss = input.config.paper.max_daily_loss_usd;
        if max_daily_loss > 0.0 && realized_today <= -max_daily_loss && self.halted.is_none() {
            let reason = format!(
                "daily realized loss {realized_today:.2} reached the {:.2} cap; no new entries today",
                -max_daily_loss
            );
            self.halted = Some(Halt {
                date: today_key.clone(),
                reason: reason.clone(),
                realized_today_usd: realized_today,
            });
            self.journal.push(note("halt", reason.clone()));
            outcome.notes.push(reason);
        }
        outcome.halted = self.halted.is_some();

        // --- entries ---
        if self.halted.is_some() {
            self.journal
                .push(note("paper", "entries skipped: daily loss cap is in force"));
        } else if !input.session_open {
            self.journal.push(note(
                "paper",
                "entries skipped: US equity session is closed",
            ));
        } else if let Some(forecast) = input.forecast {
            let min_confidence = input.config.forecast.min_confidence;
            let mut candidates: Vec<&crate::forecast::Signal> = forecast
                .signals
                .iter()
                .filter(|signal| {
                    signal.direction.eq_ignore_ascii_case("up")
                        && signal.confidence >= min_confidence
                        && input
                            .config
                            .universe
                            .iter()
                            .any(|symbol| symbol.eq_ignore_ascii_case(&signal.symbol))
                })
                .collect();
            candidates.sort_by(|a, b| {
                b.confidence
                    .total_cmp(&a.confidence)
                    .then(b.expected_return_pct.total_cmp(&a.expected_return_pct))
                    .then(a.symbol.cmp(&b.symbol))
            });

            for signal in candidates {
                if self.positions.len() >= max_open {
                    self.journal.push(note(
                        "paper",
                        format!(
                            "{} skipped: {} of {} slots already filled",
                            signal.symbol,
                            self.positions.len(),
                            max_open
                        ),
                    ));
                    break;
                }
                let symbol = signal.symbol.to_ascii_uppercase();
                if self
                    .positions
                    .iter()
                    .any(|position| position.symbol.eq_ignore_ascii_case(&symbol))
                {
                    continue;
                }
                if self
                    .entry_cooldowns
                    .get(&symbol)
                    .is_some_and(|until| *until > today_key)
                {
                    self.journal.push(note(
                        "paper",
                        format!(
                            "{symbol} skipped: re-entry blocked after an exit until {today_key}"
                        ),
                    ));
                    continue;
                }
                let Some(quote) = quote_map.get(&symbol) else {
                    self.journal
                        .push(note("paper", format!("{symbol} skipped: no quote")));
                    continue;
                };
                let Some(fill) = buy_price(quote, slippage) else {
                    self.journal
                        .push(note("paper", format!("{symbol} skipped: no usable price")));
                    continue;
                };

                // RSI entry guard from the playbook band (overbought / falling knife).
                let [rsi_low, rsi_high] = input.playbook.entry.rsi_14_range;
                if let Some(rsi) = input
                    .bars
                    .get(&symbol)
                    .and_then(|bars| indicators::rsi_before(bars, today, 14))
                {
                    if rsi > rsi_high {
                        self.journal.push(note(
                            "paper",
                            format!("{symbol} skipped: RSI(14) {rsi:.1} is above the {rsi_high:.0} band"),
                        ));
                        continue;
                    }
                    if rsi < rsi_low {
                        self.journal.push(note(
                            "paper",
                            format!(
                                "{symbol} skipped: RSI(14) {rsi:.1} is below the {rsi_low:.0} band"
                            ),
                        ));
                        continue;
                    }
                }

                let slot = (self.equity() / max_open as f64).max(0.0);
                let budget = slot.min(self.cash_usd);
                let quantity = (budget / fill).floor();
                if quantity < 1.0 {
                    self.journal.push(note(
                        "paper",
                        format!(
                            "{symbol} skipped: {:.2} budget cannot buy a share at {:.2}",
                            budget, fill
                        ),
                    ));
                    continue;
                }
                let cost = quantity * fill;
                self.cash_usd -= cost;
                self.seq += 1;
                let stop = fill * (1.0 - input.playbook.exit.stop_loss_pct / 100.0);
                let target = fill * (1.0 + input.playbook.exit.profit_target_pct / 100.0);
                let atr = input
                    .bars
                    .get(&symbol)
                    .and_then(|bars| indicators::atr_before(bars, today, 14));
                let exit_due = add_trading_days(today, input.config.paper.max_hold_days);
                self.positions.push(PaperPosition {
                    id: format!("{}#{}", symbol, self.seq),
                    symbol: symbol.clone(),
                    quantity,
                    entry_price: fill,
                    last_price: fill,
                    stop_price: stop,
                    profit_limit: target,
                    peak_price: fill,
                    peak_profit_pct: 0.0,
                    atr_at_entry: atr,
                    trail_active: false,
                    opened_at: input.now.to_rfc3339(),
                    exit_due_date: exit_due.to_string(),
                    entry_confidence: signal.confidence,
                    entry_expected_return_pct: signal.expected_return_pct,
                    entry_rank: signal.rank,
                });
                outcome.entries.push(symbol.clone());
                let detail = format!(
                    "paper open {symbol} {quantity:.0} @ {fill:.2} stop {stop:.2} target {target:.2} \
                     due {exit_due} (conf {:.2}, expected {:+.2}%). No order sent.",
                    signal.confidence, signal.expected_return_pct
                );
                self.journal.push(note("paper", detail.clone()));
                outcome.notes.push(detail);
            }
        } else {
            self.journal.push(note(
                "paper",
                "entries skipped: no forecast this tick (LLM/search unavailable or unparsed)",
            ));
        }

        // Mark fresh entries against the same quotes so the spread cost shows up
        // immediately instead of waiting for the next tick.
        for position in &mut self.positions {
            if let Some(quote) = quote_map.get(&position.symbol.to_ascii_uppercase()) {
                if let Some(mark) = mark_price(quote) {
                    position.last_price = mark;
                }
            }
        }

        // --- bookkeeping ---
        self.tick_count += 1;
        self.last_tick = Some(input.now.to_rfc3339());
        if let Some(forecast) = input.forecast {
            if !forecast.regime.is_empty() {
                self.last_regime = Some(forecast.regime.clone());
            }
        }
        if self.benchmark_symbol.is_empty() {
            self.benchmark_symbol = input.benchmark_symbol.to_string();
        }
        if let Some(last) = input.benchmark_last.filter(|value| *value > 0.0) {
            self.benchmark_last = Some(last);
            if self.benchmark_first.is_none() {
                self.benchmark_first = Some(last);
            }
        }
        let equity = self.equity();
        if equity > self.equity_peak_usd {
            self.equity_peak_usd = equity;
        }
        self.pnl_series.push(PnlPoint {
            at: input.now.to_rfc3339(),
            equity_usd: equity,
            realized_usd: self.realized_pnl_usd,
        });
        if self.pnl_series.len() > MAX_SERIES {
            self.pnl_series = self
                .pnl_series
                .split_off(self.pnl_series.len() - MAX_SERIES);
        }
        if self.journal.len() > MAX_JOURNAL {
            self.journal = self.journal.split_off(self.journal.len() - MAX_JOURNAL);
        }
        if self.recent_closes.len() > MAX_CLOSES {
            self.recent_closes = self
                .recent_closes
                .split_off(self.recent_closes.len() - MAX_CLOSES);
        }
        outcome
    }

    fn exit_reason(
        &self,
        position: &PaperPosition,
        input: &TickInputs<'_>,
        today: NaiveDate,
    ) -> Option<&'static str> {
        if position.entry_price <= 0.0 || position.last_price <= 0.0 {
            return None;
        }
        if position.stop_price > 0.0 && position.last_price <= position.stop_price {
            return Some("stop");
        }
        if position.profit_limit > 0.0 && position.last_price >= position.profit_limit {
            return Some("target");
        }
        if position
            .exit_due_date
            .parse::<NaiveDate>()
            .is_ok_and(|due| today >= due)
        {
            return Some("time");
        }
        // Research thesis: the signal that opened the trade is gone or flipped.
        if let Some(forecast) = input.forecast {
            let still_up = forecast.signals.iter().any(|signal| {
                signal.symbol.eq_ignore_ascii_case(&position.symbol)
                    && signal.direction.eq_ignore_ascii_case("up")
                    && signal.confidence >= input.config.forecast.min_confidence
            });
            if !still_up {
                return Some("thesis");
            }
        }
        None
    }

    /// The dashboard book shape (`kind: research`) plus the research extras.
    pub fn to_dashboard_json(
        &self,
        config: &ArmConfig,
        playbook: &Playbook,
        scorecard: Option<&Value>,
        forecast: Option<&Forecast>,
    ) -> Value {
        let equity = self.equity();
        let unrealized = self.unrealized();
        let win_rate = if self.closed_count == 0 {
            0.0
        } else {
            self.wins as f64 * 100.0 / self.closed_count as f64
        };
        let positions: Vec<Value> = self
            .positions
            .iter()
            .map(|position| {
                json!({
                    "id": position.id,
                    "symbol": position.symbol,
                    "quantity": position.quantity,
                    "entry_price": position.entry_price,
                    "last_price": position.last_price,
                    "stop_price": position.stop_price,
                    "profit_limit": position.profit_limit,
                    "unrealized_usd": (position.last_price - position.entry_price) * position.quantity,
                    "stop_risk_usd": (position.entry_price - position.stop_price) * position.quantity,
                    "peak_profit_pct": position.peak_profit_pct,
                    "trail_active": position.trail_active,
                    "atr_at_entry": position.atr_at_entry,
                    "opened_at": position.opened_at,
                    "exit_due_date": position.exit_due_date,
                    "entry_confidence": position.entry_confidence,
                    "entry_expected_return_pct": position.entry_expected_return_pct,
                    "entry_rank": position.entry_rank,
                })
            })
            .collect();
        let forecast_json: Option<Value> = forecast.map(|forecast| {
            let signals: Vec<Value> = forecast
                .signals
                .iter()
                .map(|signal| {
                    json!({
                        "symbol": signal.symbol,
                        "direction": signal.direction,
                        "confidence": signal.confidence,
                        "expected_return_pct": signal.expected_return_pct,
                    })
                })
                .collect();
            json!({
                "as_of": forecast.as_of,
                "target": forecast.target,
                "regime": forecast.regime,
                "signals": signals,
                "risk_alerts": forecast.risk_alerts,
                "notes": forecast.notes,
            })
        });
        json!({
            "stack": "agent-trader",
            "broker": "public",
            "broker_label": "Public API",
            "kind": "research",
            "name": self.arm_id,
            "rules_file": format!("rules/research/{}.yaml", self.arm_id),
            "profile": "research",
            "mode": "paper",
            "live_orders": false,
            "note": "Research arm: local paper fills from Public quotes. This book never submits an order.",
            "last_tick": self.last_tick,
            "tick_count": self.tick_count,
            "starting_cash_usd": self.starting_equity_usd,
            "sleeve_cap_usd": self.starting_equity_usd,
            "cash_usd": self.cash_usd,
            "equity_usd": equity,
            "unrealized_pnl_usd": unrealized,
            "realized_pnl_usd": self.realized_pnl_usd,
            "return_pct": self.return_pct(),
            "excess_return_pct": self.excess_return_pct(),
            "open_count": self.positions.len(),
            "closed_count": self.closed_count,
            "wins": self.wins,
            "win_rate_pct": win_rate,
            "halted": self.halted.as_ref().map(|halt| halt.reason.clone()),
            "positions": positions,
            "recent_closes": self.recent_closes,
            "journal": self.journal,
            "pnl_series": self.pnl_series,
            "plan": {
                "stop_loss_pct": playbook.exit.stop_loss_pct,
                "profit_target_pct": playbook.exit.profit_target_pct,
                "time_stop_days": config.paper.max_hold_days,
                "max_hold_days": config.paper.max_hold_days,
                "max_open_positions": config.paper.max_open_positions,
                "min_confidence": config.forecast.min_confidence,
                "trail_activate_pct": playbook.exit.trailing.activate_after_profit_pct,
                "trail_atr_multiple": playbook.exit.trailing.trail_atr_multiple,
                "rsi_14_range": playbook.entry.rsi_14_range,
            },
            "benchmark": {
                "symbol": if self.benchmark_symbol.is_empty() { config.forecast.benchmark.clone() } else { self.benchmark_symbol.clone() },
                "first_close": self.benchmark_first,
                "last_close": self.benchmark_last,
                "return_pct": self.benchmark_return_pct(),
            },
            "regime": self.last_regime,
            "scorecard": scorecard,
            "forecast": forecast_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ArmConfig;
    use crate::forecast::{Forecast, Signal};
    use crate::playbook::Playbook;
    use serde_json::json;

    fn quote(symbol: &str, bid: f64, ask: f64, last: f64) -> Quote {
        Quote {
            symbol: symbol.into(),
            bid: Some(bid),
            ask: Some(ask),
            last: Some(last),
            bid_size: Some(100.0),
            ask_size: Some(100.0),
            volume: Some(1_000.0),
        }
    }

    fn bar(date: &str, close: f64) -> Bar {
        Bar {
            timestamp: date.to_string(),
            open: close,
            high: close * 1.01,
            low: close * 0.99,
            close,
            volume: 1_000.0,
        }
    }

    fn rt<'a>(rfc: &str) -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(rfc)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn config() -> ArmConfig {
        let mut config = ArmConfig {
            arm_id: "research-test".into(),
            account: Some("public:ACC".into()),
            universe: vec!["MSFT".into(), "AAPL".into()],
            ..Default::default()
        };
        config.paper.starting_equity_usd = 10_000.0;
        config.paper.slippage_bps = 0.0;
        config.paper.max_open_positions = 2;
        config.paper.max_hold_days = 5;
        config.paper.max_daily_loss_usd = 300.0;
        config.forecast.min_confidence = 0.5;
        config
    }

    fn forecast(signals: Vec<(&str, &str, f64)>) -> Forecast {
        Forecast {
            arm_id: "research-test".into(),
            as_of: "2026-10-29T14:00:00Z".into(),
            target: crate::config::EXPECTED_TARGET.into(),
            benchmark: "SPY".into(),
            horizon_days: 5,
            signals: signals
                .into_iter()
                .map(|(symbol, direction, confidence)| Signal {
                    symbol: symbol.into(),
                    direction: direction.into(),
                    expected_return_pct: 1.0,
                    confidence,
                    rank: 1,
                    citations: vec![],
                })
                .collect(),
            regime: "risk_on".into(),
            plan_patch: vec![],
            risk_alerts: vec![],
            notes: String::new(),
        }
    }

    fn inputs<'a>(
        config: &'a ArmConfig,
        playbook: &'a Playbook,
        quotes: &'a [Quote],
        bars: &'a BTreeMap<String, Vec<Bar>>,
        forecast: Option<&'a Forecast>,
        now: chrono::DateTime<Utc>,
        session_open: bool,
    ) -> TickInputs<'a> {
        TickInputs {
            config,
            playbook,
            quotes,
            bars,
            forecast,
            benchmark_symbol: "SPY",
            benchmark_last: Some(100.0),
            now,
            session_open,
        }
    }

    #[test]
    fn first_tick_buys_the_top_ranked_signal_with_slippage() {
        let config = config();
        let playbook = Playbook::default();
        let quotes = vec![quote("MSFT", 99.9, 100.1, 100.0)];
        let bars = BTreeMap::new();
        let forecast = forecast(vec![("MSFT", "up", 0.8)]);
        let mut book = PaperBook::new("research-test", 10_000.0);
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &quotes,
            &bars,
            Some(&forecast),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        assert_eq!(outcome.entries, vec!["MSFT".to_string()]);
        let position = &book.positions[0];
        // $10k over 2 slots = $5k budget / 100.10 ask = 49 whole shares.
        assert_eq!(position.quantity, 49.0);
        assert!((position.entry_price - 100.1).abs() < 1e-9);
        assert!(book.cash_usd > 5_000.0 && book.cash_usd < 10_000.0);
        // Marked to the same last (100.00 vs the 100.10 ask): the half spread is real.
        assert!(
            (book.equity() - 9_995.1).abs() < 1e-6,
            "equity {}",
            book.equity()
        );
    }

    #[test]
    fn closed_session_blocks_entries_but_not_marks() {
        let config = config();
        let playbook = Playbook::default();
        let quotes = vec![quote("MSFT", 99.9, 100.1, 100.0)];
        let bars = BTreeMap::new();
        let forecast = forecast(vec![("MSFT", "up", 0.8)]);
        let mut book = PaperBook::new("research-test", 10_000.0);
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &quotes,
            &bars,
            Some(&forecast),
            rt("2026-10-29T22:00:00Z"),
            false,
        ));
        assert!(outcome.entries.is_empty());
        assert!(book.positions.is_empty());
        assert!(book
            .journal
            .iter()
            .any(|note| note.detail.contains("session is closed")));
    }

    #[test]
    fn stop_exit_books_the_loss_and_frees_cash() {
        let config = config();
        let playbook = Playbook::default();
        let mut book = PaperBook::new("research-test", 10_000.0);
        let open_quotes = vec![quote("MSFT", 99.9, 100.1, 100.0)];
        let bars = BTreeMap::new();
        let forecast = forecast(vec![("MSFT", "up", 0.8)]);
        book.tick(&inputs(
            &config,
            &playbook,
            &open_quotes,
            &bars,
            Some(&forecast),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        let cash_after_entry = book.cash_usd;
        // 4% stop from 100.1 fills at 96.096 or below.
        let crashed = vec![quote("MSFT", 95.0, 95.2, 95.1)];
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &crashed,
            &bars,
            Some(&forecast),
            rt("2026-10-29T15:00:00Z"),
            true,
        ));
        assert_eq!(outcome.exits, vec!["MSFT".to_string()]);
        assert!(book.positions.is_empty());
        assert!(book.cash_usd > cash_after_entry);
        assert!(book.entry_cooldowns.contains_key("MSFT"));
        assert!(book.realized_pnl_usd < 0.0);
        assert_eq!(book.recent_closes[0].exit_reason, "stop");
        assert_eq!(book.closed_count, 1);
    }

    #[test]
    fn target_exit_books_the_win() {
        let config = config();
        let playbook = Playbook::default();
        let mut book = PaperBook::new("research-test", 10_000.0);
        let bars = BTreeMap::new();
        let forecast = forecast(vec![("MSFT", "up", 0.9)]);
        book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&forecast),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        let spiked = vec![quote("MSFT", 109.0, 109.2, 109.1)];
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &spiked,
            &bars,
            Some(&forecast),
            rt("2026-10-29T15:00:00Z"),
            true,
        ));
        assert_eq!(outcome.exits, vec!["MSFT".to_string()]);
        assert_eq!(book.recent_closes[0].exit_reason, "target");
        assert!(book.realized_pnl_usd > 0.0);
        assert_eq!(book.wins, 1);
    }

    #[test]
    fn dropped_signal_is_a_thesis_exit() {
        let config = config();
        let playbook = Playbook::default();
        let mut book = PaperBook::new("research-test", 10_000.0);
        let bars = BTreeMap::new();
        let up = forecast(vec![("MSFT", "up", 0.9)]);
        book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&up),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        let flipped = forecast(vec![("MSFT", "down", 0.9)]);
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&flipped),
            rt("2026-10-29T15:00:00Z"),
            true,
        ));
        assert_eq!(outcome.exits, vec!["MSFT".to_string()]);
        assert_eq!(book.recent_closes[0].exit_reason, "thesis");
    }

    #[test]
    fn max_open_caps_the_book() {
        let mut config = config();
        config.paper.max_open_positions = 1;
        let playbook = Playbook::default();
        let quotes = vec![
            quote("MSFT", 99.9, 100.1, 100.0),
            quote("AAPL", 199.9, 200.1, 200.0),
        ];
        let bars = BTreeMap::new();
        let forecast = forecast(vec![("MSFT", "up", 0.9), ("AAPL", "up", 0.8)]);
        let mut book = PaperBook::new("research-test", 10_000.0);
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &quotes,
            &bars,
            Some(&forecast),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        assert_eq!(outcome.entries.len(), 1);
        assert_eq!(book.positions.len(), 1);
    }

    #[test]
    fn low_confidence_signals_are_ignored() {
        let config = config();
        let playbook = Playbook::default();
        let quotes = vec![quote("MSFT", 99.9, 100.1, 100.0)];
        let bars = BTreeMap::new();
        let weak = forecast(vec![("MSFT", "up", 0.4)]);
        let mut book = PaperBook::new("research-test", 10_000.0);
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &quotes,
            &bars,
            Some(&weak),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        assert!(outcome.entries.is_empty());
    }

    #[test]
    fn no_forecast_means_no_entries_and_no_forced_exits() {
        let config = config();
        let playbook = Playbook::default();
        let mut book = PaperBook::new("research-test", 10_000.0);
        let bars = BTreeMap::new();
        let up = forecast(vec![("MSFT", "up", 0.9)]);
        book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&up),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            None,
            rt("2026-10-29T15:00:00Z"),
            true,
        ));
        assert!(outcome.entries.is_empty());
        assert!(outcome.exits.is_empty());
        assert_eq!(book.positions.len(), 1);
    }

    #[test]
    fn trailing_stop_ratchets_only_up() {
        let config = config();
        let mut playbook = Playbook::default();
        playbook.exit.trailing.activate_after_profit_pct = 1.0;
        playbook.exit.trailing.trail_atr_multiple = 2.0;
        let mut book = PaperBook::new("research-test", 10_000.0);
        let mut bars: BTreeMap<String, Vec<Bar>> = BTreeMap::new();
        let history: Vec<Bar> = (1..=20)
            .map(|day| bar(&format!("2026-10-{day:02}"), 100.0))
            .collect();
        bars.insert("MSFT".into(), history);
        let up = forecast(vec![("MSFT", "up", 0.9)]);
        book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&up),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        let stop_before = book.positions[0].stop_price;
        // +3% with ATR ~2 (high 101, low 99) => trail candidate ~106 - 4 = 102.
        book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 102.9, 103.1, 103.0)],
            &bars,
            Some(&up),
            rt("2026-10-30T14:00:00Z"),
            true,
        ));
        let position = &book.positions[0];
        assert!(position.trail_active);
        assert!(position.stop_price > stop_before);
    }

    #[test]
    fn daily_loss_cap_halts_new_entries() {
        let mut config = config();
        config.paper.max_daily_loss_usd = 100.0;
        config.paper.max_open_positions = 2;
        config.forecast.min_confidence = 0.1;
        let playbook = Playbook::default();
        let bars = BTreeMap::new();
        let mut book = PaperBook::new("research-test", 10_000.0);
        book.cash_usd = 10_000.0;
        let up = forecast(vec![("MSFT", "up", 0.9), ("AAPL", "up", 0.8)]);
        book.tick(&inputs(
            &config,
            &playbook,
            &[
                quote("MSFT", 99.9, 100.1, 100.0),
                quote("AAPL", 199.9, 200.1, 200.0),
            ],
            &bars,
            Some(&up),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        assert_eq!(book.positions.len(), 2);
        // Crush one position past the daily loss cap.
        book.tick(&inputs(
            &config,
            &playbook,
            &[
                quote("MSFT", 50.0, 50.1, 50.0),
                quote("AAPL", 199.9, 200.1, 200.0),
            ],
            &bars,
            Some(&up),
            rt("2026-10-29T15:00:00Z"),
            true,
        ));
        assert!(book.halted.is_some());
        // A third entry must be refused even with a free slot and a fresh signal.
        let cash_before = book.cash_usd;
        let extra = forecast(vec![("MSFT", "up", 0.9), ("AAPL", "up", 0.8)]);
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &[
                quote("MSFT", 50.0, 50.1, 50.0),
                quote("AAPL", 199.9, 200.1, 200.0),
            ],
            &bars,
            Some(&extra),
            rt("2026-10-29T16:00:00Z"),
            true,
        ));
        assert!(outcome.halted);
        assert_eq!(book.cash_usd, cash_before);
    }

    #[test]
    fn dashboard_shape_is_compatible_with_the_existing_book_rendering() {
        let config = config();
        let playbook = Playbook::default();
        let mut book = PaperBook::new("research-test", 10_000.0);
        let bars = BTreeMap::new();
        let up = forecast(vec![("MSFT", "up", 0.9)]);
        book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&up),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        let doc = book.to_dashboard_json(
            &config,
            &playbook,
            Some(&json!({"hit_rate_pct": 50.0})),
            Some(&up),
        );
        assert_eq!(doc["kind"], "research");
        assert_eq!(doc["stack"], "agent-trader");
        assert_eq!(doc["broker"], "public");
        assert_eq!(doc["live_orders"], false);
        assert!(doc["win_rate_pct"].is_number());
        assert!(doc["equity_usd"].is_number());
        assert_eq!(doc["positions"][0]["symbol"], "MSFT");
        assert!(doc["positions"][0]["stop_risk_usd"].is_number());
        assert_eq!(doc["plan"]["stop_loss_pct"], 4.0);
        assert_eq!(doc["scorecard"]["hit_rate_pct"], 50.0);
    }

    #[test]
    fn slippage_moves_fills_against_the_book() {
        let quote = quote("MSFT", 99.0, 101.0, 100.0);
        assert!((buy_price(&quote, 10.0).unwrap() - 101.101).abs() < 1e-9);
        assert!((sell_price(&quote, 10.0).unwrap() - 98.901).abs() < 1e-9);
        assert_eq!(mark_price(&quote), Some(100.0));
    }

    #[test]
    fn trading_day_math_skips_weekends() {
        let friday = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        assert_eq!(
            add_trading_days(friday, 1),
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()
        );
        assert_eq!(
            add_trading_days(friday, 5),
            NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()
        );
    }

    #[test]
    fn time_stop_exits_at_the_due_date() {
        let mut config = config();
        config.paper.max_hold_days = 1;
        let playbook = Playbook::default();
        let bars = BTreeMap::new();
        let up = forecast(vec![("MSFT", "up", 0.9)]);
        let mut book = PaperBook::new("research-test", 10_000.0);
        book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&up),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        assert_eq!(book.positions[0].exit_due_date, "2026-10-30");
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.5)],
            &bars,
            Some(&up),
            rt("2026-10-30T14:00:00Z"),
            true,
        ));
        assert_eq!(outcome.exits, vec!["MSFT".to_string()]);
        assert_eq!(book.recent_closes[0].exit_reason, "time");
    }

    #[test]
    fn rsi_guard_blocks_overbought_entries() {
        let config = config();
        let playbook = Playbook::default();
        // 20 rising closes: RSI is 100, above the 70 band.
        let bars: BTreeMap<String, Vec<Bar>> = [(
            "MSFT".to_string(),
            (1..=20)
                .map(|day| bar(&format!("2026-10-{day:02}"), 100.0 + day as f64))
                .collect(),
        )]
        .into_iter()
        .collect();
        let up = forecast(vec![("MSFT", "up", 0.9)]);
        let mut book = PaperBook::new("research-test", 10_000.0);
        let outcome = book.tick(&inputs(
            &config,
            &playbook,
            &[quote("MSFT", 99.9, 100.1, 100.0)],
            &bars,
            Some(&up),
            rt("2026-10-29T14:00:00Z"),
            true,
        ));
        assert!(outcome.entries.is_empty());
        assert!(book
            .journal
            .iter()
            .any(|note| note.detail.contains("RSI(14)")));
    }

    #[test]
    fn book_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.json");
        let mut book = PaperBook::new("research-test", 10_000.0);
        book.cash_usd = 9_000.0;
        book.save(&path).unwrap();
        let loaded = PaperBook::load(&path, "research-test", 10_000.0);
        assert_eq!(loaded.cash_usd, 9_000.0);
        assert!(loaded.started);
    }
}
