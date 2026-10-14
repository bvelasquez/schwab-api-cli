use async_trait::async_trait;

use crate::account::AccountRef;
use crate::intent::SpreadLeg;
use crate::model::{session_open_local, Bar, Greeks, OptionChain, Quote, SessionStatus};

#[async_trait]
pub trait MarketData: Send + Sync {
    fn broker_name(&self) -> &'static str;

    async fn quotes(&self, account: &AccountRef, symbols: &[String]) -> anyhow::Result<Vec<Quote>>;

    async fn option_expirations(
        &self,
        account: &AccountRef,
        underlying: &str,
    ) -> anyhow::Result<Vec<String>>;

    async fn option_chain(
        &self,
        account: &AccountRef,
        underlying: &str,
        expiration: &str,
    ) -> anyhow::Result<OptionChain>;

    async fn greeks(&self, account: &AccountRef, osi_symbol: &str) -> anyhow::Result<Greeks>;

    async fn strategy_quote(
        &self,
        account: &AccountRef,
        legs: &[SpreadLeg],
    ) -> anyhow::Result<Quote>;

    async fn bars(
        &self,
        account: &AccountRef,
        symbol: &str,
        period: &str,
    ) -> anyhow::Result<Vec<Bar>>;

    async fn session_open(&self, account: &AccountRef) -> anyhow::Result<SessionStatus>;
}

/// Used when a backend has no market-hours endpoint.
pub fn local_session_status() -> SessionStatus {
    session_open_local(chrono::Utc::now())
}
