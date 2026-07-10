//! High-visibility markers for ratatui canvas charts (Braille dots are too small alone).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Context, Line as CanvasLine};

/// Crosshair arm lengths in chart data units.
pub fn marker_spans(x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> (f64, f64) {
    let x_span = (x_max - x_min).max(1.0);
    let y_span = (y_max - y_min).max(1.0);
    (x_span * 0.04, y_span * 0.08)
}

/// Bold glyph + crosshair — much easier to see than a single Braille pixel.
pub fn draw_chart_marker(
    ctx: &mut Context<'_>,
    x: f64,
    y: f64,
    hx: f64,
    hy: f64,
    color: Color,
    glyph: &'static str,
) {
    ctx.draw(&CanvasLine::new(x - hx, y, x + hx, y, color));
    ctx.draw(&CanvasLine::new(x, y - hy, x, y + hy, color));
    ctx.print(
        x,
        y,
        Line::from(Span::styled(
            glyph,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
    );
}
