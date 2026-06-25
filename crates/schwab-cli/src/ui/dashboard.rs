use chrono::{DateTime, Utc};
use console::Style;

use super::{
    ago_secs, bar, clock_dot, format_duration_secs, panel, panel_fit, rule, status_dot,
    terminal_width, two_column,
};
use super::context::DashboardContext;

pub fn render_dashboard(ctx: &DashboardContext) -> String {
    let width = terminal_width().min(120);
    let use_side_by_side = width >= 100;
    let col_inner = if use_side_by_side {
        (width / 2).saturating_sub(4)
    } else {
        width.saturating_sub(4)
    };
    let full_max = width;

    let now = chrono::Local::now().format("%a %d %b %Y  %H:%M");
    let mut out = String::new();
    out.push('\n');
    out.push_str(&rule(&format!("✦ Schwab Agent  ·  {now}")));
    out.push_str("\n\n");

    let agent_panel = render_agent_panel(ctx, col_inner);
    let rules_panel = render_rules_panel(ctx, col_inner);
    if use_side_by_side {
        out.push_str(&two_column(agent_panel, rules_panel, width));
    } else {
        out.push_str(&agent_panel);
        out.push('\n');
        out.push_str(&rules_panel);
    }
    out.push_str("\n\n");

    let positions = render_positions_panel(ctx, full_max);
    if !positions.is_empty() {
        out.push_str(&positions);
        out.push_str("\n\n");
    }

    let activity = render_activity_panel(ctx, full_max);
    if !activity.is_empty() {
        out.push_str(&activity);
        out.push_str("\n\n");
    }

    let log_panel = render_log_panel(ctx, full_max);
    if !log_panel.is_empty() {
        out.push_str(&log_panel);
        out.push_str("\n\n");
    }

    if !ctx.daemon.running {
        let rules_display = ctx
            .rules_path
            .file_name()
            .map(|n| format!("rules/{}", n.to_string_lossy()))
            .unwrap_or_else(|| ctx.rules.agent_id.clone());
        out.push_str(&format!(
            "  {} Agent daemon is not running — {} {}\n\n",
            Style::new().yellow().apply_to("!"),
            Style::new().dim().apply_to("start with"),
            Style::new().cyan().apply_to(format!(
                "schwab agent run {rules_display} --background --trust --yes"
            ))
        ));
    }

    out.push_str(&render_footer_hint());
    out.push('\n');
    out
}

fn render_agent_panel(ctx: &DashboardContext, inner: usize) -> String {
    let dim = Style::new().dim();
    let green = Style::new().green();
    let mut lines = Vec::new();

    if ctx.daemon.running {
        let pid = ctx
            .daemon
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".into());
        lines.push(format!(
            "  {}  {:<12} {}  pid {}",
            status_dot(true),
            "daemon",
            green.apply_to("running"),
            dim.apply_to(pid)
        ));
    } else {
        lines.push(format!(
            "  {}  {:<12} {}",
            status_dot(false),
            "daemon",
            Style::new().red().apply_to("stopped")
        ));
    }

    if let Some(at) = ctx.state.last_tick {
        let secs = (Utc::now() - at).num_seconds();
        let session = ctx
            .state
            .last_session
            .as_deref()
            .unwrap_or("?");
        lines.push(format!(
            "  {}  {:<12} last tick {}  {}",
            clock_dot(),
            "agent",
            dim.apply_to(ago_secs(secs)),
            session_style(session).apply_to(session)
        ));
    } else {
        lines.push(format!(
            "  {}  {:<12} {}",
            clock_dot(),
            "agent",
            dim.apply_to("no ticks yet")
        ));
    }

    let open = ctx.state.open_positions.len();
    let pending = ctx.state.pending_order_ids.len();
    let trades = ctx.state.trades_today;
    let max_trades = ctx.rules.risk.max_trades_per_day;
    lines.push(format!(
        "           {} open  ·  {} pending  ·  {}/{} trades today",
        open, pending, trades, max_trades
    ));

    if ctx.rules.llm.enabled {
        let phase = ctx
            .state
            .last_llm_summary
            .as_ref()
            .and_then(|v| v.get("phase"))
            .and_then(|v| v.as_str())
            .unwrap_or("—");
        lines.push(format!(
            "           LLM {}  ·  {} reviews",
            dim.apply_to(phase),
            ctx.state.llm_review_count
        ));
    }

    panel("Agent", &lines, inner)
}

