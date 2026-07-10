//! Ratatui-native render helpers (no ANSI / unicode box panels).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Gauge;

use super::agent_health::{format_tick_error, SharedAgentHealth};
use super::context::DashboardContext;
use super::market_status::market_label;
use super::watch::WatchAgentMode;
use super::{ago_secs, format_duration_secs};
use crate::auth_reminder::AuthReminderLevel;
use crate::market_conditions::{market_conditions_lines, MarketConditionsSnapshot};

fn format_position_summary(state: &crate::agent::state::AgentState) -> String {
    let spreads = state.open_positions.len();
    if spreads == 0 {
        return "flat".into();
    }
    let contracts = state.total_contracts();
    if contracts > spreads as u32 {
        format!(
            "{} spread{} · {} ct",
            spreads,
            if spreads == 1 { "" } else { "s" },
            contracts
        )
    } else {
        format!("{} spread{}", spreads, if spreads == 1 { "" } else { "s" })
    }
}

fn truncate_err(msg: &str, max: usize) -> String {
    if msg.len() <= max {
        msg.to_string()
    } else {
        format!("{}…", &msg[..max.saturating_sub(1)])
    }
}

fn embedded_agent_label(
    ctx: &DashboardContext,
    health: Option<&SharedAgentHealth>,
) -> (String, bool) {
    let Some(h) = health.and_then(|h| h.lock().ok()) else {
        return ("running (in-process)".into(), true);
    };
    if h.auth_required {
        return ("auth required — schwab auth login".into(), false);
    }
    if !h.loop_running {
        let err = h
            .last_error
            .as_deref()
            .map(|e| truncate_err(&format_tick_error(e), 48))
            .unwrap_or_else(|| "exited".into());
        return (format!("stopped ({err})"), false);
    }
    if ctx.tick_is_stale() && h.ticks_completed == 0 {
        let starting = h.started_at.elapsed().as_secs();
        let err = h
            .last_error
            .as_deref()
            .map(|e| truncate_err(&format_tick_error(e), 36))
            .unwrap_or_else(|| {
                if starting > 120 {
                    format!("no tick in {}s", starting)
                } else {
                    "starting…".into()
                }
            });
        return (format!("running ({err})"), true);
    }
    if ctx.tick_is_stale() {
        return ("running (tick stale)".into(), true);
    }
    (format!("running ({} ticks)", h.ticks_completed), true)
}

pub fn header_line(
    ctx: &DashboardContext,
    agent_mode: WatchAgentMode,
    agent_health: Option<&SharedAgentHealth>,
) -> Line<'static> {
    let daemon = match agent_mode {
        WatchAgentMode::Embedded => {
            let (label, ok) = embedded_agent_label(ctx, agent_health);
            let style = if ok && !ctx.tick_is_stale() {
                Style::default().fg(Color::Green)
            } else if ok {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Red)
            };
            Span::styled(format!("● {label} "), style)
        }
        WatchAgentMode::External => Span::styled(
            format!("● running pid {} ", ctx.daemon.pid.unwrap_or(0)),
            Style::default().fg(Color::Green),
        ),
        WatchAgentMode::MonitorOnly => {
            if ctx.daemon.running {
                Span::styled(
                    format!("● running pid {} ", ctx.daemon.pid.unwrap_or(0)),
                    Style::default().fg(Color::Green),
                )
            } else {
                Span::styled("○ stopped ", Style::default().fg(Color::Red))
            }
        }
    };

    let session = ctx.effective_session();
    let session_style = match session {
        "regular" => Style::default().fg(Color::Green),
        "overnight" => Style::default().fg(Color::Magenta),
        _ => Style::default().fg(Color::DarkGray),
    };

    let (mkt_label, mkt_open) = market_label(ctx.market_status, Some(session));
    let mkt_style = if mkt_open {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    };

    Line::from(vec![
        Span::styled(
            format!("✦ {} ", ctx.rules.agent_id),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        daemon,
        Span::raw("· "),
        Span::styled(mkt_label.to_string(), mkt_style),
        Span::raw(" · "),
        Span::styled(session.to_string(), session_style),
        Span::raw(format!(
            " · {} · tick {}s",
            format_position_summary(&ctx.state),
            ctx.expected_tick_interval_secs()
        )),
    ])
}

