use ratatui::text::{Line, Span};
use ratatui::style::{Color, Modifier, Style};

use crate::ui::context::WatchContext;
use crate::ui::health::AgentHealth;

pub fn overview_agent_lines(ctx: &WatchContext, health: &AgentHealth, agent_mode: &str) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("trader_id: {}", ctx.rules.trader_id)),
        Line::from(format!("mode: {agent_mode}")),
        Line::from(format!(
            "agent: {}",
            if health.loop_running {
                "running"
            } else {
                "stopped"
            }
        )),
        Line::from(format!("tick_count: {}", ctx.state.tick_count)),
        Line::from(format!("trades_today: {}", ctx.state.trades_today)),
        Line::from(format!(
            "last_tick: {}",
            ctx.state
                .last_tick
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "—".into())
        )),
    ];
    if let Some(err) = &health.last_error {
        lines.push(Line::from(vec![
            Span::styled("error: ", Style::default().fg(Color::Red)),
            Span::raw(err.clone()),
        ]));
    }
    if let Some(stats) = crate::sim::compute_stats(&ctx.state) {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "Simulation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(format!(
            "equity: ${:.2} │ ROI {:.2}% │ closed {} │ win {:.1}%",
            stats.current_equity_usd,
            stats.roi_pct,
            stats.closed_trades,
            stats.win_rate_pct
        )));
    }
    lines
}

pub fn capital_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    let Some(c) = ctx.capital_check() else {
        return vec![Line::from("(no capital data — wait for first tick)")];
    };
    let f = |k: &str| c.get(k).and_then(|v| v.as_f64());
    vec![
        Line::from(format!(
            "cash_available: ${:.2}",
            f("cash_available").unwrap_or(0.0)
        )),
        Line::from(format!(
            "options_reserved: ${:.2}",
            f("options_reserved_usd").unwrap_or(0.0)
        )),
        Line::from(format!(
            "tradable_budget: ${:.2}",
            f("tradable_budget_usd").unwrap_or(0.0)
        )),
        Line::from(format!(
            "equity_deployed: ${:.2}",
            f("equity_deployed_usd").unwrap_or(0.0)
        )),
        Line::from(format!(
            "cap_remaining: ${:.2}",
            f("cap_remaining_usd").unwrap_or(0.0)
        )),
        Line::from(format!(
            "sleeve_cap: ${:.0}",
            ctx.rules.capital.fixed_sleeve_cap_usd
        )),
    ]
}

pub fn position_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    if ctx.state.open_positions.is_empty() {
        return vec![Line::from("(no open swing positions)")];
    }
    ctx.state
        .open_positions
        .values()
        .map(|p| {
            Line::from(format!(
                "{} x{:.0} @ ${:.2} │ stop ${:.2} │ target ${:.2}",
                p.symbol, p.quantity, p.entry_price, p.stop_price, p.profit_limit
            ))
        })
        .collect()
}

pub fn candidate_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(scan) = ctx.scan() {
        if let Some(cands) = scan.get("candidates").and_then(|v| v.as_array()) {
            lines.push(Line::from(vec![Span::styled(
                format!("Candidates ({})", cands.len()),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )]));
            for c in cands {
                let sym = c.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
                let rsi = c
                    .pointer("/technical_context/rsi_14")
                    .and_then(|v| v.as_f64())
                    .map(|r| format!("RSI {r:.1}"))
                    .unwrap_or_default();
                lines.push(Line::from(format!("  + {sym}  {rsi}")));
            }
        }
        if let Some(rej) = scan.get("rejected").and_then(|v| v.as_array()) {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!("Rejected ({})", rej.len()),
                Style::default().fg(Color::DarkGray),
            )]));
            for r in rej.iter().take(8) {
                let sym = r.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
                let reason = r.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(Line::from(format!("  - {sym}: {reason}")));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("(no scan data yet)"));
    }
    lines
}

pub fn entry_attempt_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    let Some(tick) = ctx.last_tick() else {
        return vec![Line::from("(no entry attempts yet)")];
    };
    let Some(arr) = tick.get("entry_attempts").and_then(|v| v.as_array()) else {
        return vec![Line::from("(no entry attempts this tick)")];
    };
    arr.iter()
        .map(|e| {
            let status = e.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let sym = e
                .pointer("/attempt/symbol")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let reason = e
                .pointer("/attempt/reason")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if reason.is_empty() {
                Line::from(format!("{status}: {sym}"))
            } else {
                Line::from(format!("{status}: {sym} — {reason}"))
            }
        })
        .collect()
}

pub fn llm_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    let Some(llm) = ctx.llm() else {
        return vec![Line::from("(no LLM review yet)")];
    };
    if let Some(err) = llm.get("error").and_then(|v| v.as_str()) {
        return vec![Line::from(format!("error: {err}"))];
    }
    let mut lines = vec![
        Line::from(format!(
            "phase: {}",
            llm.get("phase").and_then(|v| v.as_str()).unwrap_or("?")
        )),
        Line::from(format!(
            "entry: {}",
            llm.get("entry_recommendation")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
        )),
        Line::from(""),
        Line::from(
            llm.get("market_commentary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
    ];
    if let Some(alerts) = llm.get("risk_alerts").and_then(|v| v.as_array()) {
        for a in alerts {
            if let Some(s) = a.as_str() {
                lines.push(Line::from(vec![Span::styled(
                    format!("⚠ {s}"),
                    Style::default().fg(Color::Yellow),
                )]));
            }
        }
    }
    lines
}

pub fn journal_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    if ctx.journal_tail.is_empty() {
        return vec![Line::from("(journal empty)")];
    }
    ctx.journal_tail.iter().map(|l| Line::from(l.clone())).collect()
}

pub fn log_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    if ctx.log_tail.is_empty() {
        return vec![Line::from("(no log yet)")];
    }
    ctx.log_tail.iter().map(|l| Line::from(l.clone())).collect()
}

pub fn rules_summary(ctx: &WatchContext) -> Vec<Line<'static>> {
    vec![
        Line::from(format!("style: {}", ctx.rules.playbook.style)),
        Line::from(format!("direction: {}", ctx.rules.playbook.direction)),
        Line::from(format!(
            "watchlist: {}",
            ctx.rules.all_watchlist_symbols().len()
        )),
        Line::from(format!(
            "profit/stop: {:.0}% / {:.0}%",
            ctx.rules.playbook.exit.profit_target_pct,
            ctx.rules.playbook.exit.stop_loss_pct
        )),
        Line::from(format!(
            "llm: {}",
            if ctx.rules.llm.enabled {
                "on"
            } else {
                "off"
            }
        )),
    ]
}
