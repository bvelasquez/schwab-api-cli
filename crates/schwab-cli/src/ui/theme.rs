//! Shared ratatui styling for the options watch dashboard.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::DarkGray;
pub const PROFIT: Color = Color::Green;
pub const LOSS: Color = Color::Red;
pub const WARN: Color = Color::Yellow;

pub fn panel_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "))
        .title_style(
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(Color::Rgb(60, 66, 82)))
}

pub fn chrome_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "))
        .title_style(
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(Color::Rgb(45, 50, 64)))
}

pub fn footer_block() -> Block<'static> {
    Block::default()
        .borders(Borders::TOP)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(45, 50, 64)))
}

pub fn pnl_color(profit_pct: f64) -> Color {
    if profit_pct >= 25.0 {
        PROFIT
    } else if profit_pct <= -25.0 {
        LOSS
    } else if profit_pct >= 0.0 {
        Color::LightGreen
    } else {
        WARN
    }
}

pub fn gauge_color(ratio: f64) -> Color {
    if ratio > 0.8 {
        LOSS
    } else if ratio > 0.5 {
        WARN
    } else {
        PROFIT
    }
}

pub fn label_style() -> Style {
    Style::default().fg(MUTED)
}

pub fn value_style() -> Style {
    Style::default().fg(Color::White)
}

pub fn key_style() -> Style {
    Style::default()
        .fg(Color::Rgb(130, 170, 255))
        .add_modifier(Modifier::BOLD)
}
