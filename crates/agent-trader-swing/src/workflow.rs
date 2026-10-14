//! Cron workflows: read, compute, check, then at most the orders in the file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use broker_core::{
    poll_order, parse_account_address, AccountRef, AssetKind, Bar, Broker, BrokerKind, Capabilities,
    DataSource, EquitySize, ExitBracket, LegInstruction, MarketData, NetPrice, OrderIntent, Position,
    Session, Side, SpreadKind, SpreadLeg, TimeInForce, WaitOptions, WaitUntil,
};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::indicators::{atr, change_pct, ema, rsi, sma, spread_pct};
use crate::schedule::{self, due_fire, next_fire, parse_timezone, parse_window, Cron, Window};
use agent_trader::{legacy_account_busy, require_trading_approval, state_dir, AccountLock, SafetyGate, TradingFlags};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileYaml {
    version: u32,
    #[serde(default)]
    broker: Option<String>,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    account_hash: Option<String>,
    #[serde(default)]
    data_source: Option<String>,
    workflows: Vec<WorkflowYaml>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowYaml {
    id: String,
    schedule: ScheduleYaml,
    steps: Vec<StepYaml>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleYaml {
    cron: String,
    timezone: String,
    #[serde(default)]
    active_between: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepYaml {
    id: String,
    #[serde(default)]
    fetch: Option<String>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    period: Option<serde_yaml::Value>,
    #[serde(default)]
    expiration: Option<String>,
    #[serde(default)]
    quote: Option<String>,
    #[serde(default)]
    legs: Vec<LegYaml>,
    #[serde(default)]
    when: Option<WhenYaml>,
    #[serde(default)]
    order: Option<OrderYaml>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WhenYaml {
    all: Vec<serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderYaml {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    notional_usd: Option<f64>,
    #[serde(default)]
    quantity: Option<serde_yaml::Value>,
    #[serde(default)]
    order_type: Option<String>,
    #[serde(default)]
    limit_price: Option<serde_yaml::Value>,
    #[serde(default)]
    tif: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    bracket: Option<BracketYaml>,
    #[serde(default)]
    take_profit: Option<serde_yaml::Value>,
    #[serde(default)]
    stop: Option<serde_yaml::Value>,
    #[serde(default)]
    order_id: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    poll_interval_seconds: Option<u64>,
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    contracts: Option<f64>,
    #[serde(default)]
    net_credit: Option<f64>,
    #[serde(default)]
    net_debit: Option<f64>,
    #[serde(default)]
    legs: Vec<LegYaml>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BracketYaml {
    #[serde(default)]
    take_profit: Option<serde_yaml::Value>,
    #[serde(default)]
    stop: Option<serde_yaml::Value>,
    #[serde(default)]
    take_profit_pct: Option<f64>,
    #[serde(default)]
    stop_pct: Option<f64>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct LegYaml {
    instruction: String,
    symbol: String,
    #[serde(default = "option_asset")]
    asset: String,
    #[serde(default = "one_ratio")]
    ratio: f64,
}

fn option_asset() -> String {
    "option".into()
}
fn one_ratio() -> f64 {
    1.0
}

#[derive(Debug, Clone)]
enum Check {
    SessionOpen(bool),
    BuyingPower(f64),
    MinDays(u32),
    SpreadPct(f64),
    Position { symbol: String, cmp: Cmp, value: f64 },
    Cmp { op: Cmp, left: Ref, right: Ref },
    Between { probe: Ref, low: Ref, high: Ref },
}

#[derive(Debug, Clone, Copy)]
enum Cmp {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
}

#[derive(Debug, Clone)]
enum Ref {
    Number(f64),
    Path(String),
}

enum Fetch {
    Quote { symbol: String },
    Bars { symbol: String, period: String },
    Session,
    Portfolio,
    OpenOrders,
    Expirations { symbol: String },
    Chain { symbol: String, expiration: String },
    Greeks { symbol: String },
    StrategyQuote { legs: Vec<SpreadLeg> },
    Sma { symbol: String, period: usize },
    Ema { symbol: String, period: usize },
    Rsi { symbol: String, period: usize },
    Atr { symbol: String, period: usize },
    SpreadPct { quote: String },
    ChangePct { symbol: String },
    Position { symbol: String },
}

enum OrderKind {
    Equity(EquityOrder),
    ExitBracket(ExitOrder),
    Cancel { order_id: String },
    Wait { order_id: String, until: WaitUntil, timeout: u64, poll: u64 },
    Spread(SpreadOrder),
}

struct EquityOrder {
    side: Side,
    symbol: String,
    notional: Option<f64>,
    quantity: Option<serde_yaml::Value>,
    limit: Option<serde_yaml::Value>,
    market: bool,
    tif: TimeInForce,
    session: Session,
    bracket: Option<BracketYaml>,
}

struct ExitOrder {
    symbol: String,
    quantity: serde_yaml::Value,
    take_profit: serde_yaml::Value,
    stop: serde_yaml::Value,
    tif: TimeInForce,
}

struct SpreadOrder {
    strategy: SpreadKind,
    legs: Vec<SpreadLeg>,
    contracts: f64,
    net: NetPrice,
    tif: TimeInForce,
    session: Session,
}

struct Step {
    id: String,
    body: StepBody,
}

enum StepBody {
    Fetch(Fetch),
    Order { when: Vec<Check>, order: OrderKind },
}

pub struct WorkflowFile {
    broker: Option<String>,
    account: Option<String>,
    account_hash: Option<String>,
    data_source: Option<String>,
    workflows: Vec<Workflow>,
}

struct Workflow {
    id: String,
    cron: Cron,
    tz: Tz,
    window: Option<Window>,
    steps: Vec<Step>,
    cron_text: String,
    timezone: String,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowState {
    #[serde(default)]
    slots: BTreeMap<String, String>,
    #[serde(default)]
    fills: BTreeMap<String, String>,
}

struct Outcome {
    report: Value,
    save_slot: bool,
    fills: Vec<String>,
}

enum StepFate {
    Done(Value),
    Retry(String),
}

pub fn load_workflow(path: &Path) -> Result<WorkflowFile> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_workflow(&raw)
}

pub fn parse_workflow(raw: &str) -> Result<WorkflowFile> {
    let file: FileYaml = serde_yaml::from_str(raw)?;
    if file.version != 1 {
        bail!("unsupported workflow version {}", file.version);
    }
    if file.workflows.is_empty() {
        bail!("workflow file has no workflows");
    }
    let mut seen = BTreeSet::new();
    let mut workflows = Vec::new();
    for workflow in file.workflows {
        if workflow.id.is_empty() || !seen.insert(workflow.id.clone()) {
            bail!("workflow id `{}` is empty or duplicated", workflow.id);
        }
        workflows.push(compile_workflow(workflow)?);
    }
    Ok(WorkflowFile {
        broker: file.broker,
        account: file.account,
        account_hash: file.account_hash,
        data_source: file.data_source,
        workflows,
    })
}

pub fn validate_report(path: &Path) -> Result<Value> {
    let file = load_workflow(path)?;
    let mut warnings = Vec::new();
    match file.account_ref() {
        Ok(account) if is_placeholder(&account) => {
            warnings.push(format!(
                "replace account `{}` with a real public:ID or schwab:HASH before run",
                account.address()
            ));
        }
        Ok(_) => {}
        Err(err) => warnings.push(err.to_string()),
    }
    let workflows: Vec<_> = file
        .workflows
        .iter()
        .map(|workflow| {
            json!({
                "id": workflow.id,
                "cron": workflow.cron_text,
                "timezone": workflow.timezone,
                "steps": workflow.steps.iter().map(step_summary).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({ "ok": true, "workflows": workflows, "warnings": warnings }))
}

fn step_summary(step: &Step) -> Value {
    let mut row = json!({ "id": step.id });
    if let StepBody::Fetch(Fetch::Ema { period, .. } | Fetch::Sma { period, .. } | Fetch::Rsi { period, .. } | Fetch::Atr { period, .. }) = &step.body {
        row["expanded"] = json!([format!("bars:year"), format!("indicator:{period}")]);
    }
    row
}

impl WorkflowFile {
    pub fn account_ref(&self) -> Result<AccountRef> {
        let raw = self
            .account
            .clone()
            .or(self.account_hash.clone())
            .context("workflow file needs account or account_hash")?;
        let mut account = parse_account_address(&raw)?;
        if let Some(broker) = &self.broker {
            account.broker = BrokerKind::parse(broker)?;
            if !raw.contains(':') {
                account.id = raw;
            }
        }
        Ok(account)
    }

    pub fn data_source(&self) -> Result<DataSource> {
        let account = self.account_ref()?;
        DataSource::parse(self.data_source.as_deref(), account.broker)
    }
}

pub fn is_placeholder(account: &AccountRef) -> bool {
    account.id == "ACCOUNT_ID" || account.id.starts_with('<') || account.id.contains("YOUR")
}

fn compile_workflow(raw: WorkflowYaml) -> Result<Workflow> {
    if raw.steps.is_empty() {
        bail!("workflow `{}` has no steps", raw.id);
    }
    let cron = schedule::Cron::parse(&raw.schedule.cron).with_context(|| format!("workflow `{}`", raw.id))?;
    let tz = parse_timezone(&raw.schedule.timezone)?;
    let window = match &raw.schedule.active_between {
        None => None,
        Some(pair) if pair.len() == 2 => Some(parse_window(&pair[0], &pair[1])?),
        Some(_) => bail!("workflow `{}` active_between needs start and end", raw.id),
    };
    let mut ids = BTreeSet::new();
    let mut earlier: Vec<String> = Vec::new();
    let mut quote_sources: Vec<String> = Vec::new();
    let mut steps = Vec::new();
    for step in raw.steps {
        if step.id.is_empty() || !ids.insert(step.id.clone()) {
            bail!("step id `{}` is empty or duplicated", step.id);
        }
        let compiled = compile_step(&step, &earlier, &quote_sources)?;
        if step_quotes(&compiled) {
            quote_sources.push(step.id.clone());
        }
        earlier.push(step.id.clone());
        steps.push(compiled);
    }
    Ok(Workflow {
        id: raw.id,
        cron,
        tz,
        window,
        steps,
        cron_text: raw.schedule.cron,
        timezone: raw.schedule.timezone,
    })
}

fn step_quotes(step: &Step) -> bool {
    matches!(
        step.body,
        StepBody::Fetch(Fetch::Quote { .. } | Fetch::SpreadPct { .. } | Fetch::StrategyQuote { .. })
    )
}

fn compile_step(step: &StepYaml, earlier: &[String], quote_sources: &[String]) -> Result<Step> {
    if step.fetch.is_some() && step.order.is_some() {
        bail!("step `{}` cannot both fetch and order", step.id);
    }
    if let Some(fetch) = &step.fetch {
        let body = compile_fetch(step, fetch, quote_sources)?;
        return Ok(Step { id: step.id.clone(), body: StepBody::Fetch(body) });
    }
    let order = step.order.as_ref().with_context(|| format!("step `{}` needs fetch or order", step.id))?;
    let when = match &step.when {
        Some(when) => when.all.iter().map(parse_check).collect::<Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    validate_check_names(&step.id, &when, earlier, quote_sources)?;
    let kind = compile_order(&step.id, order, earlier, &when)?;
    Ok(Step {
        id: step.id.clone(),
        body: StepBody::Order { when, order: kind },
    })
}

fn compile_fetch(step: &StepYaml, fetch: &str, quote_sources: &[String]) -> Result<Fetch> {
    let symbol = || {
        step.symbol
            .as_deref()
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty())
            .map(|symbol| symbol.to_uppercase())
            .with_context(|| format!("step `{}` needs a symbol", step.id))
    };
    match fetch {
        "quote" => Ok(Fetch::Quote { symbol: symbol()? }),
        "bars" => Ok(Fetch::Bars {
            symbol: symbol()?,
            period: bar_period(step.period.as_ref())?,
        }),
        "session" => Ok(Fetch::Session),
        "portfolio" => Ok(Fetch::Portfolio),
        "open_orders" => Ok(Fetch::OpenOrders),
        "expirations" => Ok(Fetch::Expirations { symbol: symbol()? }),
        "chain" => Ok(Fetch::Chain {
            symbol: symbol()?,
            expiration: step
                .expiration
                .clone()
                .filter(|value| !value.is_empty())
                .with_context(|| format!("step `{}` needs expiration", step.id))?,
        }),
        "greeks" => Ok(Fetch::Greeks { symbol: symbol()? }),
        "strategy_quote" => {
            if step.legs.len() < 2 {
                bail!("step `{}` strategy_quote needs at least two legs", step.id);
            }
            Ok(Fetch::StrategyQuote {
                legs: step.legs.iter().map(leg_from).collect::<Result<Vec<_>>>()?,
            })
        }
        "sma" => Ok(Fetch::Sma { symbol: symbol()?, period: required_period(step)? }),
        "ema" => Ok(Fetch::Ema { symbol: symbol()?, period: required_period(step)? }),
        "rsi" => Ok(Fetch::Rsi { symbol: symbol()?, period: optional_period(step, 14)? }),
        "atr" => Ok(Fetch::Atr { symbol: symbol()?, period: optional_period(step, 14)? }),
        "spread_pct" => {
            let quote = step.quote.clone().or_else(|| quote_sources.last().cloned());
            let Some(quote) = quote else {
                bail!("step `{}` spread_pct needs an earlier quote step", step.id);
            };
            if !quote_sources.iter().any(|id| id == &quote) {
                bail!("step `{}` quotes `{quote}`, which is not an earlier quote", step.id);
            }
            Ok(Fetch::SpreadPct { quote })
        }
        "change_pct" => Ok(Fetch::ChangePct { symbol: symbol()? }),
        "position" => Ok(Fetch::Position { symbol: symbol()? }),
        other => bail!("step `{}` has unknown fetch `{other}`", step.id),
    }
}

fn bar_period(period: Option<&serde_yaml::Value>) -> Result<String> {
    let Some(period) = period else {
        return Ok("year".into());
    };
    let text = match period {
        serde_yaml::Value::String(text) => text.to_ascii_lowercase(),
        _ => bail!("bars period must be day, week, month, or year"),
    };
    if matches!(text.as_str(), "day" | "1d" | "week" | "month" | "year" | "1y") {
        Ok(text)
    } else {
        bail!("bars period `{text}` is not day, week, month, or year")
    }
}

fn required_period(step: &StepYaml) -> Result<usize> {
    let period = step.period.as_ref().with_context(|| format!("step `{}` needs period", step.id))?;
    let number = period.as_u64().with_context(|| format!("step `{}` period must be a positive integer", step.id))?;
    if number == 0 {
        bail!("step `{}` period must be a positive integer", step.id);
    }
    Ok(number as usize)
}

fn optional_period(step: &StepYaml, default: usize) -> Result<usize> {
    match &step.period {
        None => Ok(default),
        Some(_) => required_period(step),
    }
}

fn compile_order(id: &str, order: &OrderYaml, earlier: &[String], when: &[Check]) -> Result<OrderKind> {
    let kind = order.kind.as_deref().unwrap_or(if order.side.is_some() { "equity" } else { "" });
    if when.iter().any(|check| matches!(check, Check::MinDays(_))) && order.symbol.as_deref().unwrap_or("").is_empty() && kind != "spread" {
        bail!("step `{id}` min_days_since_fill needs order.symbol");
    }
    match kind {
        "equity" | "" => Ok(OrderKind::Equity(compile_equity(id, order, earlier)?)),
        "exit_bracket" => Ok(OrderKind::ExitBracket(compile_exit(id, order, earlier)?)),
        "cancel" => {
            let order_id = order.order_id.clone().with_context(|| format!("step `{id}` cancel needs order_id"))?;
            require_path(id, &order_id, earlier)?;
            Ok(OrderKind::Cancel { order_id })
        }
        "wait" => {
            let order_id = order.order_id.clone().with_context(|| format!("step `{id}` wait needs order_id"))?;
            require_path(id, &order_id, earlier)?;
            let until = match order.until.as_deref().unwrap_or("filled") {
                "filled" => WaitUntil::Filled,
                "terminal" => WaitUntil::Terminal,
                "accepted" => WaitUntil::Accepted,
                other => bail!("step `{id}` until `{other}` is not filled, terminal, or accepted"),
            };
            Ok(OrderKind::Wait {
                order_id,
                until,
                timeout: order.timeout_seconds.unwrap_or(3600),
                poll: order.poll_interval_seconds.unwrap_or(5),
            })
        }
        "spread" => Ok(OrderKind::Spread(compile_spread(id, order)?)),
        other => bail!("step `{id}` has unknown order kind `{other}`"),
    }
}

fn compile_equity(id: &str, order: &OrderYaml, earlier: &[String]) -> Result<EquityOrder> {
    let side = match order.side.as_deref() {
        Some("buy") => Side::Buy,
        Some("sell") => Side::Sell,
        _ => bail!("step `{id}` equity order needs side buy or sell"),
    };
    let symbol = order
        .symbol
        .as_deref()
        .map(|symbol| symbol.trim().to_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .with_context(|| format!("step `{id}` needs a symbol"))?;
    if order.notional_usd.is_none() && order.quantity.is_none() {
        bail!("step `{id}` needs notional_usd or quantity");
    }
    if order.notional_usd.is_some() && order.quantity.is_some() {
        bail!("step `{id}` sets both notional_usd and quantity");
    }
    let market = match order.order_type.as_deref().unwrap_or("market") {
        "market" => true,
        "limit" => false,
        other => bail!("step `{id}` order_type `{other}` is not market or limit"),
    };
    if !market && order.limit_price.is_none() {
        bail!("step `{id}` limit order needs limit_price");
    }
    if let Some(price) = &order.limit_price {
        require_value_ref(id, price, earlier)?;
    }
    if let Some(quantity) = &order.quantity {
        require_value_ref(id, quantity, earlier)?;
    }
    if let Some(bracket) = &order.bracket {
        for price in [&bracket.take_profit, &bracket.stop].into_iter().flatten() {
            require_value_ref(id, price, earlier)?;
        }
    }
    Ok(EquityOrder {
        side,
        symbol,
        notional: order.notional_usd,
        quantity: order.quantity.clone(),
        limit: order.limit_price.clone(),
        market,
        tif: parse_tif(id, order.tif.as_deref())?,
        session: parse_session(id, order.session.as_deref())?,
        bracket: order.bracket.clone(),
    })
}

fn compile_exit(id: &str, order: &OrderYaml, earlier: &[String]) -> Result<ExitOrder> {
    let symbol = order
        .symbol
        .as_deref()
        .map(|symbol| symbol.trim().to_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .with_context(|| format!("step `{id}` exit_bracket needs a symbol"))?;
    let quantity = order.quantity.clone().with_context(|| format!("step `{id}` exit_bracket needs quantity"))?;
    let take_profit = order.take_profit.clone().with_context(|| format!("step `{id}` exit_bracket needs take_profit"))?;
    let stop = order.stop.clone().with_context(|| format!("step `{id}` exit_bracket needs stop"))?;
    require_value_ref(id, &quantity, earlier)?;
    require_value_ref(id, &take_profit, earlier)?;
    require_value_ref(id, &stop, earlier)?;
    Ok(ExitOrder {
        symbol,
        quantity,
        take_profit,
        stop,
        tif: parse_tif(id, order.tif.as_deref())?,
    })
}

fn compile_spread(id: &str, order: &OrderYaml) -> Result<SpreadOrder> {
    if order.legs.len() < 2 {
        bail!("step `{id}` spread needs at least two legs");
    }
    let strategy = match order.strategy.as_deref().unwrap_or("custom") {
        "vertical" => SpreadKind::Vertical,
        "iron_condor" => SpreadKind::IronCondor,
        "custom" => SpreadKind::Custom,
        other => bail!("step `{id}` strategy `{other}` is not vertical, iron_condor, or custom"),
    };
    let net = match (order.net_credit, order.net_debit) {
        (Some(credit), None) => NetPrice::Credit(credit),
        (None, Some(debit)) => NetPrice::Debit(debit),
        _ => bail!("step `{id}` spread needs net_credit or net_debit"),
    };
    Ok(SpreadOrder {
        strategy,
        legs: order.legs.iter().map(leg_from).collect::<Result<Vec<_>>>()?,
        contracts: order.contracts.unwrap_or(1.0),
        net,
        tif: parse_tif(id, order.tif.as_deref())?,
        session: parse_session(id, order.session.as_deref())?,
    })
}

fn parse_tif(id: &str, raw: Option<&str>) -> Result<TimeInForce> {
    match raw.unwrap_or("day") {
        "day" => Ok(TimeInForce::Day),
        "gtc" => Ok(TimeInForce::Gtc),
        "fok" => Ok(TimeInForce::Fok),
        other => bail!("step `{id}` tif `{other}` is not day, gtc, or fok"),
    }
}

fn parse_session(id: &str, raw: Option<&str>) -> Result<Session> {
    match raw.unwrap_or("core") {
        "core" => Ok(Session::Core),
        "extended" => Ok(Session::Extended),
        "twenty_four_hours" => Ok(Session::TwentyFourHours),
        other => bail!("step `{id}` session `{other}` is not core, extended, or twenty_four_hours"),
    }
}

fn leg_from(raw: &LegYaml) -> Result<SpreadLeg> {
    Ok(SpreadLeg {
        instruction: parse_instruction(&raw.instruction)?,
        symbol: raw.symbol.clone(),
        asset: parse_asset(&raw.asset)?,
        ratio: raw.ratio,
    })
}

fn parse_instruction(raw: &str) -> Result<LegInstruction> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "buy" => Ok(LegInstruction::Buy),
        "sell" => Ok(LegInstruction::Sell),
        "buy_to_open" => Ok(LegInstruction::BuyToOpen),
        "sell_to_open" => Ok(LegInstruction::SellToOpen),
        "buy_to_close" => Ok(LegInstruction::BuyToClose),
        "sell_to_close" => Ok(LegInstruction::SellToClose),
        other => bail!("unknown leg instruction `{other}`"),
    }
}

fn parse_asset(raw: &str) -> Result<AssetKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "equity" => Ok(AssetKind::Equity),
        "option" => Ok(AssetKind::Option),
        other => bail!("unknown leg asset `{other}`"),
    }
}

fn parse_check(value: &serde_yaml::Value) -> Result<Check> {
    let map = value.as_mapping().context("a check must be a one-key map")?;
    if map.len() != 1 {
        bail!("a check must set exactly one condition");
    }
    let (key, raw) = map.iter().next().unwrap();
    let key = key.as_str().context("check name")?;
    match key {
        "session_open" => Ok(Check::SessionOpen(raw.as_bool().context("session_open must be true or false")?)),
        "buying_power_at_least" => Ok(Check::BuyingPower(yaml_f64(raw).context("buying_power_at_least")?)),
        "min_days_since_fill" => Ok(Check::MinDays(yaml_f64(raw).context("min_days_since_fill")? as u32)),
        "spread_pct_at_most" => Ok(Check::SpreadPct(yaml_f64(raw).context("spread_pct_at_most")?)),
        "position_qty" => parse_position_qty(raw),
        "gt" | "gte" | "lt" | "lte" | "eq" => {
            let (left, right) = yaml_pair(raw)?;
            Ok(Check::Cmp { op: cmp_from(key), left, right })
        }
        "between" => {
            let items = raw.as_sequence().context("between needs [probe, low, high]")?;
            if items.len() != 3 {
                bail!("between needs [probe, low, high]");
            }
            Ok(Check::Between {
                probe: yaml_ref(&items[0])?,
                low: yaml_ref(&items[1])?,
                high: yaml_ref(&items[2])?,
            })
        }
        other => bail!("unknown check `{other}`"),
    }
}

fn parse_position_qty(raw: &serde_yaml::Value) -> Result<Check> {
    let map = raw.as_mapping().context("position_qty needs symbol and one comparison")?;
    let symbol = map
        .get(serde_yaml::Value::String("symbol".into()))
        .and_then(|value| value.as_str())
        .context("position_qty needs symbol")?
        .to_uppercase();
    let mut found = None;
    for name in ["eq", "gt", "gte", "lt", "lte"] {
        if let Some(value) = map.get(serde_yaml::Value::String(name.into())) {
            if found.is_some() {
                bail!("position_qty sets more than one comparison");
            }
            found = Some((cmp_from(name), yaml_f64(value).context("position_qty")?));
        }
    }
    let Some((cmp, value)) = found else {
        bail!("position_qty needs eq, gt, gte, lt, or lte");
    };
    Ok(Check::Position { symbol, cmp, value })
}

fn cmp_from(name: &str) -> Cmp {
    match name {
        "gt" => Cmp::Gt,
        "gte" => Cmp::Gte,
        "lt" => Cmp::Lt,
        "lte" => Cmp::Lte,
        _ => Cmp::Eq,
    }
}

fn yaml_pair(raw: &serde_yaml::Value) -> Result<(Ref, Ref)> {
    let items = raw.as_sequence().context("comparison needs [left, right]")?;
    if items.len() != 2 {
        bail!("comparison needs [left, right]");
    }
    Ok((yaml_ref(&items[0])?, yaml_ref(&items[1])?))
}

fn yaml_ref(raw: &serde_yaml::Value) -> Result<Ref> {
    if let Some(number) = yaml_f64(raw) {
        return Ok(Ref::Number(number));
    }
    if let Some(path) = raw.as_str() {
        if path.is_empty() {
            bail!("binding path is empty");
        }
        return Ok(Ref::Path(path.to_string()));
    }
    bail!("expected a number or a binding path")
}

fn yaml_f64(raw: &serde_yaml::Value) -> Option<f64> {
    raw.as_f64().or_else(|| raw.as_i64().map(|value| value as f64)).or_else(|| raw.as_u64().map(|value| value as f64))
}

fn validate_check_names(step_id: &str, checks: &[Check], earlier: &[String], quote_sources: &[String]) -> Result<()> {
    let mut spread = false;
    for check in checks {
        match check {
            Check::Cmp { left, right, .. } => {
                require_ref(step_id, left, earlier)?;
                require_ref(step_id, right, earlier)?;
            }
            Check::Between { probe, low, high } => {
                require_ref(step_id, probe, earlier)?;
                require_ref(step_id, low, earlier)?;
                require_ref(step_id, high, earlier)?;
            }
            Check::SpreadPct(_) => spread = true,
            _ => {}
        }
    }
    if spread && quote_sources.is_empty() {
        bail!("step `{step_id}` spread_pct_at_most needs an earlier quote or spread_pct step");
    }
    Ok(())
}

fn require_ref(step_id: &str, reference: &Ref, earlier: &[String]) -> Result<()> {
    let Ref::Path(path) = reference else {
        return Ok(());
    };
    require_path(step_id, path, earlier)
}

fn require_path(step_id: &str, path: &str, earlier: &[String]) -> Result<()> {
    let root = path.split('.').next().unwrap_or(path);
    if earlier.iter().any(|id| id == root) {
        Ok(())
    } else {
        bail!("step `{step_id}` references `{path}`, which is not an earlier step")
    }
}

fn require_value_ref(step_id: &str, value: &serde_yaml::Value, earlier: &[String]) -> Result<()> {
    if let Some(path) = value.as_str() {
        require_path(step_id, path, earlier)?;
    }
    Ok(())
}

pub fn state_path_for(rules_path: &Path) -> Result<PathBuf> {
    let stem = rules_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("workflow");
    Ok(state_dir()?.join(format!("workflow-{stem}.json")))
}

fn load_state(path: &Path) -> WorkflowState {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return WorkflowState::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_state(path: &Path, state: &WorkflowState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn seconds_until_next(file: &WorkflowFile, state_path: &Path, now: DateTime<Utc>) -> u64 {
    let state = load_state(state_path);
    let mut any_due = false;
    let mut soonest = 180u64;
    for workflow in &file.workflows {
        if let Some(fire) = due_fire(&workflow.cron, workflow.window, now, workflow.tz) {
            if state.slots.get(&workflow.id).map(String::as_str) != Some(fire.to_rfc3339().as_str()) {
                any_due = true;
            }
        }
        if let Some(next) = next_fire(&workflow.cron, workflow.window, now.with_timezone(&workflow.tz)) {
            let secs = (next.with_timezone(&Utc) - now).num_seconds();
            if secs > 0 {
                soonest = soonest.min(secs as u64);
            }
        }
    }
    if any_due {
        30
    } else {
        soonest.clamp(1, 180)
    }
}

/// Local cash and positions for a paper tick. The workflow reads these instead of the live account.
pub struct PaperLedger {
    pub cash: f64,
    pub equity: f64,
    pub positions: Vec<Position>,
}

pub async fn run_due(
    file: &WorkflowFile,
    trading: &dyn Broker,
    data: &dyn MarketData,
    account: &AccountRef,
    safety: &SafetyGate,
    flags: &TradingFlags,
    state_path: &Path,
    rules_path: &Path,
    now: DateTime<Utc>,
    lock: bool,
) -> Result<Value> {
    run_due_with(file, trading, data, account, safety, flags, state_path, rules_path, now, lock, None, false).await
}

pub async fn run_due_with(
    file: &WorkflowFile,
    trading: &dyn Broker,
    data: &dyn MarketData,
    account: &AccountRef,
    safety: &SafetyGate,
    flags: &TradingFlags,
    state_path: &Path,
    rules_path: &Path,
    now: DateTime<Utc>,
    lock: bool,
    paper: Option<&PaperLedger>,
    record: bool,
) -> Result<Value> {
    let mut state = load_state(state_path);
    let mut rows = Vec::new();
    let mut dirty = false;
    for workflow in &file.workflows {
        let local_now = now.with_timezone(&workflow.tz);
        let Some(fire) = due_fire(&workflow.cron, workflow.window, now, workflow.tz) else {
            let next = next_fire(&workflow.cron, workflow.window, local_now).map(|next| next.to_rfc3339());
            rows.push(json!({ "id": workflow.id, "status": "waiting", "next_fire": next }));
            continue;
        };
        let slot = fire.to_rfc3339();
        let persist = record || (!flags.dry_run && !flags.simulate);
        if persist && state.slots.get(&workflow.id).map(String::as_str) == Some(slot.as_str()) {
            rows.push(json!({ "id": workflow.id, "status": "already_ran", "slot": slot }));
            continue;
        }
        let outcome = eval_workflow(
            workflow, trading, data, account, safety, flags, rules_path, &state, now, lock, paper, record,
        )
        .await?;
        if persist && outcome.save_slot {
            state.slots.insert(workflow.id.clone(), slot);
            for symbol in &outcome.fills {
                state.fills.insert(format!("{}|{symbol}", workflow.id), now.to_rfc3339());
            }
            dirty = true;
        }
        rows.push(outcome.report);
    }
    if dirty {
        save_state(state_path, &state)?;
    }
    Ok(json!({ "account": account.address(), "workflows": rows }))
}

async fn eval_workflow(
    workflow: &Workflow,
    trading: &dyn Broker,
    data: &dyn MarketData,
    account: &AccountRef,
    safety: &SafetyGate,
    flags: &TradingFlags,
    rules_path: &Path,
    state: &WorkflowState,
    now: DateTime<Utc>,
    lock: bool,
    paper: Option<&PaperLedger>,
    record: bool,
) -> Result<Outcome> {
    let mut runner = Runner {
        trading,
        data,
        account,
        safety,
        flags,
        rules_path,
        now,
        lock,
        caps: trading.capabilities(),
        bindings: Map::new(),
        bars: HashMap::new(),
        quotes: HashMap::new(),
        positions: paper.map(|book| book.positions.clone()).unwrap_or_default(),
        have_portfolio: paper.is_some(),
        cash: paper.map(|book| book.cash).unwrap_or(0.0),
        equity: paper.map(|book| book.equity).unwrap_or(0.0),
        options_bp: None,
        paper: paper.is_some() && flags.simulate,
        session_open: None,
        spread: None,
        guard: None,
    };
    let mut steps = Vec::new();
    let mut save_slot = true;
    let mut fills = Vec::new();
    let mut ordered = false;
    let mut skipped = false;
    let mut rejected = false;
    for step in &workflow.steps {
        match runner.run_step(workflow, step, state).await {
            StepFate::Retry(reason) => {
                save_slot = false;
                steps.push(json!({ "id": step.id, "status": "error", "reason": reason }));
            }
            StepFate::Done(row) => {
                let status = row.get("status").and_then(|value| value.as_str()).unwrap_or("");
                if matches!(status, "submitted" | "dry_run" | "simulated") {
                    ordered = true;
                    if status == "submitted" || (record && status == "simulated") {
                        if let Some(symbol) = row.get("symbol").and_then(|value| value.as_str()) {
                            if !symbol.is_empty() {
                                fills.push(symbol.to_string());
                            }
                        }
                    }
                } else if status == "skipped" {
                    skipped = true;
                } else if status == "rejected" {
                    rejected = true;
                }
                steps.push(row);
            }
        }
    }
    let status = if !save_slot {
        "retry"
    } else if flags.dry_run && ordered {
        "dry_run"
    } else if flags.simulate && ordered {
        "simulated"
    } else if ordered {
        "submitted"
    } else if rejected {
        "rejected"
    } else if skipped {
        "skipped"
    } else {
        "ok"
    };
    Ok(Outcome {
        report: json!({ "id": workflow.id, "status": status, "steps": steps }),
        save_slot,
        fills,
    })
}

struct Runner<'a> {
    trading: &'a dyn Broker,
    data: &'a dyn MarketData,
    account: &'a AccountRef,
    safety: &'a SafetyGate,
    flags: &'a TradingFlags,
    rules_path: &'a Path,
    now: DateTime<Utc>,
    lock: bool,
    caps: Capabilities,
    bindings: Map<String, Value>,
    bars: HashMap<String, Vec<Bar>>,
    quotes: HashMap<String, broker_core::Quote>,
    positions: Vec<Position>,
    have_portfolio: bool,
    cash: f64,
    equity: f64,
    options_bp: Option<f64>,
    paper: bool,
    session_open: Option<bool>,
    spread: Option<f64>,
    guard: Option<AccountLock>,
}

impl Runner<'_> {
    async fn run_step(&mut self, workflow: &Workflow, step: &Step, state: &WorkflowState) -> StepFate {
        match &step.body {
            StepBody::Fetch(fetch) => self.run_fetch(&step.id, fetch).await,
            StepBody::Order { when, order } => self.run_order(workflow, &step.id, when, order, state).await,
        }
    }

    async fn run_fetch(&mut self, id: &str, fetch: &Fetch) -> StepFate {
        match fetch {
            Fetch::Quote { symbol } => match self.data.quotes(self.account, &[symbol.clone()]).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(quotes) => {
                    let Some(quote) = quotes.into_iter().find(|quote| quote.symbol.eq_ignore_ascii_case(symbol)) else {
                        return StepFate::Done(json!({ "id": id, "status": "skipped", "reason": "quote missing" }));
                    };
                    if let (Some(bid), Some(ask)) = (quote.bid, quote.ask) {
                        self.spread = spread_pct(bid, ask);
                    }
                    self.quotes.insert(symbol.clone(), quote.clone());
                    let row = json!({
                        "bid": quote.bid, "ask": quote.ask, "last": quote.last,
                        "bid_size": quote.bid_size, "ask_size": quote.ask_size, "volume": quote.volume,
                    });
                    self.bindings.insert(id.to_string(), row);
                    StepFate::Done(json!({ "id": id, "status": "ok" }))
                }
            },
            Fetch::Bars { symbol, period } => match self.data.bars(self.account, symbol, period).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(bars) => {
                    let bars = sort_bars(bars);
                    let count = bars.len();
                    let last_close = bars.last().map(|bar| bar.close);
                    let prev_close = bars.len().checked_sub(2).and_then(|index| bars.get(index)).map(|bar| bar.close);
                    self.bindings.insert(id.to_string(), json!({ "count": count, "last_close": last_close, "prev_close": prev_close }));
                    self.bars.insert(format!("{symbol}|{period}"), bars);
                    StepFate::Done(json!({ "id": id, "status": "ok", "count": count }))
                }
            },
            Fetch::Session => match self.data.session_open(self.account).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(status) => {
                    self.session_open = Some(status.open);
                    self.bindings.insert(id.to_string(), json!({ "open": status.open }));
                    StepFate::Done(json!({ "id": id, "status": "ok", "open": status.open }))
                }
            },
            Fetch::Portfolio => match self.ensure_portfolio().await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(()) => {
                    self.bind_portfolio(id);
                    StepFate::Done(json!({ "id": id, "status": "ok" }))
                }
            },
            Fetch::OpenOrders => match self.trading.open_orders(self.account).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(orders) => {
                    let ids: Vec<_> = orders.iter().map(|order| order.id.clone()).collect();
                    let first_id = ids.first().cloned();
                    self.bindings.insert(id.to_string(), json!({ "count": ids.len(), "ids": ids, "first_id": first_id }));
                    StepFate::Done(json!({ "id": id, "status": "ok", "count": ids.len() }))
                }
            },
            Fetch::Expirations { symbol } => match self.data.option_expirations(self.account, symbol).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(expirations) => {
                    let first = expirations.first().cloned();
                    self.bindings.insert(id.to_string(), json!({ "count": expirations.len(), "first": first }));
                    StepFate::Done(json!({ "id": id, "status": "ok", "count": expirations.len() }))
                }
            },
            Fetch::Chain { symbol, expiration } => match self.data.option_chain(self.account, symbol, expiration).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(chain) => {
                    self.bindings.insert(id.to_string(), json!({
                        "underlying": chain.underlying,
                        "expiration": expiration,
                        "contract_count": chain.contracts.len(),
                    }));
                    StepFate::Done(json!({ "id": id, "status": "ok", "contract_count": chain.contracts.len() }))
                }
            },
            Fetch::Greeks { symbol } => match self.data.greeks(self.account, symbol).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(greeks) => {
                    self.bindings.insert(id.to_string(), json!({
                        "delta": greeks.delta, "gamma": greeks.gamma, "theta": greeks.theta,
                        "vega": greeks.vega, "iv": greeks.iv,
                    }));
                    StepFate::Done(json!({ "id": id, "status": "ok" }))
                }
            },
            Fetch::StrategyQuote { legs } => {
                if !self.caps.strategy_quote {
                    return StepFate::Done(json!({
                        "id": id,
                        "status": "skipped",
                        "reason": format!("{} has no strategy quote", self.caps.broker),
                    }));
                }
                match self.data.strategy_quote(self.account, legs).await {
                    Err(err) => StepFate::Retry(err.to_string()),
                    Ok(quote) => {
                        self.bindings.insert(id.to_string(), json!({ "bid": quote.bid, "ask": quote.ask, "last": quote.last }));
                        StepFate::Done(json!({ "id": id, "status": "ok" }))
                    }
                }
            }
            Fetch::Sma { symbol, period } => self.bind_indicator(id, symbol, |closes, _, _, _| sma(closes, *period)).await,
            Fetch::Ema { symbol, period } => self.bind_indicator(id, symbol, |closes, _, _, _| ema(closes, *period)).await,
            Fetch::Rsi { symbol, period } => self.bind_indicator(id, symbol, |closes, _, _, _| rsi(closes, *period)).await,
            Fetch::Atr { symbol, period } => self.bind_indicator(id, symbol, |_, high, low, close| atr(high, low, close, *period)).await,
            Fetch::SpreadPct { quote } => {
                let Some(bound) = self.bindings.get(quote) else {
                    return StepFate::Done(json!({ "id": id, "status": "skipped", "reason": format!("missing {quote}") }));
                };
                let bid = bound.get("bid").and_then(as_number);
                let ask = bound.get("ask").and_then(as_number);
                let Some(value) = bid.zip(ask).and_then(|(bid, ask)| spread_pct(bid, ask)) else {
                    return StepFate::Done(json!({ "id": id, "status": "skipped", "reason": "spread unavailable" }));
                };
                self.spread = Some(value);
                self.bindings.insert(id.to_string(), json!(value));
                StepFate::Done(json!({ "id": id, "status": "ok", "value": value }))
            }
            Fetch::ChangePct { symbol } => match self.year_bars(symbol).await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(bars) => {
                    let last = bars.last().map(|bar| bar.close);
                    let prev = bars.len().checked_sub(2).and_then(|index| bars.get(index)).map(|bar| bar.close);
                    let Some(value) = last.zip(prev).and_then(|(last, prev)| change_pct(prev, last)) else {
                        return StepFate::Done(json!({ "id": id, "status": "skipped", "reason": "short history" }));
                    };
                    self.bindings.insert(id.to_string(), json!(value));
                    StepFate::Done(json!({ "id": id, "status": "ok", "value": value }))
                }
            },
            Fetch::Position { symbol } => match self.ensure_portfolio().await {
                Err(err) => StepFate::Retry(err.to_string()),
                Ok(()) => {
                    let row = self.position_row(symbol);
                    self.bindings.insert(id.to_string(), row);
                    StepFate::Done(json!({ "id": id, "status": "ok" }))
                }
            },
        }
    }

    async fn bind_indicator(
        &mut self,
        id: &str,
        symbol: &str,
        compute: impl Fn(&[f64], &[f64], &[f64], &[f64]) -> Option<f64>,
    ) -> StepFate {
        let bars = match self.year_bars(symbol).await {
            Ok(bars) => bars,
            Err(err) => return StepFate::Retry(err.to_string()),
        };
        let closes: Vec<_> = bars.iter().map(|bar| bar.close).collect();
        let high: Vec<_> = bars.iter().map(|bar| bar.high).collect();
        let low: Vec<_> = bars.iter().map(|bar| bar.low).collect();
        let Some(value) = compute(&closes, &high, &low, &closes) else {
            return StepFate::Done(json!({
                "id": id,
                "status": "skipped",
                "reason": format!("short history ({})", closes.len()),
            }));
        };
        self.bindings.insert(id.to_string(), json!(value));
        StepFate::Done(json!({ "id": id, "status": "ok", "value": value }))
    }

    async fn run_order(
        &mut self,
        workflow: &Workflow,
        id: &str,
        when: &[Check],
        order: &OrderKind,
        state: &WorkflowState,
    ) -> StepFate {
        if let Err(err) = self.prepare_checks(when).await {
            return StepFate::Retry(err.to_string());
        }
        let symbol = order_symbol(order);
        let last_fill = symbol.as_deref().and_then(|symbol| {
            state
                .fills
                .get(&format!("{}|{symbol}", workflow.id))
                .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                .map(|stamp| stamp.with_timezone(&Utc))
        });
        if let Some(reason) = first_failure(when, &self.check_env(last_fill)) {
            return StepFate::Done(json!({ "id": id, "status": "skipped", "reason": reason }));
        }
        match order {
            OrderKind::Cancel { order_id } => self.cancel_order(id, order_id).await,
            OrderKind::Wait { order_id, until, timeout, poll } => self.wait_order(id, order_id, *until, *timeout, *poll).await,
            OrderKind::Equity(spec) => self.place_equity(id, spec).await,
            OrderKind::ExitBracket(spec) => self.place_exit(id, spec).await,
            OrderKind::Spread(spec) => self.place_spread(id, spec).await,
        }
    }

    async fn prepare_checks(&mut self, when: &[Check]) -> Result<()> {
        if when.iter().any(|check| matches!(check, Check::SessionOpen(_))) && self.session_open.is_none() {
            let status = self.data.session_open(self.account).await?;
            self.session_open = Some(status.open);
            if !self.bindings.contains_key("session") {
                self.bindings.insert("session".into(), json!({ "open": status.open }));
            }
        }
        if when.iter().any(|check| matches!(check, Check::BuyingPower(_) | Check::Position { .. })) && !self.have_portfolio {
            self.ensure_portfolio().await?;
        }
        Ok(())
    }

    fn check_env(&self, last_fill: Option<DateTime<Utc>>) -> CheckEnv<'_> {
        CheckEnv {
            bindings: &self.bindings,
            positions: &self.positions,
            cash: self.have_portfolio.then_some(self.cash),
            session_open: self.session_open,
            spread: self.spread,
            last_fill,
            now: self.now,
        }
    }

    async fn place_equity(&mut self, id: &str, spec: &EquityOrder) -> StepFate {
        let limit = match &spec.limit {
            Some(value) => match self.resolve_number(value) {
                Ok(number) => Some(number),
                Err(err) => return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string() })),
            },
            None => None,
        };
        if !spec.market && limit.is_none() {
            return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": "limit price missing" }));
        }
        let size = if let Some(amount) = spec.notional {
            EquitySize::Notional(amount)
        } else {
            match spec.quantity.as_ref().map(|value| self.resolve_number(value)) {
                Some(Ok(quantity)) => EquitySize::Shares(quantity),
                Some(Err(err)) => return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string() })),
                None => return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": "size missing" })),
            }
        };
        let shares = match &size {
            EquitySize::Shares(quantity) => *quantity,
            EquitySize::Notional(amount) => {
                let reference = limit.or_else(|| self.quote_last(&spec.symbol)).unwrap_or(0.0);
                if reference > 0.0 { *amount / reference } else { 0.0 }
            }
        };
        let bracket = if let Some(bracket) = &spec.bracket {
            let reference = limit.or_else(|| self.quote_last(&spec.symbol));
            match build_bracket(&spec.symbol, spec.side, shares, reference, bracket, &self.bindings) {
                Ok(bracket) => Some(bracket),
                Err(err) => return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string() })),
            }
        } else {
            None
        };
        let intent = OrderIntent::Equity {
            side: spec.side,
            symbol: spec.symbol.clone(),
            size,
            limit: if spec.market { None } else { limit },
            tif: spec.tif,
            session: spec.session,
            bracket,
        };
        self.submit(id, &spec.symbol, &intent).await
    }

    async fn place_exit(&mut self, id: &str, spec: &ExitOrder) -> StepFate {
        let quantity = match self.resolve_number(&spec.quantity) {
            Ok(quantity) => quantity,
            Err(err) => return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string() })),
        };
        let take_profit = match self.resolve_number(&spec.take_profit) {
            Ok(price) => price,
            Err(err) => return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string() })),
        };
        let stop = match self.resolve_number(&spec.stop) {
            Ok(price) => price,
            Err(err) => return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string() })),
        };
        let intent = OrderIntent::ExitBracket(ExitBracket {
            symbol: spec.symbol.clone(),
            quantity,
            take_profit,
            stop,
            stop_limit: stop * 0.995,
            tif: spec.tif,
        });
        self.submit(id, &spec.symbol, &intent).await
    }

    async fn place_spread(&mut self, id: &str, spec: &SpreadOrder) -> StepFate {
        let intent = OrderIntent::Spread {
            strategy: spec.strategy,
            legs: spec.legs.clone(),
            contracts: spec.contracts,
            net: spec.net,
            tif: spec.tif,
            session: spec.session,
        };
        let symbol = spec.legs.first().map(|leg| leg.symbol.clone()).unwrap_or_default();
        self.submit(id, &symbol, &intent).await
    }

    async fn submit(&mut self, id: &str, symbol: &str, intent: &OrderIntent) -> StepFate {
        if let Err(err) = intent.validate_for(&self.caps) {
            return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string(), "symbol": symbol }));
        }
        if !self.have_portfolio {
            if let Err(err) = self.ensure_portfolio().await {
                return StepFate::Retry(err.to_string());
            }
        }
        if !self.paper {
            if let Err(err) = self.safety.check(self.account, intent, self.equity) {
                return StepFate::Done(json!({ "id": id, "status": "rejected", "reason": err.to_string(), "symbol": symbol }));
            }
        }
        let preview = order_preview(symbol, intent);
        if self.flags.dry_run || self.flags.simulate {
            let mut row = preview;
            row["id"] = json!(id);
            row["status"] = json!(if self.flags.simulate { "simulated" } else { "dry_run" });
            self.bindings.insert(id.to_string(), row.clone());
            return StepFate::Done(row);
        }
        if let Err(err) = require_trading_approval(self.flags, "workflow run") {
            return StepFate::Retry(err.to_string());
        }
        if let Err(err) = self.ensure_lock() {
            return StepFate::Retry(err.to_string());
        }
        let preflight = match self.trading.preflight(self.account, intent).await {
            Ok(preflight) => preflight,
            Err(err) => return StepFate::Retry(err.to_string()),
        };
        if !preflight.accepted {
            return StepFate::Done(json!({
                "id": id,
                "status": "rejected",
                "reason": preflight.message,
                "symbol": symbol,
            }));
        }
        match self.trading.place(self.account, intent, &uuid::Uuid::new_v4().to_string()).await {
            Err(err) => StepFate::Retry(err.to_string()),
            Ok(placed) => {
                let row = json!({
                    "id": id,
                    "status": "submitted",
                    "order_id": placed.order_id,
                    "symbol": symbol,
                    "broker_status": format!("{:?}", placed.status),
                });
                self.bindings.insert(id.to_string(), row.clone());
                StepFate::Done(row)
            }
        }
    }

    async fn cancel_order(&mut self, id: &str, path: &str) -> StepFate {
        let Some(order_id) = lookup(&self.bindings, path).and_then(|value| value.as_str().map(str::to_string)) else {
            return StepFate::Done(json!({ "id": id, "status": "skipped", "reason": format!("missing {path}") }));
        };
        if self.flags.dry_run || self.flags.simulate {
            return StepFate::Done(json!({ "id": id, "status": "dry_run", "cancel": order_id }));
        }
        if let Err(err) = require_trading_approval(self.flags, "workflow run") {
            return StepFate::Retry(err.to_string());
        }
        if let Err(err) = self.ensure_lock() {
            return StepFate::Retry(err.to_string());
        }
        match self.trading.cancel(self.account, &order_id).await {
            Err(err) => StepFate::Retry(err.to_string()),
            Ok(()) => StepFate::Done(json!({ "id": id, "status": "submitted", "cancelled": order_id, "symbol": "" })),
        }
    }

    async fn wait_order(&mut self, id: &str, path: &str, until: WaitUntil, timeout: u64, poll: u64) -> StepFate {
        let Some(order_id) = lookup(&self.bindings, path).and_then(|value| {
            value.as_str().map(str::to_string).or_else(|| value.get("order_id").and_then(|item| item.as_str()).map(str::to_string))
        }) else {
            return StepFate::Done(json!({ "id": id, "status": "skipped", "reason": format!("missing {path}") }));
        };
        if self.flags.dry_run || self.flags.simulate {
            return StepFate::Done(json!({ "id": id, "status": "dry_run", "wait": order_id }));
        }
        match poll_order(self.trading, self.account, &order_id, WaitOptions {
            until,
            timeout_seconds: timeout,
            poll_interval_seconds: poll,
            proceed_on_partial: false,
        }).await {
            Err(err) => StepFate::Done(json!({ "id": id, "status": "timeout", "reason": err.to_string() })),
            Ok(order) => StepFate::Done(json!({ "id": id, "status": "ok", "order_id": order.id, "broker_status": format!("{:?}", order.status) })),
        }
    }

    fn ensure_lock(&mut self) -> Result<()> {
        if !self.lock || self.guard.is_some() {
            return Ok(());
        }
        let rules_dir = self.rules_path.parent().unwrap_or(Path::new("."));
        if let Some(reason) = legacy_account_busy(rules_dir, &self.account.id)? {
            bail!(reason);
        }
        let dir = state_dir()?.join("locks");
        self.guard = Some(AccountLock::acquire(&dir, &self.account.address())?);
        Ok(())
    }

    async fn ensure_portfolio(&mut self) -> Result<()> {
        if self.have_portfolio {
            return Ok(());
        }
        let portfolio = self.trading.portfolio(self.account).await?;
        self.cash = portfolio.balances.cash_buying_power;
        self.equity = portfolio.balances.equity;
        self.options_bp = portfolio.balances.options_buying_power;
        self.positions = portfolio.positions;
        self.have_portfolio = true;
        Ok(())
    }

    fn bind_portfolio(&mut self, id: &str) {
        let positions: Vec<_> = self
            .positions
            .iter()
            .map(|position| {
                json!({
                    "symbol": position.symbol,
                    "quantity": position.quantity,
                    "average_price": position.average_price,
                    "market_value": position.market_value,
                })
            })
            .collect();
        self.bindings.insert(id.to_string(), json!({
            "cash_buying_power": self.cash,
            "options_buying_power": self.options_bp,
            "equity": self.equity,
            "positions": positions,
        }));
    }

    fn position_row(&self, symbol: &str) -> Value {
        let found = self.positions.iter().find(|position| position.symbol.eq_ignore_ascii_case(symbol));
        let quantity = found.map(|position| position.quantity).unwrap_or(0.0);
        let average = found.and_then(|position| position.average_price);
        let unrealized = average.and_then(|average| {
            self.quote_last(symbol).map(|last| if average == 0.0 { 0.0 } else { (last - average) / average * 100.0 })
        });
        json!({
            "symbol": symbol,
            "quantity": quantity,
            "average_price": average,
            "unrealized_pct": unrealized,
        })
    }

    fn quote_last(&self, symbol: &str) -> Option<f64> {
        self.quotes.get(symbol).and_then(|quote| quote.last.or(quote.bid))
    }

    async fn year_bars(&mut self, symbol: &str) -> Result<Vec<Bar>> {
        let key = format!("{symbol}|year");
        if let Some(bars) = self.bars.get(&key) {
            return Ok(bars.clone());
        }
        let bars = sort_bars(self.data.bars(self.account, symbol, "year").await?);
        self.bars.insert(key, bars.clone());
        Ok(bars)
    }

    fn resolve_number(&self, value: &serde_yaml::Value) -> Result<f64> {
        if let Some(number) = yaml_f64(value) {
            return Ok(number);
        }
        let path = value.as_str().context("expected a number or a binding path")?;
        lookup(&self.bindings, path)
            .and_then(as_number)
            .with_context(|| format!("missing {path}"))
    }
}

