use console::Style;
use serde_json::Value;

/// Human-readable body for agent tick / run-once envelopes (pretty output mode).
pub fn format_tick_data(data: &Value) -> String {
    let mut out = String::new();

    let agent_id = str_field(data, "agent_id").unwrap_or("agent");
    let dry_run = data.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);

    let header = Style::new().cyan().bold();
    let _label = Style::new().dim();
    let accent = Style::new().yellow();

    out.push_str(&format!("{}", header.apply_to(agent_id)));
    if dry_run {
        out.push_str(&format!("  {}", accent.apply_to("[dry-run]")));
    }
    if let Some(session) = str_field(data, "session") {
        let session_style = match session {
            "regular" => Style::new().green(),
            "overnight" => Style::new().magenta(),
            "idle" => Style::new().dim(),
            _ => Style::new().dim(),
        };
        out.push_str(&format!("  {}", session_style.apply_to(session)));
    }
    if data.get("at_open").and_then(|v| v.as_bool()).unwrap_or(false) {
        out.push_str(&format!("  {}", accent.apply_to("[at open]")));
    }
    out.push('\n');

    let signals = data.get("signals").and_then(|v| v.as_array());
    let actions = data.get("actions").and_then(|v| v.as_array());
    let skipped = data.get("skipped").and_then(|v| v.as_array());
    let monitored = data.get("monitored_positions").and_then(|v| v.as_array());

    out.push_str(&format_monitored_positions(monitored));

    out.push_str(&format_section("Signals", signals, format_signal));
    out.push_str(&format_section("Actions", actions, format_action));
    out.push_str(&format_skipped(skipped));

    if let Some(llm) = data.get("llm_review") {
        if !llm.is_null() {
            out.push('\n');
            out.push_str(&format_llm_review(llm));
        }
    }

    out.trim_end().to_string()
}

fn format_monitored_positions(monitored: Option<&Vec<Value>>) -> String {
    let Some(arr) = monitored else {
        return String::new();
    };
    if arr.is_empty() {
        return String::new();
    }

    let header = Style::new().bold();
    let hold = Style::new().green();
    let exit = Style::new().yellow();
    let mut out = format!(
        "\n{}\n",
        header.apply_to(format!(
            "Monitoring {} open position{}",
            arr.len(),
            if arr.len() == 1 { "" } else { "s" }
        ))
    );

    for pos in arr {
        out.push_str(&format!("  • {}\n", format_monitored_position(pos, &hold, &exit)));
    }
    out
}

fn format_monitored_position(pos: &Value, hold: &Style, exit: &Style) -> String {
    let underlying = str_field(pos, "underlying").unwrap_or("?");
    let strategy = str_field(pos, "strategy").unwrap_or("spread");
    let expiry = str_field(pos, "expiry").unwrap_or("?");
    let status = str_field(pos, "status").unwrap_or("holding");

    let credit = pos.get("entry_credit").and_then(|v| v.as_f64());
    let credit_s = credit
        .map(|c| format!("  cr ${c:.2}"))
        .unwrap_or_default();

    let profit = pos.get("profit_pct").and_then(|v| v.as_f64());
    let profit_s = profit
        .map(|p| format!("  P/L {p:+.0}%"))
        .unwrap_or_default();

    let dte = pos
        .get("dte")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)));
    let dte_s = dte.map(|d| format!("  {d} DTE")).unwrap_or_default();

    let greeks_s = pos
        .get("market_context")
        .map(format_monitor_greeks_suffix)
        .unwrap_or_default();

    let status_s = if status.starts_with("exit:") {
        exit.apply_to(status).to_string()
    } else {
        hold.apply_to(status).to_string()
    };

    format!(
        "{underlying} {strategy}  exp {expiry}{credit_s}{profit_s}{dte_s}{greeks_s}  — {status_s}"
    )
}