fn render_rules_panel(ctx: &DashboardContext, inner: usize) -> String {
    let dim = Style::new().dim();
    let rules = &ctx.rules;
    let mut lines = Vec::new();

    lines.push(format!(
        "  {}",
        Style::new().cyan().bold().apply_to(&rules.agent_id)
    ));
    lines.push(format!(
        "  tick {}  ·  monitor every {}m",
        format_duration_secs(rules.schedule.tick_interval_seconds),
        ctx.monitor_interval_minutes()
    ));

    let watch = rules.watchlist.join(", ");
    lines.push(format!("  watchlist  {}", dim.apply_to(&watch)));

    let mut strategies = Vec::new();
    if rules.strategies.vertical.enabled {
        let v = &rules.entry_rules.vertical;
        strategies.push(format!(
            "vertical {}  {}–{} DTE",
            v.r#type, v.dte_min, v.dte_max
        ));
    }
    if rules.strategies.iron_condor.enabled {
        let ic = &rules.entry_rules.iron_condor;
        strategies.push(format!(
            "iron condor  {}–{} DTE",
            ic.dte_min, ic.dte_max
        ));
    }
    if strategies.is_empty() {
        lines.push(format!("  strategies {}", dim.apply_to("(none enabled)")));
    } else {
        for s in strategies {
            lines.push(format!("  {s}"));
        }
    }

    let risk_used = ctx.portfolio_risk_usd();
    let risk_max = rules.risk.max_portfolio_risk_usd.max(1.0);
    let risk_ratio = risk_used / risk_max;
    lines.push(format!(
        "  risk  ${:.0}/${:.0}  {}",
        risk_used,
        risk_max,
        bar(risk_ratio, 8)
    ));

    if rules.llm.enabled {
        lines.push(format!(
            "  LLM  {} / {}",
            short_model(rules.llm.effective_selection_model()),
            short_model(rules.llm.effective_monitor_model())
        ));
    } else {
        lines.push(format!("  LLM  {}", dim.apply_to("disabled")));
    }

    if rules.schedule.overnight.enabled {
        lines.push(format!(
            "  overnight  every {}  {}",
            format_duration_secs(rules.schedule.overnight.tick_interval_seconds),
            if rules.schedule.overnight.web_digest {
                "web digest"
            } else {
                "no digest"
            }
        ));
    }

    panel("Rules", &lines, inner)
}

fn render_positions_panel(ctx: &DashboardContext, max_width: usize) -> String {
    if ctx.state.open_positions.is_empty() {
        return String::new();
    }

    let hold = Style::new().green();
    let mut lines = Vec::new();
    for pos in ctx.state.open_positions.values() {
        let credit = pos
            .entry_credit
            .map(|c| format!("  cr ${c:.2}"))
            .unwrap_or_default();
        let opened = ago_from_dt(pos.opened_at);
        lines.push(format!(
            "  {}  {}  exp {}  max loss ${:.0}{}  — {}  {}",
            pos.underlying,
            pos.strategy,
            pos.expiry,
            pos.max_loss_usd,
            credit,
            hold.apply_to("holding"),
            Style::new().dim().apply_to(opened)
        ));
    }
    panel_fit(
        &format!("Positions ({})", ctx.state.open_positions.len()),
        &lines,
        max_width,
    )
}

fn render_activity_panel(ctx: &DashboardContext, max_width: usize) -> String {
    let actions: Vec<_> = ctx.state.last_actions.iter().rev().take(8).collect();
    if actions.is_empty() {
        return String::new();
    }

    let dim = Style::new().dim();
    let mut lines = Vec::new();
    for act in actions {
        let time = act.at.format("%H:%M").to_string();
        let detail = format_action_detail(&act.action, &act.detail);
        lines.push(format!(
            "  {}  {:<16} {}",
            dim.apply_to(time),
            Style::new().bold().apply_to(&act.action),
            detail
        ));
    }
    panel_fit("Recent Activity", &lines, max_width)
}

fn render_log_panel(ctx: &DashboardContext, max_width: usize) -> String {
    if ctx.log_tail.is_empty() {
        return String::new();
    }
    let dim = Style::new().dim();
    let lines: Vec<String> = ctx
        .log_tail
        .iter()
        .map(|l| {
            let max_content = max_width.saturating_sub(6);
            let trimmed = if strip_log_len(l) > max_content {
                format!("{}…", truncate_visible(l, max_content.saturating_sub(1)))
            } else {
                l.clone()
            };
            format!("  {}", dim.apply_to(trimmed))
        })
        .collect();
    panel_fit("Agent Log (tail)", &lines, max_width)
}

fn strip_log_len(s: &str) -> usize {
    console::strip_ansi_codes(s).len()
}

fn truncate_visible(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn render_footer_hint() -> String {
    Style::new()
        .dim()
        .apply_to(
            "  schwab  dashboard · watch · rules show · agent status · agent run · agent stop · help",
        )
        .to_string()
}

fn format_action_detail(action: &str, detail: &serde_json::Value) -> String {
    match action {
        "llm_review" => detail
            .get("phase")
            .and_then(|v| v.as_str())
            .map(|p| {
                let rec = detail
                    .pointer("/new_entries/recommendation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("—");
                format!("{p} → entries {rec}")
            })
            .unwrap_or_else(|| "review".into()),
        "overnight_digest" => detail
            .get("market_commentary")
            .and_then(|v| v.as_str())
            .map(|s| {
                let t = s.chars().take(60).collect::<String>();
                if s.len() > 60 {
                    format!("{t}…")
                } else {
                    t
                }
            })
            .unwrap_or_else(|| "digest".into()),
        other => {
            let s = detail.to_string();
            if s.len() > 50 {
                format!("{other} …")
            } else {
                s
            }
        }
    }
}

fn session_style(session: &str) -> Style {
    match session {
        "regular" => Style::new().green(),
        "overnight" => Style::new().magenta(),
        "idle" => Style::new().dim(),
        _ => Style::new().dim(),
    }
}

fn short_model(model: &str) -> String {
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .chars()
        .take(20)
        .collect()
}

fn ago_from_dt(dt: DateTime<Utc>) -> String {
    ago_secs((Utc::now() - dt).num_seconds())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn dashboard_renders_for_project_rules() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../rules/options-pilot-8709.yaml");
        if !path.exists() {
            return;
        }
        let ctx = DashboardContext::load(&path).unwrap();
        let out = render_dashboard(&ctx);
        assert!(out.contains("Schwab Agent"));
        assert!(out.contains(&ctx.rules.agent_id));
    }
}