fn order_symbol(order: &OrderKind) -> Option<String> {
    match order {
        OrderKind::Equity(spec) => Some(spec.symbol.clone()),
        OrderKind::ExitBracket(spec) => Some(spec.symbol.clone()),
        OrderKind::Spread(spec) => spec.legs.first().map(|leg| leg.symbol.clone()),
        OrderKind::Cancel { .. } | OrderKind::Wait { .. } => None,
    }
}

fn order_preview(symbol: &str, intent: &OrderIntent) -> Value {
    match intent {
        OrderIntent::Equity { side, size, limit, .. } => {
            let side = match side {
                Side::Buy => "buy",
                Side::Sell => "sell",
            };
            match size {
                EquitySize::Notional(amount) => json!({
                    "symbol": symbol, "side": side, "notional_usd": amount, "order_type": if limit.is_some() { "limit" } else { "market" },
                }),
                EquitySize::Shares(quantity) => json!({
                    "symbol": symbol, "side": side, "quantity": quantity, "order_type": if limit.is_some() { "limit" } else { "market" },
                }),
            }
        }
        OrderIntent::ExitBracket(bracket) => json!({
            "symbol": bracket.symbol, "kind": "exit_bracket", "quantity": bracket.quantity,
        }),
        OrderIntent::Spread { strategy, contracts, .. } => json!({
            "symbol": symbol, "kind": "spread", "strategy": format!("{strategy:?}"), "contracts": contracts,
        }),
    }
}

