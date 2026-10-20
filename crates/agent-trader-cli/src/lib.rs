pub mod dashboard;
mod guard;
pub mod options;
pub mod paper;
mod paths;
mod plan;
mod registry;
mod safety;

pub use guard::{legacy_account_busy, AccountLock};
pub use options::{
    exit_action, select_put_credit, ExitAction, ExitRules, SpreadChoice, VerticalRules,
};
pub use paths::{config_dir, load_dotenv, safety_path, state_dir};
pub use plan::{run_plan, TradePlan};
pub use registry::build_registry;
pub use safety::{require_trading_approval, SafetyGate, TradingFlags};
