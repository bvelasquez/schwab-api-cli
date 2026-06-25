use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use schwab_api::{ClientConfig, SchwabClient};
use schwab_market_data::MarketDataApi;

use crate::market_hours::option_market_open_from_hours;

static CACHE: Mutex<Option<(Instant, Option<bool>)>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(120);

/// Cached Schwab option market open flag (`None` if credentials unavailable).
pub fn fetch_option_market_open_cached() -> Option<bool> {
    let mut guard = CACHE.lock().ok()?;
    if let Some((fetched_at, value)) = guard.as_ref() {
        if fetched_at.elapsed() < CACHE_TTL {
            return *value;
        }
    }
    let value = fetch_option_market_open_blocking();
    *guard = Some((Instant::now(), value));
    value
}

fn fetch_option_market_open_blocking() -> Option<bool> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let config = ClientConfig::from_env().ok()?;
        let market = MarketDataApi::new(SchwabClient::new(config));
        let hours = market.markets().hours("option", None).await.ok()?;
        option_market_open_from_hours(&hours, Utc::now())
    })
}

pub fn market_label(open: Option<bool>, session: Option<&str>) -> (&'static str, bool) {
    match open {
        Some(true) => ("OPEN (regular)", true),
        Some(false) => {
            if matches!(session, Some("overnight")) {
                ("CLOSED (overnight)", false)
            } else {
                ("CLOSED", false)
            }
        }
        None => ("unknown", false),
    }
}
