use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{
    Block, BorderType, Borders, List, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Tabs,
};
use ratatui::Terminal;

use super::context::DashboardContext;
use super::tui_render::{
    activity_items, agent_status_lines, daemon_hint, header_line, latest_llm_lines,
    llm_history_lines, position_items, risk_gauge, rules_detail_lines, rules_summary_lines,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// How the watch TUI relates to the agent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchAgentMode {
    /// Agent loop runs in the same process (default when no daemon exists).
    Embedded,
    /// Attached to an existing background daemon.
    External,
    /// User passed `--monitor-only`; no agent started by watch.
    MonitorOnly,
}

#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub rules_path: PathBuf,
    pub agent_mode: WatchAgentMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WatchTab {
    Overview = 0,
    Rules = 1,
    Log = 2,
    Positions = 3,
    Llm = 4,
}

impl WatchTab {
    fn all() -> [WatchTab; 5] {
        [
            WatchTab::Overview,
            WatchTab::Rules,
            WatchTab::Log,
            WatchTab::Positions,
            WatchTab::Llm,
        ]
    }

    fn title(self) -> &'static str {
        match self {
            WatchTab::Overview => "Overview",
            WatchTab::Rules => "Rules",
            WatchTab::Log => "Log",
            WatchTab::Positions => "Positions",
            WatchTab::Llm => "LLM",
        }
    }

    fn next(self) -> Self {
        match self {
            WatchTab::Overview => WatchTab::Rules,
            WatchTab::Rules => WatchTab::Log,
            WatchTab::Log => WatchTab::Positions,
            WatchTab::Positions => WatchTab::Llm,
            WatchTab::Llm => WatchTab::Overview,
        }
    }
}

struct WatchState {
    rules_scroll: u16,
    log_scroll: u16,
    llm_scroll: u16,
}

pub fn run_watch_tui(config: &WatchConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let rules_path = &config.rules_path;
    let agent_mode = config.agent_mode;

    let mut tab = WatchTab::Overview;
    let mut ctx = DashboardContext::load(rules_path)?;
    let mut last_refresh = Instant::now();
    let mut status_msg = match agent_mode {
        WatchAgentMode::Embedded => "agent running in-process".to_string(),
        WatchAgentMode::External => format!(
            "attached to pid {}",
            ctx.daemon.pid.unwrap_or(0)
        ),
        WatchAgentMode::MonitorOnly => "monitor only".to_string(),
    };
    let mut state = WatchState {
        rules_scroll: 0,
        log_scroll: 0,
        llm_scroll: 0,
    };

    loop {
        terminal.draw(|f| {
            draw_ui(
                f,
                f.area(),
                &ctx,
                tab,
                &status_msg,
                &mut state,
                agent_mode,
            );
        })?;

        let timeout = REFRESH_INTERVAL.saturating_sub(last_refresh.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Tab => tab = tab.next(),
                        KeyCode::Char('1') => tab = WatchTab::Overview,
                        KeyCode::Char('2') => tab = WatchTab::Rules,
                        KeyCode::Char('3') => tab = WatchTab::Log,
                        KeyCode::Char('4') => tab = WatchTab::Positions,
                        KeyCode::Char('5') => tab = WatchTab::Llm,
                        KeyCode::Char('j') | KeyCode::Down => {
                            scroll_active_tab(tab, &mut state, 1)
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            scroll_active_tab(tab, &mut state, -1)
                        }
                        KeyCode::Char('r') => {
                            match DashboardContext::load(rules_path) {
                                Ok(c) => {
                                    ctx = c;
                                    last_refresh = Instant::now();
                                    status_msg = "refreshed".into();
                                }
                                Err(e) => status_msg = format!("refresh failed: {e:#}"),
                            }
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        _ => {}
                    }
                }
            }
        } else if last_refresh.elapsed() >= REFRESH_INTERVAL {
            if let Ok(c) = DashboardContext::load(rules_path) {
                ctx = c;
            }
            last_refresh = Instant::now();
        }
    }

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn scroll_active_tab(tab: WatchTab, state: &mut WatchState, delta: i16) {
    let scroll = match tab {
        WatchTab::Rules => &mut state.rules_scroll,
        WatchTab::Log => &mut state.log_scroll,
        WatchTab::Llm => &mut state.llm_scroll,
        _ => return,
    };
    if delta < 0 {
        *scroll = scroll.saturating_sub(delta.unsigned_abs());
    } else {
        *scroll = scroll.saturating_add(delta as u16);
    }
}

