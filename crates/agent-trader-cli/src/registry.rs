use anyhow::Result;
use broker_core::{BrokerKind, BrokerRegistry};
use broker_public::PublicBackend;
use broker_schwab::SchwabBackend;

pub fn build_registry() -> Result<BrokerRegistry> {
    let mut registry = BrokerRegistry::default();
    if std::env::var("SCHWAB_APP_KEY").is_ok() {
        match SchwabBackend::from_env() {
            Ok(backend) => {
                let (trading, data) = backend.into_arcs();
                registry.insert(BrokerKind::Schwab, trading, data);
            }
            Err(err) => eprintln!("schwab backend not loaded: {err}"),
        }
    }
    if std::env::var("PUBLIC_API_KEY").is_ok() || std::env::var("PUBLIC_PERSONAL_SECRET").is_ok() {
        match PublicBackend::from_env() {
            Ok(backend) => {
                let (trading, data) = backend.into_arcs();
                registry.insert(BrokerKind::Public, trading, data);
            }
            Err(err) => eprintln!("public backend not loaded: {err}"),
        }
    }
    Ok(registry)
}
