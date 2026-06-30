//! Human-readable journal rendering for the watch TUI.

use chrono::DateTime;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

const SEPARATOR: &str = "────────────────────────────────────────";

pub fn format_journal_events(events: &[Value]) -> Vec<Line<'static>> {
    if events.is_empty() {
        return vec![Line::from("(journal empty)")];
    }

    let mut lines = Vec::new();
    for (i, event) in events.iter().enumerate() {
        if i > 0 {
            lines.push(separator_line());
        }
        lines.extend(format_one_event(event));
    }
    lines
}

fn separator_line() -> Line<'static> {
    Line::from(Span::styled(
        SEPARATOR,
        Style::default().fg(Color::DarkGray),
    ))
}

fn format_one_event(event: &Value) -> Vec<Line<'static>> {
    let event_type = event
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("event");
    let ts = event
        .get("ts")
        .and_then(|v| v.as_str())
        .map(short_ts)
        .unwrap_or_else(|| "—".into());
    let payload = event.get("payload").cloned().unwrap_or(Value::Null);

    let (label, style) = event_style(event_type);
    let mut lines = vec![Line::from(vec![
        Span::styled(label, style.add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {ts}"), Style::default().fg(Color::DarkGray)),
    ])];

    lines.extend(match event_type {
        "sim_tick_summary" => format_tick_summary(&payload),
        "sim_entry_filled" => format_entry_filled(&payload, "SIM ENTRY"),
        "entry_signal" => format_entry_filled(&payload, "entry signal"),
        "sim_exit_filled" => format_exit_filled(&payload),
        "profile_changed" => format_profile_changed(&payload),
        "sim_trailing_stop_updated" => format_trailing_stop(&payload),
        "rule_auto_applied" | "rule_patch_proposed" => format_rule_patch(event_type, &payload),
        "sizing_config_hint" => format_sizing_hint(&payload),
        "monitor_exit_adjusted" => format_monitor_adjusted(&payload),
        other => format_generic(other, &payload),
    });

    lines
}

fn event_style(event_type: &str) -> (&'static str, Style) {
    match event_type {
        "sim_entry_filled" => ("SIM ENTRY", Style::default().fg(Color::Green)),
        "sim_exit_filled" => ("SIM EXIT", Style::default().fg(Color::LightRed)),
        "entry_signal" => ("ENTRY SIGNAL", Style::default().fg(Color::Cyan)),
        "sim_tick_summary" => ("tick", Style::default().fg(Color::Magenta)),
        "profile_changed" => ("PROFILE", Style::default().fg(Color::Yellow)),
        "rule_auto_applied" => ("RULE APPLIED", Style::default().fg(Color::Blue)),
        "rule_patch_proposed" => ("RULE PROPOSED", Style::default().fg(Color::Blue)),
        "sizing_config_hint" => ("SIZING HINT", Style::default().fg(Color::Yellow)),
        "sim_trailing_stop_updated" => ("TRAILING STOP", Style::default().fg(Color::LightCyan)),
        "monitor_exit_adjusted" => ("MONITOR ADJUST", Style::default().fg(Color::LightYellow)),
        _ => ("EVENT", Style::default().fg(Color::Gray)),
    }
}

fn short_ts(raw: &str) -> String {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|_| raw.chars().take(19).collect())
}

