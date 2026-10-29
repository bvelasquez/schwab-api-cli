//! Paper-only research arm primitives.
//!
//! This crate is deliberately broker-agnostic and *never* performs a trade.
//! It provides the safety spine for the `agent-trader research` arm:
//!
//! - [`config`] — the arm rules file (`rules/research/<arm>.yaml`).
//! - [`gate`] — fail-closed startup self-test (plan §4, L4).
//! - [`paper_broker`] — a `Broker` wrapper that refuses every write (plan §4, L2/L5).
//! - [`forecast`] — the forecast contract (plan §0.1).
//! - [`patch`] — bounded plan patches with an allowlist and safety rails (plan §5).
//! - [`web_search`] — OpenRouter `openrouter:web_search` client (plan §0.2, §6).

pub mod config;
pub mod forecast;
pub mod gate;
pub mod paper_broker;
pub mod patch;
pub mod web_search;

pub use config::ArmConfig;
pub use forecast::{Forecast, Signal};
pub use gate::{run_self_test, GateInputs, GateReport};
pub use paper_broker::PaperOnlyBroker;
pub use patch::{apply_patches, AppliedPatch, Patch};
