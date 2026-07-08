use console::Style;

use super::context::DashboardContext;
use super::{bar, kv_line, panel_fit, terminal_width};

pub fn render_rules_detail(ctx: &DashboardContext) -> String {
    let max_w = terminal_width().min(88);
    let rules = &ctx.rules;
    let dim = Style::new().dim();
    let bold = Style::new().bold();

    let mut out = String::new();
    out.push('\n');
    out.push_str(&format!(
        "{}\n\n",
        bold.apply_to(format!("Rules — {}", rules.agent_id))
    ));
    out.push_str(&format!(
        "  {} {}\n\n",
        dim.apply_to("file:"),
        ctx.rules_path.display()
    ));

    let acct_lines: Vec<String> = rules
        .accounts
        .iter()
        .filter(|a| a.enabled)
        .map(|a| {
            let label = a.label.as_deref().unwrap_or("account");
            let hash = &a.hash[..a.hash.len().min(8)];
            format!("  {label}  {hash}…  {:?}", a.r#type)
        })
        .collect();
    out.push_str(&panel_fit("Accounts", &acct_lines, max_w));
    out.push_str("\n\n");

    let sched = &rules.schedule;
    let overnight_val = if sched.overnight.enabled {
        format!(
            "every {} · web digest {}",
            super::format_duration_secs(sched.overnight.tick_interval_seconds),
            if sched.overnight.web_digest {
                "on"
            } else {
                "off"
            }
        )
    } else {
        "disabled".into()
    };
    let sched_lines = vec![
        kv_line(
            "tick interval",
            &format!(
                "{} ({})",
                super::format_duration_secs(sched.tick_interval_seconds),
                sched.tick_interval_seconds
            ),
            16,
        ),
        kv_line("timezone", &sched.timezone, 16),
        kv_line(
            "market hours",
            if sched.market_hours_only { "yes" } else { "no" },
            16,
        ),
        kv_line("overnight", &overnight_val, 16),
    ];
    out.push_str(&panel_fit("Schedule", &sched_lines, max_w));
    out.push_str("\n\n");

    let mut entry_lines = Vec::new();
    if rules.strategies.vertical.enabled {
        let v = &rules.entry_rules.vertical;
        entry_lines.push(format!("  vertical ({})", v.r#type));
        entry_lines.push(format!(
            "    DTE {}–{}  ·  credit ≥ ${:.2}  ·  width ${:.0}",
            v.dte_min, v.dte_max, v.min_credit, v.max_width
        ));
        entry_lines.push(format!(
            "    delta {:.2}–{:.2}  ·  max {} pos  ·  {} contracts",
            v.short_delta_min, v.short_delta_max, v.max_open_positions, v.max_contracts_per_trade
        ));
        let pop = v
            .min_pop_pct
            .map(|p| format!("POP≥{p:.0}%"))
            .unwrap_or_else(|| "POP—".into());
        let be = v
            .min_distance_to_be_pct
            .map(|p| format!("BE cushion≥{p:.0}%"))
            .unwrap_or_else(|| "BE—".into());
        let ctw = v
            .min_credit_to_width_pct
            .map(|p| format!("cr/width≥{p:.0}%"))
            .unwrap_or_else(|| "cr/width≥12.5%".into());
        entry_lines.push(format!("    filters: {pop}  ·  {be}  ·  {ctw}"));
    }
    if rules.strategies.iron_condor.enabled {
        let ic = &rules.entry_rules.iron_condor;
        entry_lines.push("  iron condor".into());
        entry_lines.push(format!(
            "    DTE {}–{}  ·  credit ≥ ${:.2}  ·  wing ${:.0}  ·  δ {:.2}",
            ic.dte_min, ic.dte_max, ic.min_credit, ic.wing_width, ic.short_delta
        ));
    }
    if entry_lines.is_empty() {
        entry_lines.push("  (no strategies enabled)".into());
    }
    out.push_str(&panel_fit("Entry", &entry_lines, max_w));
    out.push_str("\n\n");

    let ex = &rules.exit_rules;
    let exit_lines = vec![
        kv_line("profit target", &format!("{}%", ex.profit_target_pct), 16),
        kv_line("stop loss", &format!("{}% of credit", ex.stop_loss_pct), 16),
        kv_line("close at DTE", &format!("≤{}", ex.dte_close), 16),
    ];
    out.push_str(&panel_fit("Exit (mechanical)", &exit_lines, max_w));
    out.push_str("\n\n");

    let risk = &rules.risk;
    let risk_used = ctx.portfolio_risk_usd();
    let risk_ratio = risk_used / risk.max_portfolio_risk_usd.max(1.0);
    let risk_lines = vec![
        format!(
            "  {:16}  ${:.0} / ${:.0}  {}",
            "portfolio risk",
            risk_used,
            risk.max_portfolio_risk_usd,
            bar(risk_ratio, 8)
        ),
        kv_line(
            "per trade max",
            &format!("${:.0}", risk.max_risk_per_trade_usd),
            16,
        ),
        kv_line("trades / day", &risk.max_trades_per_day.to_string(), 16),
        kv_line(
            "allowed",
            if risk.allowed_underlyings.is_empty() {
                "(watchlist)".into()
            } else {
                risk.allowed_underlyings.join(", ")
            }
            .as_str(),
            16,
        ),
    ];
    out.push_str(&panel_fit("Risk", &risk_lines, max_w));
    out.push_str("\n\n");

    let llm = &rules.llm;
    let llm_lines = if llm.enabled {
        vec![
            kv_line("selection", llm.effective_selection_model(), 16),
            kv_line("monitor", llm.effective_monitor_model(), 16),
            kv_line("web", &llm.web_model, 16),
            kv_line(
                "monitor every",
                &format!(
                    "{} ticks (~{}m){}",
                    llm.effective_monitor_review_ticks(
                        None,
                        rules.exit_rules.dte_close,
                    ),
                    ctx.monitor_interval_minutes(),
                    llm.monitor_review_every_ticks
                        .map(|n| format!("  (slow cadence {n}t above {} DTE)", rules.exit_rules.dte_close))
                        .unwrap_or_default()
                ),
                16,
            ),
            kv_line(
                "web research",
                &format!("every {} reviews", llm.web_research_every_reviews),
                16,
            ),
            kv_line(
                "veto / exits",
                &format!("{} / {}", llm.veto_entries, llm.allow_llm_exits),
                16,
            ),
        ]
    } else {
        vec!["  disabled".into()]
    };
    out.push_str(&panel_fit("LLM", &llm_lines, max_w));
    out.push_str("\n\n");

    let exec = &rules.execution;
    let exec_lines = vec![
        kv_line("order type", &exec.order_type, 16),
        kv_line("require preview", &exec.require_preview.to_string(), 16),
        kv_line("wait for fill", &exec.wait_for_fill.to_string(), 16),
        kv_line(
            "fill timeout",
            &format!("{}s", exec.fill_timeout_seconds),
            16,
        ),
    ];
    out.push_str(&panel_fit("Execution", &exec_lines, max_w));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rules_detail_renders_sections() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/options-rules.example.yaml");
        if !path.exists() {
            return;
        }
        let ctx = DashboardContext::load(&path).unwrap();
        let out = render_rules_detail(&ctx);
        assert!(out.contains("Schedule"));
        assert!(out.contains("Risk"));
        assert!(out.contains("LLM"));
    }

    #[test]
    fn rules_panel_borders_align() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/options-rules.example.yaml");
        if !path.exists() {
            return;
        }
        let ctx = DashboardContext::load(&path).unwrap();
        let out = render_rules_detail(&ctx);
        for block in out.split("\n\n") {
            if !block.contains('╭') {
                continue;
            }
            let mut border_width = None;
            for line in block.lines() {
                let plain = console::strip_ansi_codes(line);
                if plain.starts_with('╭') || plain.starts_with('╰') {
                    border_width = Some(plain.chars().count());
                }
                if plain.starts_with('│') {
                    let w = border_width.expect("border width");
                    assert_eq!(
                        plain.chars().count(),
                        w,
                        "panel line width mismatch: {plain}"
                    );
                }
            }
        }
    }
}
