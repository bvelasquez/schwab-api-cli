pub mod daemon;
pub mod exits;
pub mod format;
pub mod llm;
pub mod learn;
pub mod market_context;
pub mod regime;
pub mod spread_analytics;
pub mod journal;
pub mod paths;
pub mod protective;
pub mod resilience;
pub mod risk;
pub mod roll;
pub mod scorecard;
pub mod backtest;
pub mod chains_util;
pub mod technical;
pub mod sim;
pub mod volatility;
pub mod runner;
pub mod schedule;
pub mod state;
pub mod telegram_format;

pub use daemon::{daemon_status, spawn_background, stop_daemon, DaemonStatus};
pub use paths::{
    active_state_path, load_agent_state, load_sim_agent_state, log_path, pid_path,
    sim_journal_path, sim_state_path,
};
pub use runner::run_agent_loop;
pub use sim::{analysis_report, compute_stats, reset_sim};
pub use state::{load_state, save_state, state_summary};
