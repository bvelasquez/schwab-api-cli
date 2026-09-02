use ratatui::text::{Line, Span};
use ratatui::style::{Color, Modifier, Style};

use crate::ui::context::{llm_entry_recommendation, WatchContext};
use crate::ui::health::AgentHealth;
use schwab_cli::market_conditions::{market_conditions_lines, MarketConditionsSnapshot};

pub fn market_conditions_panel_lines(snapshot: &MarketConditionsSnapshot) -> Vec<Line<'static>> {
    market_conditions_lines(snapshot)
}

pub fn overview_agent_lines(ctx: &WatchContext, health: &AgentHealth, agent_mode: &str) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("trader_id: {}", ctx.rules.trader_id)),
        Line::from(format!("mode: {agent_mode}")),
        Line::from(format!("agent: {}", health.status_label())),
        Line::from(format!("exits_armed: {}", health.exits_armed())),
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
    if health.restart_count > 0 {
        lines.push(Line::from(format!("restarts: {}", health.restart_count)));
    }
    if let Some(err) = &health.last_error {
        lines.push(Line::from(vec![
            Span::styled("error: ", Style::default().fg(Color::Red)),
            Span::raw(err.clone()),
        ]));
    }
    if !health.exits_armed() {
        lines.push(Line::from(vec![Span::styled(
            "AGENT DOWN — exits not executing",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]));
    }
    lines
}

/// Full-width overview strip for plan dollar P/L + ROI %.
pub fn plan_pnl_panel_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    let sleeve = ctx.rules.capital.fixed_sleeve_cap_usd.max(0.01);
    let (pnl_usd, roi_pct, detail) = if let Some(stats) = crate::sim::compute_stats(&ctx.state) {
        let pnl = stats.current_equity_usd - stats.starting_cash_usd;
        let detail = format!(
            "sleeve ${:.0}  ·  equity ${:.0}  ·  closed {}",
            stats.starting_cash_usd, stats.current_equity_usd, stats.closed_trades
        );
        (pnl, stats.roi_pct, detail)
    } else {
        let monitors = crate::ui::live::list_position_monitors(
            &ctx.rules,
            &ctx.state,
            ctx.live.as_ref(),
            chrono::Utc::now(),
        );
        let open_pnl: f64 = monitors.iter().map(|m| m.pnl_usd).sum();
        let roi = (open_pnl / sleeve) * 100.0;
        let detail = if monitors.is_empty() {
            format!("sleeve ${sleeve:.0}  ·  flat (open marks only in live)")
        } else {
            format!("sleeve ${sleeve:.0}  ·  {} open", monitors.len())
        };
        (open_pnl, roi, detail)
    };
    let color = if pnl_usd >= 0.0 {
        Color::LightGreen
    } else {
        Color::Red
    };
    vec![
        Line::from(vec![
            Span::styled(
                format!("  ${pnl_usd:+.2}"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                format!("{roi_pct:+.1}% ROI"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            format!("  {detail}"),
            Style::default().fg(Color::DarkGray),
        )),
    ]
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

pub fn position_lines(ctx: &WatchContext, exits_armed: bool) -> Vec<Line<'static>> {
    let monitors = crate::ui::live::list_position_monitors(
        &ctx.rules,
        &ctx.state,
        ctx.live.as_ref(),
        chrono::Utc::now(),
    );
    crate::ui::positions_panel::position_preview_lines(&monitors, exits_armed)
}

pub fn position_rules_context_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    crate::ui::live::regime_and_rules_lines(ctx)
}

pub fn candidate_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    let mut lines = candidate_lines_core(ctx);
    // Overlay live quotes onto symbol header lines (geometry lines stay put).
    if let (Some(live), Some(scan)) = (ctx.live.as_ref(), ctx.scan()) {
        let mut line_idx = 0usize;
        while line_idx < lines.len() {
            // Find lines that start with "  + SYM" or "  - SYM"
            let plain = lines[line_idx]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>();
            let is_cand = plain.starts_with("  + ");
            let is_rej = plain.starts_with("  - ");
            if is_cand || is_rej {
                let rest = plain.trim_start();
                let sym = rest
                    .trim_start_matches("+ ")
                    .trim_start_matches("- ")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_uppercase();
                if let Some(q) = live.quotes.get(&sym) {
                    if is_cand {
                        let rsi = scan
                            .get("candidates")
                            .and_then(|v| v.as_array())
                            .into_iter()
                            .flatten()
                            .find(|c| {
                                c.get("symbol")
                                    .and_then(|s| s.as_str())
                                    .is_some_and(|s| s.eq_ignore_ascii_case(&sym))
                            })
                            .and_then(|c| c.pointer("/technical_context/rsi_14"))
                            .and_then(|v| v.as_f64())
                            .map(|r| format!("RSI {r:.1}"))
                            .unwrap_or_default();
                        lines[line_idx] =
                            Line::from(format!("  + {sym}  last ${:.2}  {rsi}", q.last));
                    }
                }
            }
            line_idx += 1;
        }
    }
    lines
}