fn build_bracket(
    symbol: &str,
    side: Side,
    quantity: f64,
    reference: Option<f64>,
    bracket: &BracketYaml,
    bindings: &Map<String, Value>,
) -> Result<ExitBracket> {
    if quantity <= 0.0 {
        bail!("bracket needs a share quantity or a quote to size a notional order");
    }
    let reference = reference.context("bracket percent needs a limit price or a quote")?;
    let take_profit = bracket_price(reference, side, true, bracket.take_profit.as_ref(), bracket.take_profit_pct, bindings)?;
    let stop = bracket_price(reference, side, false, bracket.stop.as_ref(), bracket.stop_pct, bindings)?;
    Ok(ExitBracket {
        symbol: symbol.to_string(),
        quantity,
        take_profit,
        stop,
        stop_limit: stop * 0.995,
        tif: TimeInForce::Gtc,
    })
}

fn bracket_price(
    reference: f64,
    side: Side,
    is_target: bool,
    price: Option<&serde_yaml::Value>,
    percent: Option<f64>,
    bindings: &Map<String, Value>,
) -> Result<f64> {
    if let Some(price) = price {
        if let Some(number) = yaml_f64(price) {
            return Ok(number);
        }
        let path = price.as_str().context("bracket price")?;
        return lookup(bindings, path).and_then(as_number).with_context(|| format!("missing {path}"));
    }
    let Some(percent) = percent else {
        bail!("bracket needs a price or a percent");
    };
    let buy = matches!(side, Side::Buy);
    let up = if is_target { buy } else { !buy };
    if up {
        Ok(reference * (1.0 + percent / 100.0))
    } else {
        Ok(reference * (1.0 - percent / 100.0))
    }
}