pub fn agent_status_lines(
    ctx: &DashboardContext,
    agent_mode: WatchAgentMode,
    agent_health: Option<&SharedAgentHealth>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let daemon_val = match agent_mode {
        WatchAgentMode::Embedded => embedded_agent_label(ctx, agent_health).0,
        WatchAgentMode::External => format!("running (pid {})", ctx.daemon.pid.unwrap_or(0)),
        WatchAgentMode::MonitorOnly => {
            if ctx.daemon.running {
                format!("running (pid {})", ctx.daemon.pid.unwrap_or(0))
            } else {
                "stopped".into()
            }
        }
    };
    lines.push(kv_line("daemon", daemon_val, 12));

    let session = ctx.effective_session();
    let (mkt_label, _) = market_label(ctx.market_status, Some(session));
    lines.push(kv_line("EQO regular", mkt_label.to_string(), 12));
    lines.push(kv_line(
        "session",
        format!(
            "{} (now){}",
            session,
            ctx.state
                .last_session
                .as_deref()
                .filter(|s| *s != session)
                .map(|s| format!(" · saved: {s}"))
                .unwrap_or_default()
        ),
        12,
    ));

    if let Some(reminder) = ctx.auth_reminder.as_ref() {
        if reminder.level != AuthReminderLevel::None {
            let style = match reminder.level {
                AuthReminderLevel::Soon => Style::default().fg(Color::Yellow),
                AuthReminderLevel::Urgent | AuthReminderLevel::Expired => {
                    Style::default().fg(Color::Red)
                }
                AuthReminderLevel::None => Style::default(),
            };
            lines.push(Line::from(vec![
                Span::styled("  Schwab auth ", Style::default().fg(Color::DarkGray)),
                Span::styled(reminder.message.clone(), style),
            ]));
            lines.push(Line::from(vec![
                Span::styled("               ", Style::default().fg(Color::DarkGray)),
                Span::styled(reminder.detail_line(), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    if let Some(age) = ctx.last_tick_age_secs() {
        let tick_label = ago_secs(age);
        let style = if ctx.tick_is_stale() {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled("  last tick  ", Style::default().fg(Color::DarkGray)),
            Span::styled(tick_label, style),
        ]));
    } else {
        lines.push(kv_line("last tick", "never".into(), 12));
    }

    lines.push(kv_line(
        "next tick",
        format!(
            "~{}",
            format_duration_secs(ctx.expected_tick_interval_secs())
        ),
        12,
    ));
    lines.push(kv_line(
        "positions",
        format!(
            "{} · {} pending",
            format_position_summary(&ctx.state),
            ctx.state.pending_count()
        ),
        12,
    ));
    lines.push(kv_line(
        "trades today",
        format!(
            "{}/{}",
            ctx.state.trades_today, ctx.rules.risk.max_trades_per_day
        ),
        12,
    ));
    if let Some(h) = agent_health.and_then(|h| h.lock().ok()) {
        if let Some(err) = h.last_error.as_ref() {
            lines.push(Line::from(vec![
                Span::styled("  last error ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format_tick_error(err),
                    Style::default().fg(Color::Red),
                ),
            ]));
        }
    }
    if ctx.rules.llm.enabled {
        let phase = ctx
            .state
            .last_llm_summary
            .as_ref()
            .and_then(|v| v.get("phase"))
            .and_then(|v| v.as_str())
            .unwrap_or("—");
        lines.push(kv_line(
            "LLM",
            format!("{phase} · {} reviews", ctx.state.llm_review_count),
            12,
        ));
    }
    lines
}

pub fn market_conditions_panel_lines(snapshot: &MarketConditionsSnapshot) -> Vec<Line<'static>> {
    market_conditions_lines(snapshot)
}

pub fn rules_summary_lines(ctx: &DashboardContext) -> Vec<Line<'static>> {
    let rules = &ctx.rules;
    let mut lines = vec![
        kv_line(
            "tick",
            format!(
                "{} · monitor ~{}m",
                format_duration_secs(rules.schedule.tick_interval_seconds),
                ctx.monitor_interval_minutes()
            ),
            10,
        ),
        kv_line("watchlist", rules.watchlist.join(", "), 10),
    ];

    if rules.strategies.vertical.enabled {
        let v = &rules.entry_rules.vertical;
        lines.push(kv_line(
            "vertical",
            format!("{} {}–{} DTE", v.r#type, v.dte_min, v.dte_max),
            10,
        ));
    }
    if rules.strategies.iron_condor.enabled {
        let ic = &rules.entry_rules.iron_condor;
        lines.push(kv_line(
            "iron condor",
            format!("{}–{} DTE", ic.dte_min, ic.dte_max),
            10,
        ));
    }

    if rules.llm.enabled {
        lines.push(kv_line(
            "LLM",
            format!(
                "{} / {}",
                short_model(rules.llm.effective_selection_model()),
                short_model(rules.llm.effective_monitor_model())
            ),
            10,
        ));
    }

    if rules.schedule.overnight.enabled {
        lines.push(kv_line(
            "overnight",
            format!(
                "every {} · digest {}",
                format_duration_secs(rules.schedule.overnight.tick_interval_seconds),
                if rules.schedule.overnight.web_digest {
                    "on"
                } else {
                    "off"
                }
            ),
            10,
        ));
    }

    lines
}

pub fn risk_gauge(ctx: &DashboardContext) -> Gauge<'static> {
    let used = ctx.portfolio_risk_usd();
    let max = ctx.rules.risk.max_portfolio_risk_usd.max(1.0);
    let ratio = (used / max).clamp(0.0, 1.0);
    Gauge::default()
        .gauge_style(
            Style::default()
                .fg(if ratio > 0.8 {
                    Color::Red
                } else if ratio > 0.5 {
                    Color::Yellow
                } else {
                    Color::Green
                })
                .bg(Color::DarkGray),
        )
        .ratio(ratio)
        .label(format!("risk ${used:.0} / ${max:.0}"))
}

pub fn activity_lines(ctx: &DashboardContext) -> Vec<Line<'static>> {
    ctx.state
        .last_actions
        .iter()
        .rev()
        .take(12)
        .map(|act| {
            let time = act.at.format("%H:%M").to_string();
            let detail = format_action_detail(&act.action, &act.detail);
            Line::from(vec![
                Span::styled(format!("{time} "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:<14} ", act.action),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(detail),
            ])
        })
        .collect()
}

/// Compact latest LLM review for the overview panel.
pub fn latest_llm_lines(ctx: &DashboardContext) -> Vec<Line<'static>> {
    if !ctx.rules.llm.enabled {
        return vec![Line::from(Span::styled(
            "LLM disabled in rules",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    let review = latest_llm_review(ctx);
    let Some(review) = review else {
        return vec![Line::from(Span::styled(
            "No LLM reviews yet",
            Style::default().fg(Color::DarkGray),
        ))];
    };

    let mut lines = format_llm_review_lines(review, None);
    if lines.len() > 8 {
        lines.truncate(8);
        lines.push(Line::from(Span::styled(
            "  … see LLM tab (5) for full history",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

/// Scrollable LLM review history (newest first).
pub fn llm_history_lines(ctx: &DashboardContext) -> Vec<Line<'static>> {
    if !ctx.rules.llm.enabled {
        return vec![Line::from("LLM disabled in rules")];
    }

    let reviews: Vec<_> = ctx
        .state
        .last_actions
        .iter()
        .rev()
        .filter(|a| matches!(a.action.as_str(), "llm_review" | "overnight_digest"))
        .take(10)
        .collect();

    if reviews.is_empty() {
        if let Some(summary) = ctx.state.last_llm_summary.as_ref() {
            return format_llm_review_lines(summary, Some("latest"));
        }
        return vec![Line::from(Span::styled(
            "No LLM reviews yet",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    let mut out = Vec::new();
    for (i, act) in reviews.iter().enumerate() {
        if i > 0 {
            out.push(Line::from("────────────────────────────────────────"));
        }
        let stamp = act.at.format("%Y-%m-%d %H:%M").to_string();
        out.extend(format_llm_review_lines(&act.detail, Some(&stamp)));
    }
    out
}

fn latest_llm_review(ctx: &DashboardContext) -> Option<&serde_json::Value> {
    ctx.state
        .last_actions
        .iter()
        .rev()
        .find(|a| matches!(a.action.as_str(), "llm_review" | "overnight_digest"))
        .map(|a| &a.detail)
        .or(ctx.state.last_llm_summary.as_ref())
}

fn format_llm_review_lines(
    review: &serde_json::Value,
    timestamp: Option<&str>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let phase = review
        .get("phase")
        .and_then(|v| v.as_str())
        .unwrap_or("review");
    let phase_label = match phase {
        "overnight_digest" => "overnight digest",
        other => other,
    };
    let model = review
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("model");
    let web = review
        .get("used_web")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut header = format!("{phase_label} ({model}");
    if web {
        header.push_str(" + web");
    }
    header.push(')');
    if let Some(ts) = timestamp {
        header = format!("{ts}  {header}");
    }
    lines.push(Line::from(vec![Span::styled(
        header,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));

    if let Some(rec) = review
        .pointer("/new_entries/recommendation")
        .and_then(|v| v.as_str())
    {
        let style = match rec {
            "proceed" => Style::default().fg(Color::Green),
            "defer" | "skip" => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Cyan),
        };
        lines.push(Line::from(vec![
            Span::styled("  entries: ", Style::default().fg(Color::DarkGray)),
            Span::styled(rec.to_string(), style),
        ]));
    }

    if let Some(reason) = review
        .pointer("/new_entries/reasoning")
        .and_then(|v| v.as_str())
    {
        if !reason.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  why: ", Style::default().fg(Color::DarkGray)),
                Span::raw(reason.to_string()),
            ]));
        }
    }

    if let Some(commentary) = review.get("market_commentary").and_then(|v| v.as_str()) {
        if !commentary.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  market: ", Style::default().fg(Color::DarkGray)),
                Span::raw(commentary.to_string()),
            ]));
        }
    }

    if let Some(alerts) = review.get("risk_alerts").and_then(|v| v.as_array()) {
        for alert in alerts {
            if let Some(s) = alert.as_str() {
                lines.push(Line::from(vec![
                    Span::styled("  alert: ", Style::default().fg(Color::Red)),
                    Span::raw(s.to_string()),
                ]));
            }
        }
    }

    if let Some(positions) = review.get("positions").and_then(|v| v.as_array()) {
        for pos in positions {
            let id = pos
                .get("position_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let rec = pos
                .get("recommendation")
                .and_then(|v| v.as_str())
                .unwrap_or("hold");
            let urgency = pos.get("urgency").and_then(|v| v.as_str()).unwrap_or("");
            let urg = if urgency.is_empty() {
                String::new()
            } else {
                format!(" ({urgency})")
            };
            lines.push(Line::from(format!("    • {id}: {rec}{urg}")));
        }
    }

    lines
}

pub fn rules_detail_lines(ctx: &DashboardContext) -> Vec<Line<'static>> {
    let rules = &ctx.rules;
    let mut out = Vec::new();

    push_section(&mut out, "Accounts");
    for a in rules.accounts.iter().filter(|a| a.enabled) {
        let label = a.label.as_deref().unwrap_or("account");
        let hash = &a.hash[..a.hash.len().min(8)];
        out.push(Line::from(format!("  {label}  {hash}…  {:?}", a.r#type)));
    }

    push_section(&mut out, "Schedule");
    let s = &rules.schedule;
    out.push(kv_line(
        "tick",
        format!(
            "{} ({})",
            format_duration_secs(s.tick_interval_seconds),
            s.tick_interval_seconds
        ),
        14,
    ));
    out.push(kv_line("timezone", s.timezone.clone(), 14));
    out.push(kv_line(
        "market hours",
        if s.market_hours_only { "yes" } else { "no" }.into(),
        14,
    ));
    if s.overnight.enabled {
        out.push(kv_line(
            "overnight",
            format!(
                "every {} · digest {}",
                format_duration_secs(s.overnight.tick_interval_seconds),
                if s.overnight.web_digest { "on" } else { "off" }
            ),
            14,
        ));
    } else {
        out.push(kv_line("overnight", "disabled".into(), 14));
    }

    push_section(&mut out, "Entry");
    if rules.strategies.vertical.enabled {
        let v = &rules.entry_rules.vertical;
        out.push(Line::from(format!("  vertical ({})", v.r#type)));
        out.push(Line::from(format!(
            "    DTE {}–{} · credit ≥ ${:.2} · width ${:.0}",
            v.dte_min, v.dte_max, v.min_credit, v.max_width
        )));
        out.push(Line::from(format!(
            "    δ {:.2}–{:.2} · max {} pos",
            v.short_delta_min, v.short_delta_max, v.max_open_positions
        )));
    }
    if rules.strategies.iron_condor.enabled {
        let ic = &rules.entry_rules.iron_condor;
        out.push(Line::from(format!(
            "    iron condor {}–{} DTE · credit ≥ ${:.2}",
            ic.dte_min, ic.dte_max, ic.min_credit
        )));
    }

    push_section(&mut out, "Exit");
    let ex = &rules.exit_rules;
    out.push(kv_line(
        "profit target",
        format!("{}%", ex.profit_target_pct),
        14,
    ));
    out.push(kv_line(
        "stop loss",
        format!("{}% credit", ex.stop_loss_pct),
        14,
    ));
    out.push(kv_line("close DTE", format!("≤{}", ex.dte_close), 14));

    push_section(&mut out, "Risk");
    let risk = &rules.risk;
    let used = ctx.portfolio_risk_usd();
    out.push(kv_line(
        "portfolio",
        format!("${:.0} / ${:.0}", used, risk.max_portfolio_risk_usd),
        14,
    ));
    out.push(kv_line(
        "per trade",
        format!("${:.0}", risk.max_risk_per_trade_usd),
        14,
    ));
    out.push(kv_line(
        "trades/day",
        risk.max_trades_per_day.to_string(),
        14,
    ));
    out.push(kv_line(
        "allowed",
        if risk.allowed_underlyings.is_empty() {
            rules.watchlist.join(", ")
        } else {
            risk.allowed_underlyings.join(", ")
        },
        14,
    ));

    push_section(&mut out, "LLM");
    let llm = &rules.llm;
    if llm.enabled {
        out.push(kv_line(
            "selection",
            llm.effective_selection_model().to_string(),
            14,
        ));
        out.push(kv_line(
            "monitor",
            llm.effective_monitor_model().to_string(),
            14,
        ));
        out.push(kv_line("web", llm.web_model.clone(), 14));
        out.push(kv_line(
            "LLM every",
            format!(
                "{} ticks (~{}m){}",
                llm.effective_llm_review_ticks(
                    ctx.has_open_positions(),
                    ctx.min_open_position_dte(),
                    rules.exit_rules.dte_close,
                ),
                ctx.monitor_interval_minutes(),
                llm.monitor_review_every_ticks
                    .map(|n| format!("  (slow {n}t >{}DTE)", rules.exit_rules.dte_close))
                    .unwrap_or_default()
            ),
            14,
        ));
        out.push(kv_line(
            "web research",
            format!("every {} reviews", llm.web_research_every_reviews),
            14,
        ));
        out.push(kv_line(
            "veto / exits",
            format!("{} / {}", llm.veto_entries, llm.allow_llm_exits),
            14,
        ));
    } else {
        out.push(Line::from("  disabled"));
    }

    push_section(&mut out, "Execution");
    let e = &rules.execution;
    out.push(kv_line("order type", e.order_type.clone(), 14));
    out.push(kv_line("preview", e.require_preview.to_string(), 14));
    out.push(kv_line("wait fill", e.wait_for_fill.to_string(), 14));
    out.push(kv_line(
        "timeout",
        format!("{}s", e.fill_timeout_seconds),
        14,
    ));

    out
}

pub fn daemon_hint(_ctx: &DashboardContext) -> Line<'static> {
    Line::from(vec![
        Span::styled("! ", Style::default().fg(Color::Yellow)),
        Span::raw("Agent not running — use "),
        Span::styled(
            "schwab watch --trust --yes",
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" to run agent + TUI, or "),
        Span::styled(
            "schwab agent run … --background",
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" for headless daemon"),
    ])
}

fn push_section(out: &mut Vec<Line<'static>>, title: &str) {
    out.push(Line::from(""));
    out.push(Line::from(vec![Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
}

fn kv_line(key: &str, value: String, key_width: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {key:width$}  ", width = key_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(value),
    ])
}

fn short_model(model: &str) -> String {
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .chars()
        .take(18)
        .collect()
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
            .map(str::to_string)
            .unwrap_or_else(|| "digest".into()),
        _ => action.to_string(),
    }
}