fn draw_ui(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &DashboardContext,
    tab: WatchTab,
    status_msg: &str,
    state: &mut WatchState,
    agent_mode: WatchAgentMode,
) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);

    f.render_widget(Paragraph::new(header_line(ctx, agent_mode)), outer[0]);

    let titles: Vec<Line> = WatchTab::all()
        .iter()
        .map(|t| Line::from(t.title()))
        .collect();
    let tabs = Tabs::new(titles)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .select(tab as usize);
    f.render_widget(tabs, outer[1]);

    match tab {
        WatchTab::Overview => render_overview(f, outer[2], ctx, agent_mode),
        WatchTab::Rules => render_rules_tab(f, outer[2], ctx, state),
        WatchTab::Log => render_log_tab(f, outer[2], ctx, state),
        WatchTab::Positions => render_positions_tab(f, outer[2], ctx),
        WatchTab::Llm => render_llm_tab(f, outer[2], ctx, state),
    }

    let footer = Line::from(vec![
        ratatui::text::Span::styled(" Tab/1-5 ", Style::default().fg(Color::DarkGray)),
        ratatui::text::Span::styled("j/k scroll ", Style::default().fg(Color::DarkGray)),
        ratatui::text::Span::styled("r refresh ", Style::default().fg(Color::DarkGray)),
        ratatui::text::Span::styled("q quit ", Style::default().fg(Color::DarkGray)),
        ratatui::text::Span::styled(status_msg, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(footer), outer[3]);
}

fn panel_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "))
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
}

fn render_overview(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &DashboardContext,
    agent_mode: WatchAgentMode,
) {
    let show_hint =
        matches!(agent_mode, WatchAgentMode::MonitorOnly) && !ctx.daemon.running;
    let main_constraints = if show_hint {
        vec![
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Min(4),
            Constraint::Length(3),
        ]
    } else {
        vec![
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Min(4),
        ]
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(main_constraints)
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);

    f.render_widget(
        Paragraph::new(agent_status_lines(ctx, agent_mode)).block(panel_block("Agent")),
        top[0],
    );

    let rules_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(top[1]);

    f.render_widget(
        Paragraph::new(rules_summary_lines(ctx)).block(panel_block("Rules")),
        rules_area[0],
    );
    f.render_widget(
        risk_gauge(ctx).block(Block::default().borders(Borders::NONE)),
        rules_area[1],
    );

    f.render_widget(
        Paragraph::new(latest_llm_lines(ctx)).block(panel_block("Last LLM")),
        rows[1],
    );

    let activity = List::new(activity_items(ctx)).block(panel_block("Recent Activity"));
    f.render_widget(activity, rows[2]);

    if show_hint {
        f.render_widget(Paragraph::new(daemon_hint(ctx)), rows[3]);
    }
}

fn render_rules_tab(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &DashboardContext,
    state: &mut WatchState,
) {
    let lines = rules_detail_lines(ctx);
    let line_count = lines.len() as u16;
    let scroll = state.rules_scroll.min(line_count.saturating_sub(1));

    let vertical = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(
                panel_block("Rules Config")
                    .title_bottom(format!(" {} ", ctx.rules_path.display())),
            ),
        vertical[0],
    );

    let mut sb = ScrollbarState::new(line_count as usize).position(scroll as usize);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓")),
        vertical[1],
        &mut sb,
    );
}

fn render_log_tab(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &DashboardContext,
    state: &mut WatchState,
) {
    let lines: Vec<Line> = if ctx.log_tail.is_empty() {
        vec![Line::from("(no log yet)")]
    } else {
        ctx.log_tail
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect()
    };
    let line_count = lines.len() as u16;
    let scroll = state.log_scroll.min(line_count.saturating_sub(1));

    let vertical = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    f.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(Color::DarkGray))
            .scroll((scroll, 0))
            .block(
                panel_block("Agent Log")
                    .title_bottom(format!(" {} ", short_path(&ctx.daemon.log_file))),
            ),
        vertical[0],
    );

    let mut sb = ScrollbarState::new(line_count as usize).position(scroll as usize);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight),
        vertical[1],
        &mut sb,
    );
}

fn render_positions_tab(f: &mut ratatui::Frame, area: Rect, ctx: &DashboardContext) {
    let title = format!("Positions ({})", ctx.state.open_positions.len());
    let list = List::new(position_items(ctx)).block(panel_block(&title));
    f.render_widget(list, area);
}

fn render_llm_tab(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &DashboardContext,
    state: &mut WatchState,
) {
    let lines = llm_history_lines(ctx);
    let line_count = lines.len() as u16;
    let scroll = state.llm_scroll.min(line_count.saturating_sub(1));

    let vertical = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let model = if ctx.rules.llm.enabled {
        format!(
            " {} / {} ",
            ctx.rules.llm.effective_monitor_model(),
            ctx.rules.llm.effective_selection_model()
        )
    } else {
        " disabled ".into()
    };

    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(
                panel_block("LLM Reviews")
                    .title_bottom(model),
            ),
        vertical[0],
    );

    let mut sb = ScrollbarState::new(line_count as usize).position(scroll as usize);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓")),
        vertical[1],
        &mut sb,
    );
}

fn short_path(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