struct CheckEnv<'a> {
    bindings: &'a Map<String, Value>,
    positions: &'a [Position],
    cash: Option<f64>,
    session_open: Option<bool>,
    spread: Option<f64>,
    last_fill: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
}

fn first_failure(checks: &[Check], env: &CheckEnv<'_>) -> Option<String> {
    for check in checks {
        if let Some(reason) = check_one(check, env) {
            return Some(reason);
        }
    }
    None
}

fn check_one(check: &Check, env: &CheckEnv<'_>) -> Option<String> {
    match check {
        Check::SessionOpen(want) => match env.session_open {
            Some(open) if open == *want => None,
            Some(_) => Some("session gate".into()),
            None => Some("missing session".into()),
        },
        Check::BuyingPower(need) => match env.cash {
            Some(cash) if cash + 1e-9 >= *need => None,
            Some(cash) => Some(format!("buying power {cash:.2} is below {need:.2}")),
            None => Some("missing portfolio".into()),
        },
        Check::MinDays(days) => match env.last_fill {
            None => None,
            Some(fill) => {
                let ready = fill + Duration::days(*days as i64);
                if env.now >= ready {
                    None
                } else {
                    Some(format!("last fill is inside {days} days"))
                }
            }
        },
        Check::SpreadPct(max) => match env.spread {
            Some(spread) if spread <= *max => None,
            Some(spread) => Some(format!("spread {spread:.2}% is above {max:.2}%")),
            None => Some("missing spread".into()),
        },
        Check::Position { symbol, cmp, value } => {
            let quantity = env
                .positions
                .iter()
                .find(|position| position.symbol.eq_ignore_ascii_case(symbol))
                .map(|position| position.quantity)
                .unwrap_or(0.0);
            if compare(quantity, *cmp, *value) {
                None
            } else {
                Some(format!("{symbol} quantity {quantity} failed the position check"))
            }
        }
        Check::Cmp { op, left, right } => {
            let (Some(left_n), Some(right_n)) = (resolve_ref(env.bindings, left), resolve_ref(env.bindings, right)) else {
                return Some(format!("missing {} or {}", ref_name(left), ref_name(right)));
            };
            if compare(left_n, *op, right_n) {
                None
            } else {
                Some(format!("{} is not {} {}", ref_name(left), cmp_word(*op), ref_name(right)))
            }
        }
        Check::Between { probe, low, high } => {
            let (Some(probe_n), Some(low_n), Some(high_n)) = (
                resolve_ref(env.bindings, probe),
                resolve_ref(env.bindings, low),
                resolve_ref(env.bindings, high),
            ) else {
                return Some(format!("missing {}", ref_name(probe)));
            };
            if probe_n >= low_n && probe_n <= high_n {
                None
            } else {
                Some(format!("{} is outside the range", ref_name(probe)))
            }
        }
    }
}