fn format_tick_summary(p: &Value) -> Vec<Line<'static>> {
    let tick = field_u64(p, "tick");
    let profile = field_str(p, "active_profile");
    let scan = p.get("scan");
    let cands = scan
        .and_then(|s| s.get("candidates"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let rejected = scan
        .and_then(|s| s.get("rejected"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let attempts = p
        .get("entry_attempts")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let open = p
        .pointer("/monitoring/open_positions")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            p.get("open_positions")
                .and_then(|v| v.as_object())
                .map(|o| o.len() as u64)
        });
    let llm = p
        .get("llm")
        .and_then(|l| l.get("entry_recommendation"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            p.get("llm_phase")
                .and_then(|v| v.as_str())
                .map(|phase| if phase == "monitor" { "monitor" } else { "—" })
        })
        .unwrap_or("—");
    let entry_block = p
        .get("entry_block_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let limits = p.get("entry_limits");
    let trades_cap = limits
        .and_then(|l| l.get("trades_today"))
        .and_then(|v| v.as_u64())
        .map(|t| {
            let max = limits
                .and_then(|l| l.get("max_new_entries_per_day"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!("{t}/{max} entries today")
        })
        .unwrap_or_default();
    let dd = p
        .pointer("/drawdown/drawdown_pct")
        .and_then(|v| v.as_f64())
        .map(|d| format!("{d:.2}%"))
        .unwrap_or_else(|| "—".into());
    let equity = p
        .pointer("/drawdown/current_equity_usd")
        .and_then(|v| v.as_f64())
        .map(|e| format!("${e:.0}"))
        .unwrap_or_else(|| "—".into());
    let exits = p
        .get("closure_exits")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let mut lines = vec![Line::from(format!(
        "tick {tick} │ {profile} │ {cands} cand / {rejected} rej │ {attempts} attempts │ equity {equity}"
    ))];
    if !trades_cap.is_empty() || !entry_block.is_empty() {
        lines.push(Line::from(vec![
            Span::raw(format!("{trades_cap}  ")),
            if entry_block.is_empty() {
                Span::styled("entries OK", Style::default().fg(Color::Green))
            } else {
                Span::styled(entry_block.to_string(), Style::default().fg(Color::Yellow))
            },
        ]));
    }
    lines.push(Line::from(format!(
        "llm: {llm} │ drawdown {dd} │ open {open:?} │ exits this tick {exits}"
    )));

    if let Some(attempts) = p.get("entry_attempts").and_then(|v| v.as_array()) {
        for a in attempts.iter().take(4) {
            let status = a.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let sym = a
                .get("symbol")
                .and_then(|v| v.as_str())
                .or_else(|| a.pointer("/attempt/symbol").and_then(|v| v.as_str()))
                .unwrap_or("?");
            let reason = a
                .get("reason")
                .and_then(|v| v.as_str())
                .or_else(|| a.pointer("/attempt/reason").and_then(|v| v.as_str()))
                .unwrap_or("");
            let style = match status {
                "simulated" | "filled" => Style::default().fg(Color::Green),
                "skipped" => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::Gray),
            };
            let detail = if reason.is_empty() {
                format!("  {status}: {sym}")
            } else {
                format!("  {status}: {sym} — {reason}")
            };
            lines.push(Line::from(Span::styled(detail, style)));
        }
        if attempts.len() > 4 {
            lines.push(Line::from(format!(
                "  … +{} more attempts",
                attempts.len() - 4
            )));
        }
    }

    lines
}

fn format_entry_filled(p: &Value, _label: &str) -> Vec<Line<'static>> {
    let sym = field_str(p, "symbol");
    let qty = opt_f64(p, "quantity").unwrap_or(0.0);
    let px = opt_f64(p, "fill_price")
        .or_else(|| opt_f64(p, "limit_price"))
        .or_else(|| opt_f64(p, "entry_price"))
        .unwrap_or(0.0);
    let stop = opt_f64(p, "stop_price")
        .or_else(|| p.pointer("/bracket_preview/stop_price").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    let target = opt_f64(p, "profit_limit")
        .or_else(|| p.pointer("/bracket_preview/profit_limit").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    let profile = field_str(p, "active_profile");

    vec![
        Line::from(vec![
            Span::styled(sym.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "  x{qty:.2} @ ${px:.2}  │  stop ${stop:.2}  target ${target:.2}"
            )),
        ]),
        Line::from(format!("profile: {profile}")),
    ]
}

fn format_exit_filled(p: &Value) -> Vec<Line<'static>> {
    let sym = field_str(p, "symbol");
    let reason = field_str(p, "exit_reason");
    let pnl = opt_f64(p, "pnl_usd").unwrap_or(0.0);
    let pnl_pct = opt_f64(p, "pnl_pct").unwrap_or(0.0);
    let style = if pnl >= 0.0 {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };
    vec![
        Line::from(format!("{sym} closed — {reason}")),
        Line::from(Span::styled(
            format!("P&L ${pnl:+.2} ({pnl_pct:+.1}%)"),
            style,
        )),
    ]
}

fn format_profile_changed(p: &Value) -> Vec<Line<'static>> {
    let from = p.get("from").cloned().unwrap_or(Value::Null);
    let to = field_str(p, "to");
    let to = if to.is_empty() {
        p.get("active_profile")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        to
    };
    let from_s = match from {
        Value::String(s) => s,
        Value::Null => "—".into(),
        other => other.to_string(),
    };
    let reason = field_str(p, "reason");
    let regime = p
        .pointer("/regime/class")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut lines = vec![Line::from(format!("{from_s} → {to}  ({regime})"))];
    if !reason.is_empty() {
        lines.extend(wrap_text(&reason, 72));
    }
    lines
}

fn format_trailing_stop(p: &Value) -> Vec<Line<'static>> {
    let sym = field_str(p, "symbol");
    let old = opt_f64(p, "old_stop").unwrap_or(0.0);
    let new = opt_f64(p, "new_stop").unwrap_or(0.0);
    vec![Line::from(format!("{sym} stop ${old:.2} → ${new:.2}"))]
}

