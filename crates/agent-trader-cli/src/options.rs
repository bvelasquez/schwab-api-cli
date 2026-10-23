use std::path::Path;

use anyhow::{Context, Result};
use broker_core::{
    build_osi_symbol, parse_account_address, AccountRef, AssetKind, BrokerKind, BrokerRegistry,
    DataSource, LegInstruction, MarketData, NetPrice, OptionChain, OptionContract, OrderIntent,
    Session, SpreadKind, SpreadLeg, TimeInForce,
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::guard::{legacy_account_busy, AccountLock};
use crate::paths::state_dir;
use crate::safety::{require_trading_approval, SafetyGate, TradingFlags};

#[derive(Debug, Clone)]
pub struct VerticalRules {
    pub dte_min: i64,
    pub dte_max: i64,
    pub min_credit: f64,
    pub max_width: f64,
    pub short_delta_min: f64,
    pub short_delta_max: f64,
    pub min_credit_to_width_pct: f64,
    pub max_contracts: f64,
}

impl Default for VerticalRules {
    fn default() -> Self {
        Self {
            dte_min: 32,
            dte_max: 45,
            min_credit: 0.30,
            max_width: 5.0,
            short_delta_min: 0.10,
            short_delta_max: 0.16,
            min_credit_to_width_pct: 12.5,
            max_contracts: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExitRules {
    pub profit_target_pct: f64,
    pub stop_loss_pct: f64,
    pub dte_close: i64,
    pub min_short_otm_pct: f64,
    pub peak_profit_min_pct: f64,
    pub giveback_exit_pct: f64,
    pub roll_enabled: bool,
    pub roll_min_dte: i64,
    pub roll_max_debit_pct: f64,
}

impl Default for ExitRules {
    fn default() -> Self {
        Self {
            profit_target_pct: 50.0,
            stop_loss_pct: 250.0,
            dte_close: 21,
            min_short_otm_pct: 3.0,
            peak_profit_min_pct: 40.0,
            giveback_exit_pct: 10.0,
            roll_enabled: false,
            roll_min_dte: 21,
            roll_max_debit_pct: 25.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SpreadChoice {
    pub underlying: String,
    pub expiration: String,
    pub short_symbol: String,
    pub long_symbol: String,
    pub short_strike: f64,
    pub long_strike: f64,
    pub credit: f64,
    pub width: f64,
    pub delta: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitAction {
    Hold,
    Close { reason: String },
    Roll { reason: String },
}

pub fn select_put_credit(
    chain: &OptionChain,
    rules: &VerticalRules,
    today: NaiveDate,
) -> Option<SpreadChoice> {
    let expiration = chain.expirations.first()?.clone();
    let expiry = NaiveDate::parse_from_str(&expiration, "%Y-%m-%d").ok()?;
    let dte = (expiry - today).num_days();
    if dte < rules.dte_min || dte > rules.dte_max {
        return None;
    }
    let mut puts: Vec<&OptionContract> = chain
        .contracts
        .iter()
        .filter(|contract| contract.put_call.eq_ignore_ascii_case("P"))
        .collect();
    puts.sort_by(|a, b| {
        b.strike
            .partial_cmp(&a.strike)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for short in &puts {
        let Some(delta) = short.greeks.delta.map(f64::abs) else {
            continue;
        };
        if delta < rules.short_delta_min || delta > rules.short_delta_max {
            continue;
        }
        let target = short.strike - rules.max_width;
        let Some(long) = puts
            .iter()
            .copied()
            .filter(|contract| contract.strike <= target + 0.001)
            .max_by(|a, b| {
                a.strike
                    .partial_cmp(&b.strike)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            continue;
        };
        let width = short.strike - long.strike;
        if width <= 0.0 || width > rules.max_width + 0.001 {
            continue;
        }
        let credit = short.bid.unwrap_or(0.0) - long.ask.unwrap_or(f64::MAX / 4.0);
        if credit < rules.min_credit {
            continue;
        }
        if credit / width * 100.0 < rules.min_credit_to_width_pct {
            continue;
        }
        return Some(SpreadChoice {
            underlying: chain.underlying.clone(),
            expiration,
            short_symbol: short.symbol.clone(),
            long_symbol: long.symbol.clone(),
            short_strike: short.strike,
            long_strike: long.strike,
            credit,
            width,
            delta,
        });
    }
    None
}

pub fn exit_action(
    entry_credit: f64,
    close_debit: f64,
    dte: i64,
    short_otm_pct: f64,
    peak_profit_pct: f64,
    rules: &ExitRules,
) -> ExitAction {
    if entry_credit <= 0.0 {
        return if dte <= rules.dte_close {
            ExitAction::Close {
                reason: format!("dte {dte} <= {}", rules.dte_close),
            }
        } else {
            ExitAction::Hold
        };
    }
    let profit_pct = (entry_credit - close_debit) / entry_credit * 100.0;
    if dte <= rules.dte_close {
        return ExitAction::Close {
            reason: format!("dte {dte} <= {}", rules.dte_close),
        };
    }
    if short_otm_pct < rules.min_short_otm_pct {
        return ExitAction::Close {
            reason: format!(
                "short otm {short_otm_pct:.2}% < {:.2}%",
                rules.min_short_otm_pct
            ),
        };
    }
    if profit_pct >= rules.profit_target_pct {
        return ExitAction::Close {
            reason: format!("profit {profit_pct:.1}% >= {:.1}%", rules.profit_target_pct),
        };
    }
    if peak_profit_pct >= rules.peak_profit_min_pct && profit_pct <= rules.giveback_exit_pct {
        return ExitAction::Close {
            reason: format!("giveback from peak {peak_profit_pct:.1}% to {profit_pct:.1}%"),
        };
    }
    let stop_debit = entry_credit * rules.stop_loss_pct / 100.0;
    if close_debit >= stop_debit {
        if rules.roll_enabled && dte >= rules.roll_min_dte {
            let debit_pct = (close_debit - entry_credit).max(0.0) / entry_credit * 100.0;
            if debit_pct <= rules.roll_max_debit_pct {
                return ExitAction::Roll {
                    reason: format!("stop debit {close_debit:.2} with roll room"),
                };
            }
        }
        return ExitAction::Close {
            reason: format!("stop debit {close_debit:.2} >= {stop_debit:.2}"),
        };
    }
    ExitAction::Hold
}

pub fn spread_intent(choice: &SpreadChoice, contracts: f64) -> OrderIntent {
    OrderIntent::Spread {
        strategy: SpreadKind::Vertical,
        legs: vec![
            SpreadLeg {
                instruction: LegInstruction::SellToOpen,
                symbol: choice.short_symbol.clone(),
                asset: AssetKind::Option,
                ratio: 1.0,
            },
            SpreadLeg {
                instruction: LegInstruction::BuyToOpen,
                symbol: choice.long_symbol.clone(),
                asset: AssetKind::Option,
                ratio: 1.0,
            },
        ],
        contracts,
        net: NetPrice::Credit(choice.credit),
        tif: TimeInForce::Day,
        session: Session::Core,
    }
}

pub fn close_intent(
    short_symbol: &str,
    long_symbol: &str,
    contracts: f64,
    debit: f64,
) -> OrderIntent {
    OrderIntent::Spread {
        strategy: SpreadKind::Vertical,
        legs: vec![
            SpreadLeg {
                instruction: LegInstruction::BuyToClose,
                symbol: short_symbol.into(),
                asset: AssetKind::Option,
                ratio: 1.0,
            },
            SpreadLeg {
                instruction: LegInstruction::SellToClose,
                symbol: long_symbol.into(),
                asset: AssetKind::Option,
                ratio: 1.0,
            },
        ],
        contracts,
        net: NetPrice::Debit(debit.max(0.01)),
        tif: TimeInForce::Day,
        session: Session::Core,
    }
}

#[derive(Debug, Deserialize)]
struct RulesFile {
    #[serde(default)]
    accounts: Vec<RulesAccount>,
    #[serde(default)]
    broker: Option<String>,
    #[serde(default)]
    data_source: Option<String>,
    #[serde(default)]
    watchlist: Vec<WatchItem>,
    #[serde(default)]
    entry_policy: PolicyYaml,
    #[serde(default)]
    entry_rules: EntryYaml,
    #[serde(default)]
    exit_rules: ExitYaml,
    #[serde(default)]
    schedule: ScheduleYaml,
    #[serde(default)]
    execution: ExecutionYaml,
    #[serde(default)]
    max_open_positions: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RulesAccount {
    #[serde(default)]
    hash: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    broker: Option<String>,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WatchItem {
    Symbol(String),
    Map {
        symbol: String,
        #[serde(default)]
        min_credit: Option<f64>,
    },
}

#[derive(Debug, Default, Deserialize)]
struct PolicyYaml {
    #[serde(default)]
    require_llm_proceed: bool,
}

#[derive(Debug, Default, Deserialize)]
struct EntryYaml {
    #[serde(default)]
    vertical: VerticalYaml,
}

#[derive(Debug, Deserialize)]
struct VerticalYaml {
    #[serde(default = "dte_min")]
    dte_min: i64,
    #[serde(default = "dte_max")]
    dte_max: i64,
    #[serde(default = "min_credit")]
    min_credit: f64,
    #[serde(default = "max_width")]
    max_width: f64,
    #[serde(default = "delta_min")]
    short_delta_min: f64,
    #[serde(default = "delta_max")]
    short_delta_max: f64,
    #[serde(default = "credit_width")]
    min_credit_to_width_pct: f64,
    #[serde(default = "one")]
    max_contracts_per_trade: f64,
}

impl Default for VerticalYaml {
    fn default() -> Self {
        Self {
            dte_min: 32,
            dte_max: 45,
            min_credit: 0.30,
            max_width: 5.0,
            short_delta_min: 0.10,
            short_delta_max: 0.16,
            min_credit_to_width_pct: 12.5,
            max_contracts_per_trade: 1.0,
        }
    }
}

fn dte_min() -> i64 {
    32
}
fn dte_max() -> i64 {
    45
}
fn min_credit() -> f64 {
    0.30
}
fn max_width() -> f64 {
    5.0
}
fn delta_min() -> f64 {
    0.10
}
fn delta_max() -> f64 {
    0.16
}
fn credit_width() -> f64 {
    12.5
}
fn one() -> f64 {
    1.0
}

#[derive(Debug, Deserialize)]
struct ExitYaml {
    #[serde(default = "profit")]
    profit_target_pct: f64,
    #[serde(default = "stop")]
    stop_loss_pct: f64,
    #[serde(default = "dte_close")]
    dte_close: i64,
    #[serde(default)]
    thesis: ThesisYaml,
    #[serde(default)]
    roll: RollYaml,
}

impl Default for ExitYaml {
    fn default() -> Self {
        Self {
            profit_target_pct: 50.0,
            stop_loss_pct: 250.0,
            dte_close: 21,
            thesis: ThesisYaml::default(),
            roll: RollYaml::default(),
        }
    }
}

fn profit() -> f64 {
    50.0
}
fn stop() -> f64 {
    250.0
}
fn dte_close() -> i64 {
    21
}

#[derive(Debug, Deserialize)]
struct ThesisYaml {
    #[serde(default = "otm")]
    min_short_otm_pct: f64,
    #[serde(default)]
    profit_giveback: GivebackYaml,
}

impl Default for ThesisYaml {
    fn default() -> Self {
        Self {
            min_short_otm_pct: 3.0,
            profit_giveback: GivebackYaml::default(),
        }
    }
}

fn otm() -> f64 {
    3.0
}

#[derive(Debug, Deserialize)]
struct GivebackYaml {
    #[serde(default = "peak")]
    peak_profit_min_pct: f64,
    #[serde(default = "give")]
    exit_if_below_pct: f64,
}

impl Default for GivebackYaml {
    fn default() -> Self {
        Self {
            peak_profit_min_pct: 40.0,
            exit_if_below_pct: 10.0,
        }
    }
}

fn peak() -> f64 {
    40.0
}
fn give() -> f64 {
    10.0
}

#[derive(Debug, Deserialize)]
struct RollYaml {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "dte_close")]
    min_dte_remaining: i64,
    #[serde(default = "roll_debit")]
    max_debit_pct_of_entry_credit: f64,
}

impl Default for RollYaml {
    fn default() -> Self {
        Self {
            enabled: false,
            min_dte_remaining: 21,
            max_debit_pct_of_entry_credit: 25.0,
        }
    }
}

fn roll_debit() -> f64 {
    25.0
}

#[derive(Debug, Default, Deserialize)]
struct ScheduleYaml {
    #[serde(default = "hours")]
    market_hours_only: bool,
}

fn hours() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
struct ExecutionYaml {
    #[serde(default)]
    min_minutes_after_open: Option<u32>,
}

impl RulesFile {
    fn vertical(&self) -> VerticalRules {
        let row = &self.entry_rules.vertical;
        VerticalRules {
            dte_min: row.dte_min,
            dte_max: row.dte_max,
            min_credit: row.min_credit,
            max_width: row.max_width,
            short_delta_min: row.short_delta_min,
            short_delta_max: row.short_delta_max,
            min_credit_to_width_pct: row.min_credit_to_width_pct,
            max_contracts: row.max_contracts_per_trade,
        }
    }

    fn exits(&self) -> ExitRules {
        ExitRules {
            profit_target_pct: self.exit_rules.profit_target_pct,
            stop_loss_pct: self.exit_rules.stop_loss_pct,
            dte_close: self.exit_rules.dte_close,
            min_short_otm_pct: self.exit_rules.thesis.min_short_otm_pct,
            peak_profit_min_pct: self.exit_rules.thesis.profit_giveback.peak_profit_min_pct,
            giveback_exit_pct: self.exit_rules.thesis.profit_giveback.exit_if_below_pct,
            roll_enabled: self.exit_rules.roll.enabled,
            roll_min_dte: self.exit_rules.roll.min_dte_remaining,
            roll_max_debit_pct: self.exit_rules.roll.max_debit_pct_of_entry_credit,
        }
    }

    fn accounts(&self) -> Result<Vec<AccountRef>> {
        let mut out = Vec::new();
        for row in &self.accounts {
            if !row.enabled {
                continue;
            }
            let id = row
                .id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| row.hash.clone());
            if id.is_empty() || id.starts_with('<') {
                continue;
            }
            let mut account = parse_account_address(&id)?;
            if let Some(broker) = row.broker.as_deref().or(self.broker.as_deref()) {
                account.broker = BrokerKind::parse(broker)?;
                if !id.contains(':') {
                    account.id = id;
                }
            }
            out.push(account);
        }
        Ok(out)
    }
}

fn load_rules(path: &str) -> Result<RulesFile> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    Ok(serde_yaml::from_str(&raw)?)
}

pub(crate) struct PaperSetup {
    pub watchlist: Vec<String>,
    pub vertical: VerticalRules,
    pub exits: ExitRules,
    pub max_open: usize,
    pub market_hours_only: bool,
    pub min_minutes_after_open: Option<u32>,
}

pub(crate) fn paper_setup(path: &str) -> Result<PaperSetup> {
    let rules = load_rules(path)?;
    let watchlist = rules
        .watchlist
        .iter()
        .map(|item| match item {
            WatchItem::Symbol(symbol) => symbol.clone(),
            WatchItem::Map { symbol, .. } => symbol.clone(),
        })
        .collect();
    Ok(PaperSetup {
        watchlist,
        vertical: rules.vertical(),
        exits: rules.exits(),
        max_open: rules.max_open_positions.unwrap_or(2).max(1),
        market_hours_only: rules.schedule.market_hours_only,
        min_minutes_after_open: rules.execution.min_minutes_after_open,
    })
}

pub async fn tick(
    registry: &BrokerRegistry,
    rules_path: &str,
    flags: &TradingFlags,
) -> Result<Value> {
    let rules = load_rules(rules_path)?;
    let vertical = rules.vertical();
    let exits = rules.exits();
    let today = chrono::Utc::now().date_naive();
    let safety = SafetyGate::load()?;
    let mut decisions = Vec::new();
    if rules.entry_policy.require_llm_proceed && !flags.simulate && !flags.dry_run {
        decisions.push(json!({
            "action": "entries_paused",
            "reason": "entry_policy.require_llm_proceed is set and the LLM advisor is not part of this stack"
        }));
    }
    for account in rules.accounts()? {
        let source = DataSource::parse(rules.data_source.as_deref(), account.broker)?;
        let handle = registry.resolve(&account, source)?;
        if rules.schedule.market_hours_only {
            let session = handle.data.session_open(&account).await?;
            if !session.open && !flags.dry_run && !flags.simulate {
                decisions.push(json!({"account": account.address(), "action": "session_closed", "source": session.source}));
                continue;
            }
        }
        let portfolio = handle.trading.portfolio(&account).await?;
        for strategy in &portfolio.strategies {
            let action = exit_action(
                strategy.unit_cost.unwrap_or(0.0).abs(),
                0.0,
                exits.dte_close + 1,
                10.0,
                0.0,
                &exits,
            );
            decisions.push(json!({
                "account": account.address(),
                "strategy": strategy.id,
                "action": format!("{action:?}"),
            }));
        }
        let allow_entry =
            !rules.entry_policy.require_llm_proceed || flags.dry_run || flags.simulate;
        if !allow_entry {
            continue;
        }
        for item in &rules.watchlist {
            let (symbol, min_credit) = match item {
                WatchItem::Symbol(symbol) => (symbol.clone(), None),
                WatchItem::Map { symbol, min_credit } => (symbol.clone(), *min_credit),
            };
            let mut rules_for_symbol = vertical.clone();
            if let Some(min_credit) = min_credit {
                rules_for_symbol.min_credit = rules_for_symbol.min_credit.max(min_credit);
            }
            let choice = pick_from_market(
                handle.data.as_ref(),
                &account,
                &symbol,
                &rules_for_symbol,
                today,
            )
            .await?;
            let Some(mut choice) = choice else {
                decisions.push(json!({"account": account.address(), "symbol": symbol, "action": "no_candidate"}));
                continue;
            };
            if handle.trading.capabilities().strategy_quote {
                let legs = match spread_intent(&choice, 1.0) {
                    broker_core::OrderIntent::Spread { legs, .. } => legs,
                    _ => Vec::new(),
                };
                if let Ok(quote) = handle.data.strategy_quote(&account, &legs).await {
                    if let Some(mark) = quote.last.or(quote.bid) {
                        if mark.abs() > 0.0 {
                            choice.credit = mark.abs();
                        }
                    }
                }
            }
            let intent = spread_intent(&choice, rules_for_symbol.max_contracts);
            safety.check(&account, &intent, portfolio.balances.equity)?;
            if flags.dry_run || flags.simulate {
                decisions.push(json!({"account": account.address(), "action": "would_open", "choice": format!("{:?}", choice)}));
                continue;
            }
            require_trading_approval(flags, "options tick")?;
            if let Some(reason) = legacy_account_busy(
                Path::new(rules_path).parent().unwrap_or(Path::new("rules")),
                &account.id,
            )? {
                anyhow::bail!(reason);
            }
            let _lock = AccountLock::acquire(&state_dir()?.join("locks"), &account.address())?;
            let preflight = handle.trading.preflight(&account, &intent).await?;
            if !preflight.accepted {
                decisions
                    .push(json!({"action": "preflight_rejected", "message": preflight.message}));
                continue;
            }
            let placed = handle
                .trading
                .place(&account, &intent, &uuid::Uuid::new_v4().to_string())
                .await?;
            decisions.push(json!({"action": "opened", "order_id": placed.order_id}));
        }
    }
    Ok(json!({"rules": rules_path, "decisions": decisions}))
}

pub(crate) async fn pick_from_market(
    data: &dyn MarketData,
    account: &AccountRef,
    symbol: &str,
    rules: &VerticalRules,
    today: NaiveDate,
) -> Result<Option<SpreadChoice>> {
    let expirations = data.option_expirations(account, symbol).await?;
    for expiration in expirations {
        let Ok(date) = NaiveDate::parse_from_str(&expiration, "%Y-%m-%d") else {
            continue;
        };
        let dte = (date - today).num_days();
        if dte < rules.dte_min || dte > rules.dte_max {
            continue;
        }
        let mut chain = data.option_chain(account, symbol, &expiration).await?;
        chain.expirations = vec![expiration];
        if let Some(choice) = select_put_credit(&chain, rules, today) {
            return Ok(Some(choice));
        }
    }
    Ok(None)
}

pub async fn flatten(
    registry: &BrokerRegistry,
    rules_path: &str,
    flags: &TradingFlags,
) -> Result<Value> {
    let rules = load_rules(rules_path)?;
    let safety = SafetyGate::load()?;
    let mut closed = Vec::new();
    for account in rules.accounts()? {
        let handle = registry.resolve(&account, DataSource::AccountBroker)?;
        let portfolio = handle.trading.portfolio(&account).await?;
        let equity = portfolio.balances.equity;
        for strategy in portfolio.strategies {
            if strategy.legs.len() < 2 {
                continue;
            }
            let short = strategy
                .legs
                .iter()
                .find(|leg| leg.side == broker_core::Side::Sell);
            let long = strategy
                .legs
                .iter()
                .find(|leg| leg.side == broker_core::Side::Buy);
            let (Some(short), Some(long)) = (short, long) else {
                continue;
            };
            let intent = close_intent(
                &short.symbol,
                &long.symbol,
                strategy.quantity.max(1.0),
                0.05,
            );
            safety.check(&account, &intent, equity)?;
            if flags.dry_run || flags.simulate {
                closed.push(json!({"account": account.address(), "strategy": strategy.id, "status": "dry_run"}));
                continue;
            }
            require_trading_approval(flags, "options flatten")?;
            let _lock = AccountLock::acquire(&state_dir()?.join("locks"), &account.address())?;
            let placed = handle
                .trading
                .place(&account, &intent, &uuid::Uuid::new_v4().to_string())
                .await?;
            closed.push(json!({"account": account.address(), "order_id": placed.order_id}));
        }
    }
    Ok(json!({"closed": closed}))
}

pub fn replay_file(chain_path: &str, today: &str, rules_path: Option<&str>) -> Result<Value> {
    let raw = std::fs::read_to_string(chain_path)?;
    let chain: OptionChain = serde_json::from_str(&raw)?;
    let rules = if let Some(path) = rules_path {
        load_rules(path)?.vertical()
    } else {
        VerticalRules::default()
    };
    let today = NaiveDate::parse_from_str(today, "%Y-%m-%d")?;
    let choice = select_put_credit(&chain, &rules, today);
    Ok(json!({"choice": choice}))
}

pub fn sample_osi(underlying: &str, expiry: &str, put_call: char, strike: f64) -> Result<String> {
    build_osi_symbol(underlying, expiry, put_call, strike)
}

#[cfg(test)]
mod tests {
    use super::*;
    use broker_core::{Greeks, OptionContract};

    fn contract(symbol: &str, strike: f64, delta: f64, bid: f64, ask: f64) -> OptionContract {
        OptionContract {
            symbol: symbol.into(),
            expiration: "2026-11-20".into(),
            strike,
            put_call: "P".into(),
            bid: Some(bid),
            ask: Some(ask),
            open_interest: Some(100.0),
            greeks: Greeks {
                delta: Some(delta),
                ..Greeks::default()
            },
        }
    }

    #[test]
    fn selects_first_put_credit_in_the_delta_band() {
        let chain = OptionChain {
            underlying: "SPY".into(),
            expirations: vec!["2026-11-20".into()],
            contracts: vec![
                contract("SPY   261120P00640000", 640.0, -0.30, 8.0, 8.2),
                contract("SPY   261120P00620000", 620.0, -0.15, 1.50, 1.60),
                contract("SPY   261120P00615000", 615.0, -0.10, 0.70, 0.80),
            ],
        };
        let choice = select_put_credit(
            &chain,
            &VerticalRules::default(),
            NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(),
        )
        .unwrap();
        assert_eq!(choice.short_strike, 620.0);
        assert_eq!(choice.long_strike, 615.0);
        assert!((choice.credit - 0.70).abs() < 1e-9);
    }

    #[test]
    fn missing_delta_on_a_higher_strike_does_not_abort_the_scan() {
        let mut wing = contract("SPY   261120P00740000", 740.0, 0.0, 0.10, 0.12);
        wing.greeks.delta = None;
        let chain = OptionChain {
            underlying: "SPY".into(),
            expirations: vec!["2026-11-20".into()],
            contracts: vec![
                wing,
                contract("SPY   261120P00620000", 620.0, -0.15, 1.50, 1.60),
                contract("SPY   261120P00615000", 615.0, -0.10, 0.70, 0.80),
            ],
        };
        let choice = select_put_credit(
            &chain,
            &VerticalRules::default(),
            NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(),
        )
        .unwrap();
        assert_eq!(choice.short_strike, 620.0);
    }

    #[test]
    fn stop_is_a_multiple_of_credit_and_can_roll() {
        let mut rules = ExitRules::default();
        rules.roll_enabled = true;
        rules.roll_max_debit_pct = 200.0;
        let action = exit_action(1.0, 2.5, 30, 8.0, 0.0, &rules);
        assert!(matches!(action, ExitAction::Roll { .. }));
        rules.roll_max_debit_pct = 25.0;
        let refused = exit_action(1.0, 2.5, 30, 8.0, 0.0, &rules);
        assert!(matches!(refused, ExitAction::Close { .. }));
        let stopped = exit_action(1.0, 2.5, 10, 8.0, 0.0, &rules);
        assert!(matches!(stopped, ExitAction::Close { .. }));
    }

    #[test]
    fn replay_is_stable() {
        let chain = OptionChain {
            underlying: "SPY".into(),
            expirations: vec!["2026-11-20".into()],
            contracts: vec![
                contract("SPY   261120P00620000", 620.0, -0.15, 1.20, 1.30),
                contract("SPY   261120P00615000", 615.0, -0.10, 0.70, 0.80),
            ],
        };
        let today = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        let first = select_put_credit(&chain, &VerticalRules::default(), today);
        let second = select_put_credit(&chain, &VerticalRules::default(), today);
        assert_eq!(first, second);
    }
}