fn geometry_line_for_row(
    effective: &crate::rules::TraderRules,
    row: &serde_json::Value,
    live: Option<&crate::ui::live::WatchLiveSnapshot>,
) -> Line<'static> {
    use crate::capital::{exit_geometry, format_exit_geometry_brief};

    let sym = row.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
    let last = row
        .pointer("/technical_context/last")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            live.and_then(|l| l.quotes.get(&sym.to_uppercase()))
                .map(|q| q.last)
        })
        .unwrap_or(0.0);
    let atr = row
        .pointer("/technical_context/atr_14")
        .and_then(|v| v.as_f64());
    let lookback = effective
        .playbook
        .exit
        .profit_target_recent_range_cap
        .lookback_days;
    let range = if effective.playbook.exit.profit_target_recent_range_cap.enabled {
        let high_key = match lookback {
            0..=30 => "high_20d",
            31..=75 => "high_60d",
            _ => "high_90d",
        };
        let low_key = match lookback {
            0..=30 => "low_20d",
            31..=75 => "low_60d",
            _ => "low_90d",
        };
        let hf = |k: &str| {
            row.pointer(&format!("/technical_context/history_features/{k}"))
                .and_then(|v| v.as_f64())
        };
        crate::capital::ExitRangeContext {
            recent_high: hf(high_key).or_else(|| hf("high_60d")),
            recent_low: hf(low_key).or_else(|| hf("low_60d")),
        }
    } else {
        crate::capital::ExitRangeContext::none()
    };
    if last > 0.0 {
        let g = exit_geometry(last, effective, atr, range);
        Line::from(vec![Span::styled(
            format!("      → {}", format_exit_geometry_brief(&g)),
            Style::default().fg(Color::Cyan),
        )])
    } else {
        Line::from(Span::styled(
            "      → (no price for target preview)",
            Style::default().fg(Color::DarkGray),
        ))
    }
}

