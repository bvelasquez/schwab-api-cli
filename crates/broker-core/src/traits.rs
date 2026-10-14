use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::account::AccountRef;
use crate::capabilities::Capabilities;
use crate::intent::OrderIntent;
use crate::model::{Balances, Order, PlaceResult, Portfolio, Preflight};

#[async_trait]
pub trait Broker: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    async fn accounts(&self) -> anyhow::Result<Vec<AccountRef>>;

    async fn portfolio(&self, account: &AccountRef) -> anyhow::Result<Portfolio>;

    async fn balances(&self, account: &AccountRef) -> anyhow::Result<Balances> {
        Ok(self.portfolio(account).await?.balances)
    }

    async fn open_orders(&self, account: &AccountRef) -> anyhow::Result<Vec<Order>>;

    async fn order_history(
        &self,
        account: &AccountRef,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<Order>>;

    async fn preflight(
        &self,
        account: &AccountRef,
        intent: &OrderIntent,
    ) -> anyhow::Result<Preflight>;

    async fn place(
        &self,
        account: &AccountRef,
        intent: &OrderIntent,
        client_id: &str,
    ) -> anyhow::Result<PlaceResult>;

    async fn get_order(&self, account: &AccountRef, order_id: &str) -> anyhow::Result<Order>;

    async fn cancel(&self, account: &AccountRef, order_id: &str) -> anyhow::Result<()>;

    async fn replace(
        &self,
        account: &AccountRef,
        order_id: &str,
        intent: &OrderIntent,
    ) -> anyhow::Result<PlaceResult>;
}
