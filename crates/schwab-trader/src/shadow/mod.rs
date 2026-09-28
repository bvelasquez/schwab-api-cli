//! Shadow arms: paper-only alternative rule sets evaluated on the production
//! agent's regular-session ticks (same quotes, bars, and timestamps), each with
//! its own ledger and journal. See docs/TRADER_RULES.md § Shadow arms.

pub mod arm;
pub mod report;
pub mod stats;
mod tick;

pub use arm::{arm_watch_paths, build_arm_rules, merge_overrides, ShadowArms};
