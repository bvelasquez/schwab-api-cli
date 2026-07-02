pub mod agent_health;
pub mod context;
pub mod dashboard;
pub mod discover;
pub mod market_status;
pub mod menu;
pub mod rules_view;
pub mod spread_feed;
pub mod spread_live;
pub mod tui_render;
pub mod watch;

use console::Style;
use unicode_width::UnicodeWidthStr;

/// Visible terminal width (fallback 100).
pub fn terminal_width() -> usize {
    console::Term::stdout().size().1.max(80) as usize
}

/// Horizontal rule with centered title (furoshiki-style).
pub fn rule(title: &str) -> String {
    let width = terminal_width().min(120);
    let plain_len = UnicodeWidthStr::width(title);
    let pad = width.saturating_sub(plain_len + 4);
    let left = pad / 2;
    let right = pad - left;
    format!(
        "{}{}{}",
        "─".repeat(left),
        Style::new().bold().apply_to(format!(" {title} ")),
        "─".repeat(right)
    )
}

/// Unicode progress bar (█░).
pub fn bar(ratio: f64, width: usize) -> String {
    let ratio = ratio.clamp(0.0, 1.0);
    let filled = (ratio * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    format!(
        "{}{}",
        Style::new().green().apply_to("█".repeat(filled)),
        Style::new().dim().apply_to("░".repeat(empty))
    )
}

pub fn status_dot(running: bool) -> String {
    if running {
        Style::new().green().apply_to("●").to_string()
    } else {
        Style::new().red().apply_to("○").to_string()
    }
}

pub fn clock_dot() -> String {
    Style::new().dim().apply_to("◷").to_string()
}

/// Outer width of a panel that fits its content (capped at `max_width`).
pub fn panel_width_for(title: &str, lines: &[String], max_width: usize) -> usize {
    let title_plain = format!(" {title} ");
    let title_need = UnicodeWidthStr::width(title_plain.as_str()) + 2;
    let content_need = lines
        .iter()
        .map(|l| strip_ansi_width(l) + 4)
        .max()
        .unwrap_or(0);
    title_need.max(content_need).clamp(28, max_width)
}

/// Rounded panel with title; `width` is total outer width including corners.
pub fn panel(title: &str, lines: &[String], width: usize) -> String {
    let width = width.max(28);
    let border_inner = width.saturating_sub(2);
    let title_plain = format!(" {title} ");
    let title_w = UnicodeWidthStr::width(title_plain.as_str());
    let dash_total = border_inner.saturating_sub(title_w);
    let dash_left = dash_total / 2;
    let dash_right = dash_total - dash_left;

    let mut out = String::new();
    out.push('╭');
    out.push_str(&"─".repeat(dash_left));
    out.push_str(&Style::new().bold().apply_to(&title_plain).to_string());
    out.push_str(&"─".repeat(dash_right));
    out.push_str("╮\n");

    for line in lines {
        let visible = strip_ansi_width(line);
        let pad = width.saturating_sub(visible + 4);
        out.push('│');
        out.push(' ');
        out.push_str(line);
        out.push_str(&" ".repeat(pad));
        out.push(' ');
        out.push_str("│\n");
    }

    out.push('╰');
    out.push_str(&"─".repeat(border_inner));
    out.push('╯');
    out
}

/// Panel sized to content, not full terminal width.
pub fn panel_fit(title: &str, lines: &[String], max_width: usize) -> String {
    let width = panel_width_for(title, lines, max_width);
    panel(title, lines, width)
}

/// Key / value line with aligned columns inside a panel.
pub fn kv_line(key: &str, value: &str, key_width: usize) -> String {
    format!("  {key:key_width$}  {value}", key_width = key_width)
}

/// Two panels side by side (each column is one panel's line sequence).
pub fn two_column(left: String, right: String, total_width: usize) -> String {
    let gap = 2;
    let col_w = (total_width.saturating_sub(gap)) / 2;
    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();
    let rows = left_lines.len().max(right_lines.len());

    let mut out = String::new();
    for i in 0..rows {
        let l = left_lines.get(i).copied().unwrap_or("");
        let r = right_lines.get(i).copied().unwrap_or("");
        let l_vis = strip_ansi_width(l);
        let pad = col_w.saturating_sub(l_vis);
        out.push_str(l);
        out.push_str(&" ".repeat(pad));
        out.push_str(&" ".repeat(gap));
        out.push_str(r);
        out.push('\n');
    }
    out.trim_end().to_string()
}

pub fn ago_secs(secs: i64) -> String {
    if secs < 0 {
        return "just now".into();
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    if secs < 3600 {
        return format!("{}m ago", secs / 60);
    }
    if secs < 86_400 {
        return format!("{}h ago", secs / 3600);
    }
    format!("{}d ago", secs / 86_400)
}

pub fn format_duration_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn strip_ansi_width(s: &str) -> usize {
    let stripped = console::strip_ansi_codes(s);
    UnicodeWidthStr::width(stripped.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_clamps() {
        assert!(bar(1.5, 8).contains('█'));
        assert!(bar(0.0, 8).contains('░'));
    }

    #[test]
    fn panel_has_corners() {
        let p = panel("Test", &["line".into()], 40);
        assert!(p.starts_with('╭'));
        assert!(p.contains('╯'));
    }

    #[test]
    fn panel_lines_match_border_width() {
        let lines = vec!["  tick interval     2m (120)".into()];
        let p = panel_fit("Schedule", &lines, 120);
        let border_len = p.lines().next().unwrap().chars().count();
        for line in p.lines().skip(1) {
            if line.starts_with('│') {
                assert_eq!(
                    line.chars().count(),
                    border_len,
                    "mismatched line width: {line}"
                );
            }
        }
    }

    #[test]
    fn panel_fit_shrinks_to_content() {
        let lines = vec!["  short".into()];
        let w = panel_width_for("Accounts", &lines, 120);
        assert!(w < 80, "expected compact panel, got width {w}");
    }
}