fn compare(left: f64, op: Cmp, right: f64) -> bool {
    match op {
        Cmp::Gt => left > right,
        Cmp::Gte => left >= right,
        Cmp::Lt => left < right,
        Cmp::Lte => left <= right,
        Cmp::Eq => (left - right).abs() <= 1e-9,
    }
}

fn cmp_word(op: Cmp) -> &'static str {
    match op {
        Cmp::Gt => "above",
        Cmp::Gte => "at or above",
        Cmp::Lt => "below",
        Cmp::Lte => "at or below",
        Cmp::Eq => "equal to",
    }
}

fn resolve_ref(bindings: &Map<String, Value>, reference: &Ref) -> Option<f64> {
    match reference {
        Ref::Number(number) => Some(*number),
        Ref::Path(path) => lookup(bindings, path).and_then(as_number),
    }
}

fn ref_name(reference: &Ref) -> String {
    match reference {
        Ref::Number(number) => number.to_string(),
        Ref::Path(path) => path.clone(),
    }
}

fn lookup<'a>(bindings: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let root = parts.next()?;
    let mut current = bindings.get(root)?;
    for part in parts {
        current = if let Ok(index) = part.parse::<usize>() {
            current.get(index)?
        } else {
            current.get(part)?
        };
    }
    Some(current)
}

