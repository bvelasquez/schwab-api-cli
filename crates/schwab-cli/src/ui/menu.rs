use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use inquire::Select;

use crate::agent::{log_path, spawn_background, stop_daemon};
use crate::config::RuntimeConfig;
use crate::output::ResponseEnvelope;
use crate::rules::RulesConfig;
use crate::ui::context::{tail_lines, DashboardContext};
use crate::ui::dashboard::render_dashboard;
use crate::ui::discover::{list_rules_files_display, resolve_rules_file};
use crate::ui::rules_view::render_rules_detail;

pub async fn run_interactive_menu(runtime: &RuntimeConfig) -> Result<()> {
    loop {
        let choice = Select::new("schwab — what next?", menu_choices())
            .with_help_message("Human mode interactive menu")
            .prompt();

        let choice = match choice {
            Ok(c) => c,
            Err(_) => break,
        };

        match choice.as_str() {
            "Dashboard" => show_dashboard(runtime, None).await?,
            "Watch agent (live)" => {
                crate::commands::ui_cmd::run_watch(runtime, None, false).await?;
            }
            "Show rules" => show_rules(runtime, None).await?,
            "Tail agent log" => tail_agent_log(None)?,
            "Validate rules" => validate_rules(runtime, None).await?,
            "Run agent (background)" => run_agent_background(runtime, None).await?,
            "Stop agent" => stop_agent(runtime, None).await?,
            "List rules files" => {
                println!("\n{}", list_rules_files_display());
                println!();
            }
            "Help" => print_menu_help(),
            "Quit" => break,
            _ => {}
        }
    }
    Ok(())
}

fn menu_choices() -> Vec<String> {
    vec![
        "Dashboard".into(),
        "Watch agent (live)".into(),
        "Show rules".into(),
        "Tail agent log".into(),
        "Validate rules".into(),
        "Run agent (background)".into(),
        "Stop agent".into(),
        "List rules files".into(),
        "Help".into(),
        "Quit".into(),
    ]
}

pub async fn show_dashboard(
    runtime: &RuntimeConfig,
    file: Option<PathBuf>,
) -> Result<()> {
    let rules_path = resolve_rules_file(file, runtime.is_interactive())?;
    let ctx = DashboardContext::load(&rules_path)?;

    if runtime.output == crate::output::OutputFormat::Json {
        runtime.emit(ResponseEnvelope::ok(
            "dashboard",
            ctx.to_json(),
        ));
        return Ok(());
    }

    print!("{}", render_dashboard(&ctx));
    io::stdout().flush().ok();
    Ok(())
}

pub async fn show_rules(runtime: &RuntimeConfig, file: Option<PathBuf>) -> Result<()> {
    let rules_path = resolve_rules_file(file, runtime.is_interactive())?;
    let ctx = DashboardContext::load(&rules_path)?;

    if runtime.output == crate::output::OutputFormat::Json {
        runtime.emit(ResponseEnvelope::ok(
            "rules show",
            ctx.to_json(),
        ));
        return Ok(());
    }

    print!("{}", render_rules_detail(&ctx));
    io::stdout().flush().ok();
    Ok(())
}

pub fn tail_agent_log(file: Option<PathBuf>) -> Result<()> {
    let rules_path = resolve_rules_file(file, true)?;
    let log = log_path(&rules_path);
    let lines = tail_lines(&log, 40);
    if lines.is_empty() {
        println!("\n  (no log at {})\n", log.display());
        return Ok(());
    }
    println!("\n  Agent log — {}\n", log.display());
    for line in lines {
        println!("  {line}");
    }
    println!();
    Ok(())
}

async fn validate_rules(runtime: &RuntimeConfig, file: Option<PathBuf>) -> Result<()> {
    let rules_path = resolve_rules_file(file, true)?;
    let rules = RulesConfig::load(&rules_path)?;
    runtime.emit(ResponseEnvelope::ok(
        "agent validate",
        serde_json::json!({
            "valid": true,
            "agent_id": rules.agent_id,
            "rules_path": rules_path,
            "accounts": rules.accounts.len(),
            "watchlist": rules.watchlist,
            "llm_enabled": rules.llm.enabled,
            "telegram_enabled": rules.notify.telegram.enabled,
        }),
    ));
    Ok(())
}

async fn run_agent_background(
    runtime: &RuntimeConfig,
    file: Option<PathBuf>,
) -> Result<()> {
    let rules_path = resolve_rules_file(file, true)?;
    if !runtime.trust || !runtime.yes {
        anyhow::bail!(
            "background agent requires --trust --yes (live trading guardrails still apply)"
        );
    }
    let mut extra = Vec::new();
    if runtime.dry_run {
        extra.push("--dry-run".into());
    }
    extra.push("--trust".into());
    extra.push("--yes".into());
    let pid = spawn_background(&rules_path, &extra)?;
    runtime.emit(ResponseEnvelope::ok(
        "agent background",
        serde_json::json!({
            "pid": pid,
            "rules": rules_path,
            "log_file": log_path(&rules_path),
        }),
    ));
    Ok(())
}

async fn stop_agent(runtime: &RuntimeConfig, file: Option<PathBuf>) -> Result<()> {
    let rules_path = resolve_rules_file(file, true)?;
    stop_daemon(&rules_path)?;
    runtime.emit(ResponseEnvelope::ok(
        "agent stop",
        serde_json::json!({ "stopped": true, "rules": rules_path }),
    ));
    Ok(())
}

fn print_menu_help() {
    println!(
        r#"
  schwab dashboard [rules.yaml]     Rich status dashboard
  schwab watch [rules.yaml]         Live TUI + agent (q stops both)
  schwab watch --monitor-only       Attach to running daemon only
  schwab rules show [rules.yaml]    Full rules breakdown
  schwab rules list               Discover rules/*.yaml
  schwab agent run --background   Start daemon
  schwab agent stop               Stop daemon

  Set SCHWAB_RULES=path/to/rules.yaml to skip file prompts.
  Agent mode: add --json for machine-readable output.
"#
    );
}

pub fn list_rules_files(runtime: &RuntimeConfig) {
    use crate::ui::discover::discover_rules_files;
    let files = discover_rules_files();
    runtime.emit(ResponseEnvelope::ok(
        "rules list",
        serde_json::json!({
            "files": files,
            "env": "SCHWAB_RULES",
        }),
    ));
}
