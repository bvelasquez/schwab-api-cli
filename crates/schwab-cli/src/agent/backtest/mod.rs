//! Options historical backtest (synthetic BS marks over Schwab underlying bars).

pub mod bs;
pub mod cache;
pub mod prefetch;
pub mod report;
pub mod runner;
pub mod synth;

pub use prefetch::prefetch_daily_bars;
pub use report::build_backtest_report;
pub use runner::{run_backtest, BacktestRunOptions};
