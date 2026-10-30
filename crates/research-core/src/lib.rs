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
//! - [`playbook`] — the mutable artifact and its versioned overlay store (plan §5).
//! - [`book`] — the paper fill model, positions, marks and P&L (plan §7).
//! - [`scoring`] — forward-return scoring and rank IC (plan §8 stage B).
//! - [`indicators`] — RSI/ATR over daily bars, point-in-time filtered.

pub mod book;
pub mod config;
pub mod forecast;
pub mod gate;
pub mod indicators;
pub mod paper_broker;
pub mod patch;
pub mod playbook;
pub mod scoring;
pub mod web_search;

pub use book::{PaperBook, PaperPosition, TickInputs, TickOutcome};
pub use config::ArmConfig;
pub use forecast::{Forecast, Signal};
pub use gate::{run_self_test, GateInputs, GateReport};
pub use paper_broker::PaperOnlyBroker;
pub use patch::{apply_patches, AppliedPatch, Patch};
pub use playbook::Playbook;
pub use scoring::{ForecastScore, PendingForecast, Scorecard};