fn candidate_lines_core(ctx: &WatchContext) -> Vec<Line<'static>> {
    use crate::adaptation::effective_rules;

    let mut lines = Vec::new();
    let effective = effective_rules(&ctx.rules, &ctx.state);

    let session = ctx.last_session_label();
    if ctx.scan_is_stale() {
        lines.push(Line::from(vec![Span::styled(
            format!(
                "session={session} — showing last regular-hours scan (market not in regular session)"
            ),
            Style::default().fg(Color::Yellow),
        )]));
    } else if ctx.scan().is_none() {
        let hint = match session {
            "idle" => "Market closed (idle). Scan + FMP discover run in regular hours — check back after the open.",
            "overnight" => "Overnight session — no scan. Wait for regular hours.",
            "premarket" => "Premarket — wait for the open for a full scan.",
            _ => "Waiting for first regular-hours tick…",
        };
        lines.push(Line::from(vec![Span::styled(
            hint.to_string(),
            Style::default().fg(Color::Yellow),
        )]));
        if ctx.rules.sources.fmp.enabled {
            let last = ctx
                .state
                .last_fmp_discover_at
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "never".into());
            lines.push(Line::from(format!(
                "FMP every {}m · last {last} · dynamic {:?}",
                ctx.rules.sources.fmp.discover_every_minutes,
                ctx.state.fmp_dynamic_symbols
            )));
        }
        return lines;
    }

    if let Some(scan) = ctx.scan() {
        if let Some(cands) = scan.get("candidates").and_then(|v| v.as_array()) {
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "Candidates ({}) — likely brackets from ATR/horizon",
                    cands.len()
                ),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )]));
            if cands.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  (none passed filters — see Rejected brackets below)",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            for c in cands {
                let sym = c.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
                let rsi = c
                    .pointer("/technical_context/rsi_14")
                    .and_then(|v| v.as_f64())
                    .map(|r| format!("RSI {r:.1}"))
                    .unwrap_or_default();
                lines.push(Line::from(format!("  + {sym}  {rsi}")));
                lines.push(geometry_line_for_row(&effective, c, ctx.live.as_ref()));
            }
        }
        if let Some(rej) = scan.get("rejected").and_then(|v| v.as_array()) {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!("Rejected ({}) — would-be brackets if they passed", rej.len()),
                Style::default().fg(Color::DarkGray),
            )]));
            for r in rej.iter().take(12) {
                let sym = r.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
                let reason = r.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(Line::from(format!("  - {sym}: {reason}")));
                if r.pointer("/technical_context/last").is_some()
                    || r.pointer("/technical_context/atr_14").is_some()
                {
                    lines.push(geometry_line_for_row(&effective, r, ctx.live.as_ref()));
                }
            }
            if rej.len() > 12 {
                lines.push(Line::from(format!(
                    "  … +{} more rejected",
                    rej.len() - 12
                )));
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
    let llm = ctx.resolved_llm();

    let mut header = vec![
        Line::from(format!(
            "session: {}  │  phase: {}",
            ctx.session_label(),
            ctx.llm_phase().unwrap_or("—")
        )),
    ];
    if let Some(open) = ctx.market_open() {
        let style = if open {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Yellow)
        };
        header.push(Line::from(Span::styled(
            if open {
                "market: OPEN (regular session)"
            } else {
                "market: CLOSED"
            },
            style,
        )));
    }
    if let Some(reason) = ctx.entry_block_reason() {
        header.push(Line::from(vec![
            Span::styled("entries blocked: ", Style::default().fg(Color::Yellow)),
            Span::raw(reason.to_string()),
        ]));
    }
    if let Some(ts) = ctx.state.last_tick.as_ref() {
        let llm_tag = if ctx.llm_ran_this_tick() {
            "LLM reviewed this tick"
        } else {
            "no new LLM this tick"
        };
        header.push(Line::from(format!(
            "agent tick {}  │  {}  │  {}",
            ctx.state.tick_count,
            ts.format("%H:%M:%S UTC"),
            llm_tag
        )));
    }
    header.push(Line::from(""));

    let Some(llm) = llm else {
        header.push(Line::from(
            "(no LLM review yet — monitor runs every few ticks when entries are blocked)",
        ));
        return header;
    };

    if let Some(err) = llm.get("error").and_then(|v| v.as_str()) {
        header.push(Line::from(format!("error: {err}")));
        return header;
    }

    let rec = llm_entry_recommendation(llm).unwrap_or_else(|| {
        if ctx.entry_block_reason().is_some() {
            "n/a (entries blocked)"
        } else {
            "—"
        }
    });
    let rec_style = match rec {
        "proceed" => Style::default().fg(Color::Green),
        "defer" | "skip" => Style::default().fg(Color::Yellow),
        "n/a (entries blocked)" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::Cyan),
    };

    let mut lines = header;
    lines.push(Line::from(vec![
        Span::raw("entry recommendation: "),
        Span::styled(rec.to_string(), rec_style),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(
        llm.get("market_commentary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    ));
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
    if !ctx.llm_ran_this_tick() && ctx.state.last_llm_summary.is_some() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(showing last stored LLM review — monitor runs on a schedule, not every tick)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

pub fn journal_lines(ctx: &WatchContext) -> Vec<Line<'static>> {
    crate::ui::journal_view::format_journal_events(&ctx.journal_events)
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
            "profit/stop ceil: {:.0}% / {:.0}%",
            ctx.rules.playbook.exit.profit_target_pct,
            ctx.rules.playbook.exit.stop_loss_pct
        )),
        Line::from(crate::capital::format_exit_cap_rules(&ctx.rules)),
        Line::from(format!(
            "hold target_days: {}",
            ctx.rules.playbook.holding_period.target_days
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