fn as_number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_i64().map(|number| number as f64)).or_else(|| value.as_u64().map(|number| number as f64))
}

fn sort_bars(mut bars: Vec<Bar>) -> Vec<Bar> {
    bars.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    bars
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use super::ema;
    use broker_core::{BrokerKind, Capabilities, MockBroker, Quote, SessionStatus};
    use chrono::TimeZone;

    fn pass_bindings() -> Map<String, Value> {
        let mut bindings = Map::new();
        bindings.insert("quote".into(), json!({ "last": 100.0, "bid": 99.9, "ask": 100.1 }));
        bindings.insert("ema200".into(), json!(90.0));
        bindings.insert("ema20".into(), json!(110.0));
        bindings
    }

    fn dip_checks() -> Vec<Check> {
        let when: WhenYaml = serde_yaml::from_str(
            r#"
all:
  - session_open: true
  - gt: ["quote.last", "ema200"]
  - lte: ["quote.last", "ema20"]
  - min_days_since_fill: 7
  - buying_power_at_least: 250
"#,
        )
        .unwrap();
        when.all.iter().map(parse_check).collect::<Result<Vec<_>>>().unwrap()
    }

    fn env<'a>(
        bindings: &'a Map<String, Value>,
        positions: &'a [Position],
        cash: Option<f64>,
        session_open: Option<bool>,
        last_fill: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> CheckEnv<'a> {
        CheckEnv {
            bindings,
            positions,
            cash,
            session_open,
            spread: Some(0.1),
            last_fill,
            now,
        }
    }

    #[test]
    fn dip_predicate_requires_trend_pullback_session_and_cooldown() {
        let checks = dip_checks();
        let bindings = pass_bindings();
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 15, 0, 0).unwrap();
        let positions = Vec::new();
        let ready = env(&bindings, &positions, Some(1_000.0), Some(true), Some(now - Duration::days(8)), now);
        assert!(first_failure(&checks, &ready).is_none());
        let first = env(&bindings, &positions, Some(1_000.0), Some(true), None, now);
        assert!(first_failure(&checks, &first).is_none());

        let closed = env(&bindings, &positions, Some(1_000.0), Some(false), None, now);
        assert!(first_failure(&checks, &closed).is_some());

        let mut cheap = pass_bindings();
        cheap.insert("quote".into(), json!({ "last": 80.0 }));
        let below = env(&cheap, &positions, Some(1_000.0), Some(true), None, now);
        assert!(first_failure(&checks, &below).unwrap().contains("quote.last"));

        let mut extended = pass_bindings();
        extended.insert("quote".into(), json!({ "last": 120.0 }));
        let above = env(&extended, &positions, Some(1_000.0), Some(true), None, now);
        assert!(first_failure(&checks, &above).is_some());

        let recent = env(&bindings, &positions, Some(1_000.0), Some(true), Some(now - Duration::days(3)), now);
        assert!(first_failure(&checks, &recent).unwrap().contains("last fill"));

        let poor = env(&bindings, &positions, Some(100.0), Some(true), None, now);
        assert!(first_failure(&checks, &poor).unwrap().contains("buying power"));
    }

    #[test]
    fn notional_order_fails_schwab_capabilities() {
        let intent = OrderIntent::Equity {
            side: Side::Buy,
            symbol: "MSFT".into(),
            size: EquitySize::Notional(250.0),
            limit: None,
            tif: TimeInForce::Day,
            session: Session::Core,
            bracket: None,
        };
        assert!(intent.validate_for(&Capabilities::schwab()).is_err());
    }

    #[test]
    fn example_file_validates() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules/workflows/example-msft-dip.yaml");
        let report = validate_report(&path).unwrap();
        assert_eq!(report["ok"], json!(true));
        assert_eq!(report["workflows"][0]["id"], json!("msft-dip"));
        assert!(report["warnings"][0].as_str().unwrap().contains("ACCOUNT"));
    }

    #[test]
    fn prompt_says_a_workflow_is_allowed() {
        let prompt = crate::instructions::workflow_prompt();
        let capability = prompt["capability"].as_str().unwrap();
        assert!(capability.contains("continuous workflow is allowed"));
        assert!(capability.contains("rules/workflows/"));
        let schema = crate::instructions::workflow_schema();
        for kind in ["quote", "ema", "rsi", "atr", "spread", "exit_bracket", "cancel"] {
            let text = schema.to_string();
            assert!(text.contains(kind), "{kind} missing from schema");
        }
    }

    struct Tape {
        last: f64,
        bars: Vec<Bar>,
    }

    #[async_trait]
    impl MarketData for Tape {
        fn broker_name(&self) -> &'static str {
            "tape"
        }
        async fn quotes(&self, _: &AccountRef, symbols: &[String]) -> Result<Vec<Quote>> {
            Ok(symbols.iter().map(|symbol| Quote {
                symbol: symbol.clone(),
                bid: Some(self.last - 0.05),
                ask: Some(self.last + 0.05),
                last: Some(self.last),
                bid_size: Some(1.0),
                ask_size: Some(1.0),
                volume: Some(1.0),
            }).collect())
        }
        async fn option_expirations(&self, _: &AccountRef, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn option_chain(&self, _: &AccountRef, underlying: &str, expiration: &str) -> Result<broker_core::OptionChain> {
            Ok(broker_core::OptionChain { underlying: underlying.into(), expirations: vec![expiration.into()], contracts: vec![] })
        }
        async fn greeks(&self, _: &AccountRef, _: &str) -> Result<broker_core::Greeks> {
            Ok(broker_core::Greeks::default())
        }
        async fn strategy_quote(&self, _: &AccountRef, _: &[SpreadLeg]) -> Result<Quote> {
            anyhow::bail!("no strategy quote")
        }
        async fn bars(&self, _: &AccountRef, _: &str, _: &str) -> Result<Vec<Bar>> {
            Ok(self.bars.clone())
        }
        async fn session_open(&self, _: &AccountRef) -> Result<SessionStatus> {
            Ok(SessionStatus { open: true, source: "tape".into() })
        }
    }

    fn rising_bars() -> Vec<Bar> {
        let start = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        (0..40).map(|i| {
            let close = (i + 1) as f64;
            Bar {
                timestamp: (start + chrono::Duration::days(i)).format("%Y-%m-%d").to_string(),
                open: close,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 1_000.0,
            }
        }).collect()
    }

    fn dip_file(short: &str, long: &str) -> String {
        format!(
            r#"
version: 1
broker: public
account: "public:paper"
workflows:
  - id: dip
    schedule:
      cron: "* * * * *"
      timezone: America/New_York
    steps:
      - id: quote
        fetch: quote
        symbol: MSFT
      - id: {short}
        fetch: ema
        symbol: MSFT
        period: 5
      - id: {long}
        fetch: ema
        symbol: MSFT
        period: 20
      - id: buy
        when:
          all:
            - session_open: true
            - gt: ["quote.last", "{long}"]
            - lte: ["quote.last", "{short}"]
        order:
          side: buy
          symbol: MSFT
          notional_usd: 250
          order_type: market
"#
        )
    }

    fn safety() -> SafetyGate {
        let mut safety = SafetyGate::default();
        safety.public_license_ack = true;
        safety
    }

    fn flags(dry_run: bool) -> TradingFlags {
        TradingFlags { yes: true, trust: true, dry_run, simulate: false }
    }

    #[tokio::test]
    async fn dry_run_does_not_place_and_the_same_slot_orders_once() {
        let bars = rising_bars();
        let closes: Vec<_> = bars.iter().map(|bar| bar.close).collect();
        let short = ema(&closes, 5).unwrap();
        let long = ema(&closes, 20).unwrap();
        assert!(short > long);
        let mid = (short + long) / 2.0;
        let file = parse_workflow(&dip_file("ema5", "ema20")).unwrap();
        let account = AccountRef::new(BrokerKind::Public, "paper");
        let broker = MockBroker::with_cash(10_000.0);
        let tape = Tape { last: mid, bars };
        let dir = std::env::temp_dir().join(format!("workflow-slot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("state.json");
        let rules = dir.join("rules.yaml");
        std::fs::write(&rules, "version: 1\n").unwrap();
        let now = Utc::now();

        let dry = run_due(&file, &broker, &tape, &account, &safety(), &flags(true), &state, &rules, now, false).await.unwrap();
        assert_eq!(dry["workflows"][0]["status"], json!("dry_run"));
        assert_eq!(dry["workflows"][0]["steps"][3]["notional_usd"], json!(250.0));
        assert!(broker.orders().is_empty());

        let live = run_due(&file, &broker, &tape, &account, &safety(), &flags(false), &state, &rules, now, false).await.unwrap();
        assert_eq!(live["workflows"][0]["status"], json!("submitted"));
        assert_eq!(broker.orders().len(), 1);

        let again = run_due(&file, &broker, &tape, &account, &safety(), &flags(false), &state, &rules, now, false).await.unwrap();
        assert_eq!(again["workflows"][0]["status"], json!("already_ran"));
        assert_eq!(broker.orders().len(), 1);

        let high = Tape { last: short + 1.0, bars: rising_bars() };
        let other = dir.join("other.json");
        let skipped = run_due(&file, &broker, &high, &account, &safety(), &flags(false), &other, &rules, now, false).await.unwrap();
        assert_eq!(skipped["workflows"][0]["status"], json!("skipped"));
        assert_eq!(broker.orders().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
