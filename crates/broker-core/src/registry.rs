use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use crate::account::{AccountRef, BrokerKind};
use crate::market::MarketData;
use crate::traits::Broker;

#[derive(Clone)]
pub struct BrokerHandle {
    pub trading: Arc<dyn Broker>,
    pub data: Arc<dyn MarketData>,
    pub data_source: BrokerKind,
}

/// Lazy map from broker kind to trading and market-data adapters.
#[derive(Default, Clone)]
pub struct BrokerRegistry {
    trading: HashMap<BrokerKind, Arc<dyn Broker>>,
    data: HashMap<BrokerKind, Arc<dyn MarketData>>,
}

impl BrokerRegistry {
    pub fn insert(&mut self, kind: BrokerKind, trading: Arc<dyn Broker>, data: Arc<dyn MarketData>) {
        self.trading.insert(kind, trading);
        self.data.insert(kind, data);
    }

    pub fn resolve(&self, account: &AccountRef, data_source: DataSource) -> Result<BrokerHandle> {
        let trading = self
            .trading
            .get(&account.broker)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no trading backend registered for {}", account.broker))?;
        let source = match data_source {
            DataSource::AccountBroker => account.broker,
            DataSource::Explicit(kind) => kind,
        };
        let data = self.data.get(&source).cloned().ok_or_else(|| {
            anyhow::anyhow!("no market-data backend registered for {source}")
        })?;
        if !matches!(data_source, DataSource::AccountBroker) && source != account.broker {
            tracing_fallback(account, source);
        }
        Ok(BrokerHandle {
            trading,
            data,
            data_source: source,
        })
    }

    pub fn get(&self, kind: BrokerKind) -> Result<BrokerHandle> {
        let account = AccountRef::new(kind, "");
        self.resolve(&account, DataSource::AccountBroker)
    }
}

fn tracing_fallback(account: &AccountRef, source: BrokerKind) {
    eprintln!(
        "data_source {source} overrides account broker {} for {}",
        account.broker,
        account.address()
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSource {
    AccountBroker,
    Explicit(BrokerKind),
}

impl DataSource {
    pub fn resolve(explicit: Option<BrokerKind>, _account: BrokerKind) -> Result<Self> {
        match explicit {
            Some(kind) => Ok(Self::Explicit(kind)),
            None => Ok(Self::AccountBroker),
        }
    }

    pub fn parse(raw: Option<&str>, account: BrokerKind) -> Result<Self> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(Self::AccountBroker),
            Some(value) => {
                let kind = BrokerKind::parse(value)?;
                if kind == account {
                    Ok(Self::AccountBroker)
                } else {
                    Ok(Self::Explicit(kind))
                }
            }
        }
    }
}
