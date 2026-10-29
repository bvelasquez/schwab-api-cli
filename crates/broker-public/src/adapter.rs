use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use broker_core::{
    local_session_status, AccountKind, AccountRef, AssetKind, Balances, Bar, Broker, Capabilities,
    Greeks, LegInstruction, MarketData, OptionChain, Order, OrderIntent, OrderStatus, PlaceResult,
    Portfolio, Preflight, Quote, SessionStatus, SpreadLeg,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::client::PublicClient;
use crate::parse::{
    accounts_from, bars_from, chain_from, expirations_from, greeks_from, order_from, orders_from,
    portfolio_from, preflight_from, quotes_from,
};
use crate::payload::{exit_bracket_body, multileg_body, single_leg_body};

pub struct PublicBackend {
    client: PublicClient,
    default_account_id: String,
    chain_cache: Mutex<HashMap<(String, String), (Instant, OptionChain)>>,
}

impl PublicBackend {
    pub fn from_env() -> Result<Self> {
        let default_account_id = std::env::var("PUBLIC_ACCOUNT_ID").unwrap_or_default();
        Ok(Self {
            client: PublicClient::from_env()?,
            default_account_id,
            chain_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Read-only backend for the research-paper arm. Trading writes are refused
    /// at the HTTP boundary in addition to the `PaperOnlyBroker` wrapper.
    pub fn from_env_read_only() -> Result<Self> {
        let default_account_id = std::env::var("PUBLIC_ACCOUNT_ID").unwrap_or_default();
        Ok(Self {
            client: PublicClient::from_env_read_only()?,
            default_account_id,
            chain_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn is_read_only(&self) -> bool {
        self.client.is_read_only()
    }

    pub fn into_arcs(self) -> (Arc<dyn Broker>, Arc<dyn MarketData>) {
        let backend = Arc::new(self);
        let trading: Arc<dyn Broker> = backend.clone();
        let data: Arc<dyn MarketData> = backend;
        (trading, data)
    }

    fn account_id<'a>(&'a self, account: &'a AccountRef) -> Result<&'a str> {
        if !account.id.is_empty() {
            return Ok(account.id.as_str());
        }
        if self.default_account_id.is_empty() {
            anyhow::bail!("Public account id missing; set PUBLIC_ACCOUNT_ID or public:<id>");
        }
        Ok(self.default_account_id.as_str())
    }

    async fn use_margin(&self, account: &AccountRef) -> bool {
        if account.kind == AccountKind::Cash {
            return false;
        }
        match self.portfolio(account).await {
            Ok(portfolio) => portfolio.kind != AccountKind::Cash,
            Err(_) => account.kind != AccountKind::Cash,
        }
    }
}

#[async_trait]
impl Broker for PublicBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities::public()
    }

    async fn accounts(&self) -> Result<Vec<AccountRef>> {
        let raw = self.client.get_json("/userapigateway/trading/account").await?;
        let mut accounts = accounts_from(&raw);
        if accounts.is_empty() && !self.default_account_id.is_empty() {
            accounts.push(AccountRef::new(
                broker_core::BrokerKind::Public,
                self.default_account_id.clone(),
            ));
        }
        Ok(accounts)
    }

    async fn portfolio(&self, account: &AccountRef) -> Result<Portfolio> {
        let id = self.account_id(account)?;
        let raw = self
            .client
            .get_json(&format!("/userapigateway/trading/{id}/portfolio/v2"))
            .await?;
        Ok(portfolio_from(&raw, account))
    }

    async fn balances(&self, account: &AccountRef) -> Result<Balances> {
        Ok(self.portfolio(account).await?.balances)
    }

    async fn open_orders(&self, account: &AccountRef) -> Result<Vec<Order>> {
        let portfolio = self.portfolio(account).await?;
        let id = self.account_id(account)?;
        let raw = self
            .client
            .get_json(&format!("/userapigateway/trading/{id}/portfolio/v2"))
            .await?;
        let _ = portfolio;
        Ok(orders_from(&raw)
            .into_iter()
            .filter(|order| !order.status.is_terminal())
            .collect())
    }

    async fn order_history(&self, account: &AccountRef, _since: DateTime<Utc>) -> Result<Vec<Order>> {
        let id = self.account_id(account)?;
        let raw = self
            .client
            .get_json(&format!("/userapigateway/trading/{id}/portfolio/v2"))
            .await?;
        Ok(orders_from(&raw))
    }

    async fn preflight(&self, account: &AccountRef, intent: &OrderIntent) -> Result<Preflight> {
        intent.validate_for(&self.capabilities())?;
        let id = self.account_id(account)?;
        let margin = self.use_margin(account).await;
        let (path, body) = match intent {
            OrderIntent::Spread { .. } => (
                format!("/userapigateway/trading/{id}/preflight/multi-leg"),
                multileg_body(intent, None, margin)?,
            ),
            OrderIntent::Equity { .. } => (
                format!("/userapigateway/trading/{id}/preflight/single-leg"),
                single_leg_body(intent, None, margin)?,
            ),
            OrderIntent::ExitBracket(_) => {
                return Ok(Preflight {
                    accepted: true,
                    estimated_cost: Some(intent.estimated_notional()),
                    estimated_proceeds: None,
                    buying_power_requirement: None,
                    fees: None,
                    message: "exit bracket is attached on the Public order".into(),
                });
            }
        };
        match self.client.post_json(&path, &body).await {
            Ok(raw) => Ok(preflight_from(&raw)),
            Err(err) => Ok(Preflight {
                accepted: false,
                estimated_cost: None,
                estimated_proceeds: None,
                buying_power_requirement: None,
                fees: None,
                message: err.to_string(),
            }),
        }
    }

    async fn place(
        &self,
        account: &AccountRef,
        intent: &OrderIntent,
        client_id: &str,
    ) -> Result<PlaceResult> {
        intent.validate_for(&self.capabilities())?;
        let id = self.account_id(account)?;
        let margin = self.use_margin(account).await;
        let order_id = if client_id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            client_id.to_string()
        };
        let (path, body) = match intent {
            OrderIntent::Spread { .. } => (
                format!("/userapigateway/trading/{id}/order/multileg"),
                multileg_body(intent, Some(&order_id), margin)?,
            ),
            OrderIntent::Equity { .. } => (
                format!("/userapigateway/trading/{id}/order"),
                single_leg_body(intent, Some(&order_id), margin)?,
            ),
            OrderIntent::ExitBracket(bracket) => (
                format!("/userapigateway/trading/{id}/order"),
                exit_bracket_body(bracket, &order_id, margin),
            ),
        };
        let raw = self.client.post_json(&path, &body).await?;
        let returned = raw
            .get("orderId")
            .and_then(|value| value.as_str())
            .unwrap_or(&order_id)
            .to_string();
        Ok(PlaceResult {
            order_id: returned,
            status: OrderStatus::Accepted,
            message: "Public accepted the submission; confirm with get_order".into(),
        })
    }

    async fn get_order(&self, account: &AccountRef, order_id: &str) -> Result<Order> {
        let id = self.account_id(account)?;
        let raw = self
            .client
            .get_json(&format!("/userapigateway/trading/{id}/order/{order_id}"))
            .await?;
        order_from(&raw).context("Public order response missing orderId")
    }

    async fn cancel(&self, account: &AccountRef, order_id: &str) -> Result<()> {
        let id = self.account_id(account)?;
        self.client
            .delete(&format!("/userapigateway/trading/{id}/order/{order_id}"))
            .await?;
        Ok(())
    }

    async fn replace(
        &self,
        account: &AccountRef,
        order_id: &str,
        intent: &OrderIntent,
    ) -> Result<PlaceResult> {
        let id = self.account_id(account)?;
        let margin = self.use_margin(account).await;
        let body = match intent {
            OrderIntent::Spread { .. } => multileg_body(intent, Some(order_id), margin)?,
            OrderIntent::Equity { .. } => single_leg_body(intent, Some(order_id), margin)?,
            OrderIntent::ExitBracket(bracket) => exit_bracket_body(bracket, order_id, margin),
        };
        self.client
            .put_json(&format!("/userapigateway/trading/{id}/order/{order_id}"), &body)
            .await?;
        let confirmed = self.get_order(account, order_id).await?;
        Ok(PlaceResult {
            order_id: confirmed.id,
            status: confirmed.status,
            message: "Public replace is async; status is from a follow-up GET".into(),
        })
    }
}

#[async_trait]
impl MarketData for PublicBackend {
    fn broker_name(&self) -> &'static str {
        "public"
    }

    async fn quotes(&self, account: &AccountRef, symbols: &[String]) -> Result<Vec<Quote>> {
        let id = self.account_id(account)?;
        let instruments: Vec<Value> = symbols
            .iter()
            .map(|symbol| json!({"symbol": symbol, "type": "EQUITY"}))
            .collect();
        let raw = self
            .client
            .post_json(
                &format!("/userapigateway/marketdata/{id}/quotes"),
                &json!({"instruments": instruments}),
            )
            .await?;
        Ok(quotes_from(&raw))
    }

    async fn option_expirations(&self, account: &AccountRef, underlying: &str) -> Result<Vec<String>> {
        let id = self.account_id(account)?;
        let raw = self
            .client
            .post_json(
                &format!("/userapigateway/marketdata/{id}/option-expirations"),
                &json!({"instrument": {"symbol": underlying, "type": "EQUITY"}}),
            )
            .await?;
        Ok(expirations_from(&raw))
    }

    async fn option_chain(
        &self,
        account: &AccountRef,
        underlying: &str,
        expiration: &str,
    ) -> Result<OptionChain> {
        let key = (underlying.to_uppercase(), expiration.to_string());
        {
            let cache = self.chain_cache.lock().await;
            if let Some((stored, chain)) = cache.get(&key) {
                if stored.elapsed() < Duration::from_secs(30) {
                    return Ok(chain.clone());
                }
            }
        }
        let id = self.account_id(account)?;
        let raw = self
            .client
            .post_json(
                &format!("/userapigateway/marketdata/{id}/option-chain"),
                &json!({
                    "instrument": {"symbol": underlying, "type": "EQUITY"},
                    "expirationDate": expiration
                }),
            )
            .await?;
        let chain = chain_from(underlying, expiration, &raw);
        self.chain_cache
            .lock()
            .await
            .insert(key, (Instant::now(), chain.clone()));
        Ok(chain)
    }

    async fn greeks(&self, account: &AccountRef, osi_symbol: &str) -> Result<Greeks> {
        let id = self.account_id(account)?;
        let raw = self
            .client
            .post_json(
                &format!("/userapigateway/marketdata/{id}/quotes"),
                &json!({"instruments": [{"symbol": osi_symbol, "type": "OPTION"}]}),
            )
            .await?;
        Ok(greeks_from(&raw))
    }

    async fn strategy_quote(&self, account: &AccountRef, legs: &[SpreadLeg]) -> Result<Quote> {
        let id = self.account_id(account)?;
        let base_symbol = legs
            .iter()
            .find_map(|leg| broker_core::parse_osi_symbol(&leg.symbol).ok())
            .map(|parsed| parsed.underlying)
            .unwrap_or_default();
        let body = json!({
            "baseSymbol": base_symbol,
            "optionLegs": legs.iter().filter(|leg| leg.asset == AssetKind::Option).map(|leg| {
                let (side, open_close) = match leg.instruction {
                    LegInstruction::Buy | LegInstruction::BuyToOpen => ("BUY", "OPEN"),
                    LegInstruction::BuyToClose => ("BUY", "CLOSE"),
                    LegInstruction::Sell | LegInstruction::SellToClose => ("SELL", "CLOSE"),
                    LegInstruction::SellToOpen => ("SELL", "OPEN"),
                };
                json!({
                    "symbol": leg.symbol.trim(),
                    "side": side,
                    "openCloseIndicator": open_close,
                    "ratioQuantity": leg.ratio.round() as i64
                })
            }).collect::<Vec<_>>()
        });
        let raw = self
            .client
            .post_json(
                &format!("/userapigateway/option-details/{id}/strategy-details/quote"),
                &body,
            )
            .await?;
        Ok(Quote {
            symbol: raw
                .get("strategyName")
                .and_then(|value| value.as_str())
                .unwrap_or("strategy")
                .to_string(),
            bid: crate::parse::num(raw.get("bid")),
            ask: crate::parse::num(raw.get("ask")),
            last: crate::parse::num(raw.get("mark")).or_else(|| crate::parse::num(raw.get("price"))),
            bid_size: None,
            ask_size: None,
            volume: None,
        })
    }

    async fn bars(&self, _account: &AccountRef, symbol: &str, period: &str) -> Result<Vec<Bar>> {
        let period_owned = match period.to_ascii_lowercase().as_str() {
            "day" | "1d" => "DAY".to_string(),
            "week" => "WEEK".to_string(),
            "month" => "MONTH".to_string(),
            "year" | "1y" => "YEAR".to_string(),
            other => other.to_ascii_uppercase(),
        };
        let raw = self
            .client
            .get_json(&format!(
                "/userapigateway/historicdata/EQUITY/{symbol}/{period_owned}"
            ))
            .await?;
        Ok(bars_from(&raw))
    }

    async fn session_open(&self, _account: &AccountRef) -> Result<SessionStatus> {
        Ok(local_session_status())
    }
}
