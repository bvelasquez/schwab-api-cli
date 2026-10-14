//! Broker-neutral domain types. Schwab and Public adapters live in other crates.

mod account;
mod capabilities;
mod intent;
mod market;
mod mock;
mod model;
mod osi;
mod registry;
mod traits;

pub use account::{parse_account_address, AccountKind, AccountRef, BrokerKind};
pub use capabilities::Capabilities;
pub use intent::{
    AssetKind, EquitySize, ExitBracket, LegInstruction, NetPrice, OrderIntent, Session, Side,
    SpreadKind, SpreadLeg, TimeInForce,
};
pub use market::{local_session_status, MarketData};
pub use mock::MockBroker;
pub use model::{
    Balances, Bar, Greeks, OptionChain, OptionContract, OptionStrategyGroup, Order, OrderStatus,
    PlaceResult, Portfolio, Position, Preflight, Quote, SessionStatus, StrategyLeg, WaitOptions,
};
pub use osi::{build_osi_symbol, parse_osi_symbol, ParsedOsi};
pub use registry::{BrokerHandle, BrokerRegistry, DataSource};
pub use traits::Broker;

pub async fn poll_order(
    broker: &dyn Broker,
    account: &AccountRef,
    order_id: &str,
    opts: WaitOptions,
) -> anyhow::Result<Order> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(opts.timeout_seconds);
    loop {
        let order = broker.get_order(account, order_id).await?;
        if order.status.is_terminal() || matches!(opts.until, WaitUntil::Accepted) {
            return Ok(order);
        }
        if matches!(opts.until, WaitUntil::Filled) && order.status == OrderStatus::Partial {
            if opts.proceed_on_partial {
                return Ok(order);
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for order {order_id} ({:?})", order.status);
        }
        tokio::time::sleep(std::time::Duration::from_secs(opts.poll_interval_seconds.max(1))).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    Accepted,
    Filled,
    Terminal,
}
