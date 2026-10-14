use std::path::Path;

use anyhow::{bail, Context, Result};
use broker_core::{
    parse_account_address, AccountRef, BrokerKind, BrokerRegistry, DataSource, EquitySize,
    OrderIntent, OrderStatus, Session, Side, TimeInForce, WaitOptions, WaitUntil,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::guard::{legacy_account_busy, AccountLock};
use crate::paths::state_dir;
use crate::safety::{require_trading_approval, SafetyGate, TradingFlags};

#[derive(Debug, Deserialize)]
pub struct TradePlan {
    pub version: u32,
    pub plan_id: String,
    #[serde(default)]
    pub broker: Option<String>,
    #[serde(default)]
    pub account_hash: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub data_source: Option<String>,
    #[serde(default)]
    pub execution: Execution,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
pub struct Step {
    pub id: String,
    pub side: String,
    pub symbol: String,
    pub quantity: f64,
    #[serde(default = "limit_default")]
    pub order_type: String,
    #[serde(default)]
    pub limit_price: Option<f64>,
}

fn limit_default() -> String {
    "limit".into()
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Execution {
    pub stop_on_error: bool,
    pub wait_for_fill: bool,
    pub fill_timeout_seconds: u64,
    pub poll_interval_seconds: u64,
}

impl Default for Execution {
    fn default() -> Self {
        Self {
            stop_on_error: true,
            wait_for_fill: false,
            fill_timeout_seconds: 3600,
            poll_interval_seconds: 5,
        }
    }
}

impl TradePlan {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = std::fs::read_to_string(path.as_ref())?;
        let plan: Self = serde_yaml::from_str(&raw)?;
        if plan.version != 1 {
            bail!("unsupported plan version {}", plan.version);
        }
        if plan.steps.is_empty() {
            bail!("plan has no steps");
        }
        Ok(plan)
    }

    pub fn account(&self) -> Result<AccountRef> {
        if let Some(address) = &self.account {
            return parse_account_address(address);
        }
        let id = self
            .account_hash
            .as_deref()
            .context("plan needs account or account_hash")?;
        let mut account = parse_account_address(id)?;
        if let Some(broker) = &self.broker {
            account.broker = BrokerKind::parse(broker)?;
            if !id.contains(':') {
                account.id = id.to_string();
            }
        }
        Ok(account)
    }
}

pub async fn run_plan(
    registry: &BrokerRegistry,
    plan: &TradePlan,
    flags: &TradingFlags,
) -> Result<Value> {
    let account = plan.account()?;
    let source = DataSource::parse(plan.data_source.as_deref(), account.broker)?;
    let handle = registry.resolve(&account, source)?;
    let safety = SafetyGate::load()?;
    let portfolio = handle.trading.portfolio(&account).await?;
    let mut results = Vec::new();
    let _lock = if flags.dry_run || flags.simulate {
        None
    } else {
        if let Some(reason) = legacy_account_busy(Path::new("rules"), &account.id)? {
            bail!("{reason}");
        }
        Some(AccountLock::acquire(&state_dir()?.join("locks"), &account.address())?)
    };

    for step in &plan.steps {
        let intent = step_intent(step)?;
        safety.check(&account, &intent, portfolio.balances.equity)?;
        if matches!(step.side.to_ascii_lowercase().as_str(), "buy") {
            let cost = intent.estimated_notional();
            if cost > portfolio.balances.cash_buying_power && !flags.dry_run {
                bail!(
                    "step {} needs {cost:.2} cash and the account has {:.2}",
                    step.id,
                    portfolio.balances.cash_buying_power
                );
            }
        }
        let preflight = handle.trading.preflight(&account, &intent).await?;
        if flags.dry_run || flags.simulate {
            results.push(json!({
                "id": step.id,
                "status": if flags.simulate { "simulated" } else { "dry_run" },
                "preflight": preflight,
            }));
            continue;
        }
        require_trading_approval(flags, "plan run")?;
        if !preflight.accepted {
            let message = format!("step {} preflight rejected: {}", step.id, preflight.message);
            if plan.execution.stop_on_error {
                bail!(message);
            }
            results.push(json!({"id": step.id, "status": "rejected", "message": message}));
            continue;
        }
        let client_id = uuid::Uuid::new_v4().to_string();
        let placed = handle
            .trading
            .place(&account, &intent, &client_id)
            .await?;
        let mut status = placed.status;
        if plan.execution.wait_for_fill && !placed.order_id.is_empty() {
            let order = broker_core::poll_order(
                &*handle.trading,
                &account,
                    &placed.order_id,
                    WaitOptions {
                        until: WaitUntil::Filled,
                        timeout_seconds: plan.execution.fill_timeout_seconds,
                        poll_interval_seconds: plan.execution.poll_interval_seconds,
                        proceed_on_partial: false,
                    },
                )
                .await?;
            status = order.status;
            if status != OrderStatus::Filled && plan.execution.stop_on_error {
                bail!("step {} ended {:?}", step.id, status);
            }
        }
        results.push(json!({
            "id": step.id,
            "order_id": placed.order_id,
            "status": format!("{status:?}"),
        }));
    }
    Ok(json!({"plan_id": plan.plan_id, "account": account.address(), "steps": results}))
}

fn step_intent(step: &Step) -> Result<OrderIntent> {
    let side = match step.side.to_ascii_lowercase().as_str() {
        "buy" => Side::Buy,
        "sell" => Side::Sell,
        other => bail!("step {} has unknown side {other}", step.id),
    };
    let limit = match step.order_type.to_ascii_lowercase().as_str() {
        "market" => None,
        "limit" => Some(step.limit_price.context("limit price required")?),
        other => bail!("step {} has unknown order_type {other}", step.id),
    };
    Ok(OrderIntent::Equity {
        side,
        symbol: step.symbol.clone(),
        size: EquitySize::Shares(step.quantity),
        limit,
        tif: TimeInForce::Day,
        session: Session::Core,
        bracket: None,
    })
}
