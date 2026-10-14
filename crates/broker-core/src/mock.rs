use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::account::{AccountKind, AccountRef, BrokerKind};
use crate::capabilities::Capabilities;
use crate::intent::{EquitySize, OrderIntent, SpreadLeg};
use crate::market::MarketData;
use crate::model::{
    Balances, Bar, Greeks, OptionChain, Order, OrderStatus, PlaceResult, Portfolio, Position,
    Preflight, Quote, SessionStatus,
};
use crate::traits::Broker;

/// In-memory broker for engine tests. Places fill immediately and never touch a network.
#[derive(Default)]
pub struct MockBroker {
    orders: Mutex<Vec<Order>>,
    positions: Mutex<Vec<Position>>,
    cash: Mutex<f64>,
}

impl MockBroker {
    pub fn with_cash(cash: f64) -> Self {
        Self {
            cash: Mutex::new(cash),
            ..Self::default()
        }
    }

    pub fn orders(&self) -> Vec<Order> {
        self.orders.lock().expect("orders").clone()
    }
}

#[async_trait]
impl Broker for MockBroker {
    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::public();
        caps.broker = "mock".into();
        caps.paper = true;
        caps
    }

    async fn accounts(&self) -> anyhow::Result<Vec<AccountRef>> {
        Ok(vec![AccountRef::new(BrokerKind::Public, "mock")])
    }

    async fn portfolio(&self, account: &AccountRef) -> anyhow::Result<Portfolio> {
        let cash = *self.cash.lock().expect("cash");
        Ok(Portfolio {
            account: account.clone(),
            kind: AccountKind::Cash,
            balances: Balances {
                cash_buying_power: cash,
                options_buying_power: Some(cash),
                equity: cash,
            },
            positions: self.positions.lock().expect("positions").clone(),
            strategies: vec![],
        })
    }

    async fn open_orders(&self, _account: &AccountRef) -> anyhow::Result<Vec<Order>> {
        Ok(self
            .orders
            .lock()
            .expect("orders")
            .iter()
            .filter(|order| !order.status.is_terminal())
            .cloned()
            .collect())
    }

    async fn order_history(
        &self,
        _account: &AccountRef,
        _since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<Order>> {
        Ok(self.orders.lock().expect("orders").clone())
    }

    async fn preflight(
        &self,
        account: &AccountRef,
        intent: &OrderIntent,
    ) -> anyhow::Result<Preflight> {
        let balances = self.balances(account).await?;
        let cost = intent.estimated_notional();
        let accepted = cost <= balances.cash_buying_power || cost == 0.0;
        Ok(Preflight {
            accepted,
            estimated_cost: Some(cost),
            estimated_proceeds: None,
            buying_power_requirement: Some(cost),
            fees: Some(0.0),
            message: if accepted {
                "mock preflight accepted".into()
            } else {
                "insufficient cash".into()
            },
        })
    }

    async fn place(
        &self,
        _account: &AccountRef,
        intent: &OrderIntent,
        client_id: &str,
    ) -> anyhow::Result<PlaceResult> {
        let qty = match intent {
            OrderIntent::Equity {
                size: EquitySize::Shares(qty),
                ..
            } => *qty,
            _ => 1.0,
        };
        self.orders.lock().expect("orders").push(Order {
            id: client_id.to_string(),
            status: OrderStatus::Filled,
            filled_quantity: qty,
            average_price: None,
            raw_status: "FILLED".into(),
        });
        Ok(PlaceResult {
            order_id: client_id.to_string(),
            status: OrderStatus::Filled,
            message: "mock fill".into(),
        })
    }

    async fn get_order(&self, _account: &AccountRef, order_id: &str) -> anyhow::Result<Order> {
        self.orders
            .lock()
            .expect("orders")
            .iter()
            .find(|order| order.id == order_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown order {order_id}"))
    }

    async fn cancel(&self, _account: &AccountRef, order_id: &str) -> anyhow::Result<()> {
        if let Some(order) = self
            .orders
            .lock()
            .expect("orders")
            .iter_mut()
            .find(|order| order.id == order_id)
        {
            order.status = OrderStatus::Canceled;
            order.raw_status = "CANCELED".into();
        }
        Ok(())
    }

    async fn replace(
        &self,
        account: &AccountRef,
        order_id: &str,
        intent: &OrderIntent,
    ) -> anyhow::Result<PlaceResult> {
        self.cancel(account, order_id).await?;
        self.place(account, intent, order_id).await
    }
}

#[async_trait]
impl MarketData for MockBroker {
    fn broker_name(&self) -> &'static str {
        "mock"
    }

    async fn quotes(
        &self,
        _account: &AccountRef,
        symbols: &[String],
    ) -> anyhow::Result<Vec<Quote>> {
        Ok(symbols
            .iter()
            .map(|symbol| Quote {
                symbol: symbol.clone(),
                bid: Some(100.0),
                ask: Some(100.1),
                last: Some(100.05),
                bid_size: Some(10.0),
                ask_size: Some(10.0),
                volume: Some(1_000.0),
            })
            .collect())
    }

    async fn option_expirations(
        &self,
        _account: &AccountRef,
        _underlying: &str,
    ) -> anyhow::Result<Vec<String>> {
        Ok(vec!["2026-11-20".into()])
    }

    async fn option_chain(
        &self,
        _account: &AccountRef,
        underlying: &str,
        expiration: &str,
    ) -> anyhow::Result<OptionChain> {
        Ok(OptionChain {
            underlying: underlying.into(),
            expirations: vec![expiration.into()],
            contracts: vec![],
        })
    }

    async fn greeks(&self, _account: &AccountRef, _osi_symbol: &str) -> anyhow::Result<Greeks> {
        Ok(Greeks {
            delta: Some(-0.15),
            gamma: Some(0.01),
            theta: Some(-0.02),
            vega: Some(0.1),
            iv: Some(0.2),
        })
    }

    async fn strategy_quote(
        &self,
        _account: &AccountRef,
        _legs: &[SpreadLeg],
    ) -> anyhow::Result<Quote> {
        Ok(Quote {
            symbol: "strategy".into(),
            bid: Some(0.40),
            ask: Some(0.45),
            last: Some(0.42),
            bid_size: None,
            ask_size: None,
            volume: None,
        })
    }

    async fn bars(
        &self,
        _account: &AccountRef,
        _symbol: &str,
        _period: &str,
    ) -> anyhow::Result<Vec<Bar>> {
        Ok(vec![])
    }

    async fn session_open(&self, _account: &AccountRef) -> anyhow::Result<SessionStatus> {
        Ok(SessionStatus {
            open: true,
            source: "mock".into(),
        })
    }
}