fn format_rule_patch(event_type: &str, p: &Value) -> Vec<Line<'static>> {
    if let Some(arr) = p.get("patches").and_then(|v| v.as_array()) {
        let mut lines = Vec::new();
        for patch in arr.iter().take(5) {
            if let Some(s) = patch.as_str() {
                lines.extend(wrap_text(&format!("• {s}"), 72));
            } else if let Some(path) = patch.get("path").and_then(|v| v.as_str()) {
                let val = patch.get("value").map(|v| v.to_string()).unwrap_or_default();
                lines.push(Line::from(format!("  {path} = {val}")));
            }
        }
        if !lines.is_empty() {
            return lines;
        }
    }
    if event_type == "rule_auto_applied" {
        if let Some(applied) = p.get("applied").and_then(|v| v.as_array()) {
            return applied
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| Line::from(format!("  • {s}")))
                .collect();
        }
    }
    format_generic(event_type, p)
}

fn format_sizing_hint(p: &Value) -> Vec<Line<'static>> {
    let msg = field_str(p, "message");
    let streak = p.get("streak").and_then(|v| v.as_u64());
    let mut lines = wrap_text(&msg, 72);
    if let Some(s) = streak {
        lines.push(Line::from(format!("binding streak: {s} ticks")));
    }
    lines
}

fn format_monitor_adjusted(p: &Value) -> Vec<Line<'static>> {
    let sym = field_str(p, "symbol");
    let action = field_str(p, "action");
    let old = opt_f64(p, "old_stop").unwrap_or(0.0);
    let new = opt_f64(p, "new_stop").unwrap_or(0.0);
    vec![Line::from(format!("{sym} {action}: stop ${old:.2} → ${new:.2}"))]
}

fn format_generic(event_type: &str, p: &Value) -> Vec<Line<'static>> {
    match serde_json::to_string_pretty(p) {
        Ok(text) => text
            .lines()
            .take(12)
            .map(|l| Line::from(l.to_string()))
            .chain(if text.lines().count() > 12 {
                Some(Line::from("  …"))
            } else {
                None
            })
            .collect(),
        Err(_) => vec![Line::from(format!("{event_type}: {p}"))],
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(Line::from(current.clone()));
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn field_str(p: &Value, key: &str) -> String {
    p.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_f64(p: &Value, key: &str) -> Option<f64> {
    p.get(key).and_then(|v| v.as_f64())
}

fn field_u64(p: &Value, key: &str) -> u64 {
    p.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tick_summary_is_compact() {
        let event = json!({
            "ts": "2026-06-30T14:30:50Z",
            "type": "sim_tick_summary",
            "payload": {
                "tick": 42,
                "active_profile": "elevated_vol",
                "scan": { "candidates": [{"symbol":"A"}], "rejected": [] },
                "entry_attempts": [{"status":"skipped","symbol":"X","reason":"rsi"}],
                "llm": { "entry_recommendation": "defer" },
                "drawdown": { "drawdown_pct": 0.5, "current_equity_usd": 4000.0 }
            }
        });
        let lines = format_one_event(&event);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("tick 42"));
        assert!(text.contains("elevated_vol"));
        assert!(!text.contains("effective_playbook"));
    }

    #[test]
    fn separators_between_events() {
        let events = vec![
            json!({"ts":"2026-01-01T12:00:00Z","type":"profile_changed","payload":{"from":"a","to":"b","reason":"test"}}),
            json!({"ts":"2026-01-01T12:01:00Z","type":"sim_entry_filled","payload":{"symbol":"AMD","quantity":1.0,"fill_price":100.0,"stop_price":95.0,"profit_limit":110.0}}),
        ];
        let lines = format_journal_events(&events);
        let joined = lines
            .iter()
            .map(|l| l.spans.first().map(|s| s.content.as_ref()).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("|");
        assert!(joined.contains(SEPARATOR));
    }
}
