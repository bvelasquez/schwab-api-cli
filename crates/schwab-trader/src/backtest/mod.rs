pub mod cache;
pub mod cron;
pub mod exits;
pub mod prefetch;
pub mod report;
pub mod runner;

pub use cache::{BacktestCache, StoredCandle};
pub use prefetch::prefetch_daily_bars;
pub use report::{build_backtest_analysis_report, compute_benchmark_roi};
pub use runner::{run_backtest, BacktestRunOptions, EntryFillMode};
