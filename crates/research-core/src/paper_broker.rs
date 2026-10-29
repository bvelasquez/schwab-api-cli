//! A `Broker` wrapper that refuses every write (plan §4, L2 and L5).
//!
//! Reads delegate to the inner (Public, read-only) broker. `place`, `cancel`,
//! and `replace` increment a tripwire counter, write `unsafe_attempt.json` if a
//! path is configured, and return an error. The process is expected to exit
//! non-zero when the counter is non-zero at shutdown.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use broker_core::{
    AccountRef, Balances, Broker, Capabilities, Order, OrderIntent, PlaceResult, Portfolio,
    Preflight,
};
use chrono::{DateTime, Utc};
use serde_json::json;

pub struct PaperOnlyBroker {
    inner: Arc<dyn Broker>,
    attempts: AtomicU64,
    tripwire_path: Option<PathBuf>,
}

impl PaperOnlyBroker {
    pub fn new(inner: Arc<dyn Broker>, tripwire_path: Option<PathBuf>) -> Self {
        Self {
            inner,
            attempts: AtomicU64::new(0),
            tripwire_path,
        }
    }

    /// Number of refused write attempts. Must be `0` for a clean run.
    pub fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::SeqCst)
    }

    fn tripwire(&self, op: &str) -> Result<()> {
        let count = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(path) = &self.tripwire_path {
            let record = json!({
                "op": op,
                "attempt": count,
                "at": Utc::now().to_rfc3339(),
                "note": "paper-only broker refused a write; the process must exit non-zero",
            });
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, record.to_string());
        }
        bail!("paper-only: {op} is disabled (research-paper hard gate)")
    }
}

#[async_trait]
impl Broker for PaperOnlyBroker {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn accounts(&self) -> Result<Vec<AccountRef>> {
        self.inner.accounts().await
    }

    async fn portfolio(&self, account: &AccountRef) -> Result<Portfolio> {
        self.inner.portfolio(account).await
    }

    async fn balances(&self, account: &AccountRef) -> Result<Balances> {
        self.inner.balances(account).await
    }

    async fn open_orders(&self, account: &AccountRef) -> Result<Vec<Order>> {
        self.inner.open_orders(account).await
    }

    async fn order_history(&self, account: &AccountRef, since: DateTime<Utc>) -> Result<Vec<Order>> {
        self.inner.order_history(account, since).await
    }

    async fn preflight(&self, account: &AccountRef, intent: &OrderIntent) -> Result<Preflight> {
        self.inner.preflight(account, intent).await
    }

    async fn get_order(&self, account: &AccountRef, order_id: &str) -> Result<Order> {
        self.inner.get_order(account, order_id).await
    }

    async fn place(
        &self,
        _account: &AccountRef,
        _intent: &OrderIntent,
        _client_id: &str,
    ) -> Result<PlaceResult> {
        self.tripwire("place")?;
        unreachable!("tripwire always returns Err")
    }

    async fn cancel(&self, _account: &AccountRef, _order_id: &str) -> Result<()> {
        self.tripwire("cancel")?;
        unreachable!("tripwire always returns Err")
    }

    async fn replace(
        &self,
        _account: &AccountRef,
        _order_id: &str,
        _intent: &OrderIntent,
    ) -> Result<PlaceResult> {
        self.tripwire("replace")?;
        unreachable!("tripwire always returns Err")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use broker_core::{EquitySize, Side, TimeInForce};

    #[tokio::test]
    async fn place_is_refused_and_trips() {
        let inner: Arc<dyn Broker> = Arc::new(broker_core::MockBroker::default());
        let dir = tempfile::tempdir().unwrap();
        let tripwire = dir.path().join("unsafe_attempt.json");
        let broker = PaperOnlyBroker::new(inner, Some(tripwire.clone()));
        let account = AccountRef::new(broker_core::BrokerKind::Public, "ACC");
        let intent = OrderIntent::Equity {
            side: Side::Buy,
            symbol: "MSFT".into(),
            size: EquitySize::Shares(1.0),
            limit: Some(100.0),
            tif: TimeInForce::Day,
            session: broker_core::Session::Core,
            bracket: None,
        };
        let err = broker.place(&account, &intent, "c1").await.unwrap_err();
        assert!(err.to_string().contains("paper-only"));
        assert_eq!(broker.attempts(), 1);
        assert!(tripwire.is_file());
        let body = std::fs::read_to_string(&tripwire).unwrap();
        assert!(body.contains("\"op\":\"place\""));
    }

    #[tokio::test]
    async fn reads_delegate() {
        let inner: Arc<dyn Broker> = Arc::new(broker_core::MockBroker::default());
        let broker = PaperOnlyBroker::new(inner, None);
        let account = AccountRef::new(broker_core::BrokerKind::Public, "ACC");
        // MockBroker portfolio is a read; it must not trip.
        let _ = broker.portfolio(&account).await;
        assert_eq!(broker.attempts(), 0);
    }
}