fn format_monitor_greeks_suffix(ctx: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(px) = ctx.get("underlying_price").and_then(|v| v.as_f64()) {
        parts.push(format!("@${px:.2}"));
    }
    if let (Some(short), Some(long)) = (
        ctx.get("short_strike").and_then(|v| v.as_f64()),
        ctx.get("long_strike").and_then(|v| v.as_f64()),
    ) {
        parts.push(format!("${short:.0}/${long:.0}"));
    }
    if let Some(delta) = ctx.get("short_delta").and_then(|v| v.as_f64()) {
        parts.push(format!("δ {delta:.2}"));
    }
    if let Some(otm) = ctx.get("short_otm_pct").and_then(|v| v.as_f64()) {
        parts.push(format!("{otm:.1}% OTM"));
    }
    if let Some(pop) = ctx
        .get("approx_short_otm_probability_pct")
        .and_then(|v| v.as_f64())
    {
        parts.push(format!("~{pop:.0}% OTM prob"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  ({})", parts.join(", "))
    }
}

pub fn format_status_data(data: &Value) -> String {
    let mut out = String::new();
    let label = Style::new().dim();

    if let Some(path) = str_field(data, "state_path") {
        out.push_str(&format!("  {} {}\n", label.apply_to("state file:"), path));
    }

    if let Some(state) = data.get("state") {
        for (key, fmt) in [
            ("agent_id", "agent"),
            ("tick_count", "ticks"),
            ("trades_today", "trades today"),
            ("open_positions", "open positions"),
            ("pending_orders", "pending orders"),
            ("last_tick", "last tick"),
            ("last_llm_summary", "last LLM"),
        ] {
            if let Some(v) = state.get(key) {
                if key == "last_llm_summary" {
                    if let Some(summary) = format_llm_summary_line(v) {
                        out.push_str(&format!("  {} {}\n", label.apply_to(fmt), summary));
                    }
                } else {
                    out.push_str(&format!(
                        "  {} {}\n",
                        label.apply_to(fmt),
                        json_scalar(v)
                    ));
                }
            }
        }
    }

    out.trim_end().to_string()
}

pub fn format_validate_data(data: &Value) -> String {
    let label = Style::new().dim();
    let ok = Style::new().green();
    let mut out = String::new();

    out.push_str(&format!(
        "  {} {}\n",
        label.apply_to("valid:"),
        ok.apply_to("yes")
    ));
    for (key, label_text) in [
        ("agent_id", "agent id"),
        ("accounts", "accounts"),
        ("watchlist", "watchlist"),
        ("llm_enabled", "LLM"),
        ("telegram_enabled", "telegram"),
    ] {
        if let Some(v) = data.get(key) {
            out.push_str(&format!(
                "  {} {}\n",
                label.apply_to(label_text),
                json_scalar(v)
            ));
        }
    }
    out.trim_end().to_string()
}

pub fn format_background_data(data: &Value) -> String {
    let label = Style::new().dim();
    let mut out = String::new();
    for (key, label_text) in [
        ("pid", "pid"),
        ("rules", "rules"),
        ("pid_file", "pid file"),
        ("log_file", "log file"),
    ] {
        if let Some(v) = data.get(key) {
            out.push_str(&format!(
                "  {} {}\n",
                label.apply_to(label_text),
                json_scalar(v)
            ));
        }
    }
    out.trim_end().to_string()
}

fn format_section<F>(title: &str, items: Option<&Vec<Value>>, formatter: F) -> String
where
    F: Fn(&Value) -> String,
{
    let Some(arr) = items else {
        return String::new();
    };
    if arr.is_empty() {
        return String::new();
    }
    let header = Style::new().bold();
    let mut out = format!("\n{}\n", header.apply_to(title));
    for item in arr {
        out.push_str(&format!("  • {}\n", formatter(item)));
    }
    out
}

fn format_skipped(skipped: Option<&Vec<Value>>) -> String {
    let Some(arr) = skipped else {
        return String::new();
    };
    if arr.is_empty() {
        return String::new();
    }
    let header = Style::new().bold();
    let mut out = format!("\n{}\n", header.apply_to("Skipped"));
    for item in arr {
        if let Some(s) = item.as_str() {
            out.push_str(&format!("  • {s}\n"));
        }
    }
    out
}

fn format_signal(signal: &Value) -> String {
    let kind = str_field(signal, "type").unwrap_or("signal");
    match kind {
        "entry" => format_entry_signal(signal),
        "exit" => {
            let underlying = str_field(signal, "underlying").unwrap_or("?");
            let reason = str_field(signal, "reason").unwrap_or("exit");
            let pos = str_field(signal, "position_id").unwrap_or("");
            let mark = signal.get("mark").and_then(|v| v.as_f64());
            let mark_s = mark
                .map(|m| format!("  mark ${m:.2}"))
                .unwrap_or_default();
            format!("EXIT  {underlying}  {reason}  {pos}{mark_s}")
        }
        _ => compact_json(signal),
    }
}

fn format_entry_signal(signal: &Value) -> String {
    let strategy = str_field(signal, "strategy").unwrap_or("spread");
    let params = signal.get("params");
    let underlying = params
        .and_then(|p| str_field(p, "underlying"))
        .or_else(|| str_field(signal, "underlying"))
        .unwrap_or("?");
    let expiry = params
        .and_then(|p| str_field(p, "expiry"))
        .unwrap_or("");

    let strikes = match strategy {
        "vertical" => {
            let spread_type = params
                .and_then(|p| str_field(p, "type"))
                .unwrap_or("spread");
            let short = params.and_then(|p| p.get("short_strike")).and_then(|v| v.as_f64());
            let long = params.and_then(|p| p.get("long_strike")).and_then(|v| v.as_f64());
            match (short, long) {
                (Some(s), Some(l)) => format!("{spread_type}  ${s:.0}/${l:.0}"),
                _ => spread_type.to_string(),
            }
        }
        "iron_condor" => {
            let ps = params.and_then(|p| p.get("put_short")).and_then(|v| v.as_f64());
            let pl = params.and_then(|p| p.get("put_long")).and_then(|v| v.as_f64());
            let cs = params.and_then(|p| p.get("call_short")).and_then(|v| v.as_f64());
            let cl = params.and_then(|p| p.get("call_long")).and_then(|v| v.as_f64());
            match (ps, pl, cs, cl) {
                (Some(ps), Some(pl), Some(cs), Some(cl)) => {
                    format!("iron condor  puts ${ps:.0}/${pl:.0}  calls ${cs:.0}/${cl:.0}")
                }
                _ => "iron condor".to_string(),
            }
        }
        _ => strategy.to_string(),
    };

    let credit = signal
        .get("estimated_credit")
        .or_else(|| params.and_then(|p| p.get("limit_credit")))
        .and_then(|v| v.as_f64());
    let credit_s = credit
        .map(|c| format!("  ~${c:.2} cr"))
        .unwrap_or_default();

    let ctx = signal.get("market_context");
    let ctx_s = ctx.map(format_market_context_suffix).unwrap_or_default();

    format!(
        "ENTRY  {underlying}  {strikes}  exp {expiry}{credit_s}{ctx_s}"
    )
}

fn format_market_context_suffix(ctx: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(dte) = ctx.get("dte").and_then(|v| v.as_u64()) {
        parts.push(format!("{dte} DTE"));
    }
    if let Some(delta) = ctx.get("short_delta").and_then(|v| v.as_f64()) {
        parts.push(format!("δ {delta:.2}"));
    }
    if let Some(pct) = ctx.get("credit_to_width_pct").and_then(|v| v.as_f64()) {
        parts.push(format!("{pct:.0}% cr/width"));
    }
    if let Some(loss) = ctx.get("max_loss_total_usd").and_then(|v| v.as_f64()) {
        parts.push(format!("max loss ${loss:.0}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  ({})", parts.join(", "))
    }
}

fn format_action(action: &Value) -> String {
    if let Some(fill) = str_field(action, "fill_status") {
        return format_order_action(action, &fill);
    }
    if action.get("exit").is_some() {
        let underlying = action
            .pointer("/signal/underlying")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let reason = action
            .pointer("/signal/reason")
            .and_then(|v| v.as_str())
            .unwrap_or("exit");
        return format!("EXIT filled  {underlying}  ({reason})");
    }
    if let Some(kind) = str_field(action, "kind") {
        return format!("{kind}  {}", compact_json(action));
    }
    compact_json(action)
}

fn format_order_action(action: &Value, fill_status: &str) -> String {
    let underlying = action
        .pointer("/signal/params/underlying")
        .or_else(|| action.pointer("/signal/underlying"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let strategy = action
        .pointer("/signal/strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("spread");

    let order_id = action
        .pointer("/entry/orderId")
        .or_else(|| action.pointer("/place/orderId"))
        .and_then(|v| v.as_u64())
        .map(|id| format!("  order #{id}"))
        .unwrap_or_default();

    let status = match fill_status {
        "FILLED" => Style::new().green().apply_to("FILLED").to_string(),
        "REJECTED" | "CANCELED" | "EXPIRED" => {
            Style::new().red().apply_to(fill_status).to_string()
        }
        _ => Style::new().yellow().apply_to(fill_status).to_string(),
    };

    let note = action
        .get("note")
        .and_then(|v| v.as_str())
        .map(|n| format!("  — {n}"))
        .unwrap_or_default();

    format!("ENTRY {status}  {underlying} {strategy}{order_id}{note}")
}

fn format_llm_review(llm: &Value) -> String {
    let header = Style::new().bold();
    let label = Style::new().dim();
    let phase = str_field(llm, "phase").unwrap_or("review");
    let phase_label = match phase {
        "overnight_digest" => "overnight digest",
        other => other,
    };
    let model = str_field(llm, "model").unwrap_or("model");
    let web = llm.get("used_web").and_then(|v| v.as_bool()).unwrap_or(false);
    let web_tag = if web { " + web" } else { "" };

    let mut out = format!(
        "{}\n",
        header.apply_to(format!("LLM {phase_label} ({model}{web_tag})"))
    );

    if let Some(rec) = llm.pointer("/new_entries/recommendation").and_then(|v| v.as_str()) {
        let rec_style = match rec {
            "proceed" => Style::new().green(),
            "defer" | "skip" | "hold" => Style::new().yellow(),
            _ => Style::new().cyan(),
        };
        out.push_str(&format!(
            "  {} {}\n",
            label.apply_to("entries:"),
            rec_style.apply_to(rec)
        ));
    }

    if let Some(reason) = llm
        .pointer("/new_entries/reasoning")
        .and_then(|v| v.as_str())
    {
        if !reason.is_empty() {
            out.push_str(&format!("  {}\n", wrap_text(reason, 76, "  ")));
        }
    }

    if let Some(commentary) = llm.get("market_commentary").and_then(|v| v.as_str()) {
        if !commentary.is_empty() {
            out.push_str(&format!(
                "  {}\n",
                label.apply_to("market:")
            ));
            out.push_str(&format!("  {}\n", wrap_text(commentary, 76, "  ")));
        }
    }

    if let Some(alerts) = llm.get("risk_alerts").and_then(|v| v.as_array()) {
        if !alerts.is_empty() {
            out.push_str(&format!("  {}\n", label.apply_to("alerts:")));
            for alert in alerts {
                if let Some(s) = alert.as_str() {
                    out.push_str(&format!("    • {s}\n"));
                }
            }
        }
    }

    if let Some(positions) = llm.get("positions").and_then(|v| v.as_array()) {
        if !positions.is_empty() {
            out.push_str(&format!("  {}\n", label.apply_to("positions:")));
            for pos in positions {
                let id = pos.get("position_id").and_then(|v| v.as_str()).unwrap_or("?");
                let rec = pos
                    .get("recommendation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hold");
                out.push_str(&format!("    • {id}: {rec}\n"));
            }
        }
    }

    out.trim_end().to_string()
}

fn format_llm_summary_line(v: &Value) -> Option<String> {
    if v.is_null() {
        return None;
    }
    let phase = str_field(v, "phase").unwrap_or("?");
    let rec = v
        .pointer("/new_entries/recommendation")
        .and_then(|r| r.as_str())
        .unwrap_or("—");
    Some(format!("{phase} → entries {rec}"))
}

fn wrap_text(text: &str, width: usize, indent: &str) -> String {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
        .into_iter()
        .map(|l| format!("{indent}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

fn json_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a
            .iter()
            .filter_map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        _ => v.to_string(),
    }
}

fn compact_json(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_skipped_only_tick() {
        let data = json!({
            "agent_id": "options-pilot-8709",
            "dry_run": true,
            "signals": [],
            "actions": [],
            "monitored_positions": [{
                "underlying": "IWM",
                "strategy": "vertical",
                "expiry": "2026-07-31",
                "entry_credit": 0.25,
                "profit_pct": 8.0,
                "dte": 36,
                "status": "holding"
            }],
            "skipped": ["new entries paused — max_trades_per_day (1) reached"],
            "llm_review": null
        });
        let out = format_tick_data(&data);
        assert!(out.contains("options-pilot-8709"));
        assert!(out.contains("Monitoring 1 open position"));
        assert!(out.contains("IWM"));
        assert!(out.contains("max_trades_per_day"));
        assert!(!out.contains("\"signals\""));
    }

    #[test]
    fn format_entry_signal_with_context() {
        let data = json!({
            "agent_id": "options-pilot-8709",
            "dry_run": false,
            "monitored_positions": [],
            "signals": [{
                "type": "entry",
                "strategy": "vertical",
                "params": {
                    "underlying": "IWM",
                    "expiry": "2025-07-18",
                    "type": "put_credit",
                    "short_strike": 282.0,
                    "long_strike": 280.0
                },
                "estimated_credit": 0.26,
                "market_context": {
                    "dte": 36,
                    "short_delta": -0.18,
                    "credit_to_width_pct": 13.0,
                    "max_loss_total_usd": 174.0
                }
            }],
            "actions": [],
            "skipped": [],
            "llm_review": {
                "phase": "selection",
                "model": "anthropic/claude-sonnet-4",
                "used_web": false,
                "new_entries": { "recommendation": "proceed", "reasoning": "Premium acceptable." },
                "market_commentary": "",
                "risk_alerts": []
            }
        });
        let out = format_tick_data(&data);
        assert!(out.contains("ENTRY  IWM"));
        assert!(out.contains("put_credit"));
        assert!(out.contains("LLM selection"));
        assert!(out.contains("proceed"));
    }
}
