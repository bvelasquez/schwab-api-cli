use std::io;
use std::path::Path;
use std::sync::{Arc, RwLock};
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
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs, Wrap};
use ratatui::Terminal;

use crate::ui::context::{header_line, WatchContext};
use crate::ui::health::SharedAgentHealth;
use crate::ui::live::{list_position_monitors, WatchLiveSnapshot};
use crate::ui::positions_panel::{positions_content_height, render_positions_panel, CARD_HEIGHT};
use crate::ui::render::{
    candidate_lines, capital_lines, entry_attempt_lines, journal_lines, llm_lines, log_lines,
    market_conditions_panel_lines, overview_agent_lines, position_lines,
    position_rules_context_lines, rules_summary,
};
use schwab_cli::market_conditions::MarketConditionsSnapshot;
use schwab_cli::ui::theme::{self, footer_block, key_style};

const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchAgentMode {
    Embedded,
    MonitorOnly,
}

#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub rules_path: std::path::PathBuf,
    pub agent_mode: WatchAgentMode,
    pub dry_run: bool,
    pub simulate: bool,
    pub agent_health: Option<SharedAgentHealth>,
    pub live: Arc<RwLock<WatchLiveSnapshot>>,
    pub market_conditions: Arc<std::sync::Mutex<MarketConditionsSnapshot>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WatchTab {
    Overview = 0,
    Positions = 1,
    Candidates = 2,
    Capital = 3,
    Journal = 4,
    Llm = 5,
}

impl WatchTab {
    fn all() -> [WatchTab; 6] {
        [
            WatchTab::Overview,
            WatchTab::Positions,
            WatchTab::Candidates,
            WatchTab::Capital,
            WatchTab::Journal,
            WatchTab::Llm,
        ]
    }

    fn title(self) -> &'static str {
        match self {
            WatchTab::Overview => "Overview",
            WatchTab::Positions => "Positions",
            WatchTab::Candidates => "Candidates",
            WatchTab::Capital => "Capital",
            WatchTab::Journal => "Journal",
            WatchTab::Llm => "LLM",
        }
    }

    fn next(self) -> Self {
        match self {
            WatchTab::Overview => WatchTab::Positions,
            WatchTab::Positions => WatchTab::Candidates,
            WatchTab::Candidates => WatchTab::Capital,
            WatchTab::Capital => WatchTab::Journal,
            WatchTab::Journal => WatchTab::Llm,
            WatchTab::Llm => WatchTab::Overview,
        }
    }
}

struct ScrollState {
    scroll: u16,
}

struct WatchUiState {
    journal_scroll: ScrollState,
    log_scroll: ScrollState,
    llm_scroll: ScrollState,
    positions_scroll: ScrollState,
}

pub fn run_watch_tui(config: &WatchConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut tab = WatchTab::Overview;
    let mut ctx = load_watch_context(&config.rules_path, &config.live)?;
    let mut last_refresh = Instant::now();
    let agent_mode_str = match config.agent_mode {
        WatchAgentMode::Embedded => "embedded agent",
        WatchAgentMode::MonitorOnly => "monitor only",
    };
    let mut status_msg = agent_mode_str.to_string();
    let mut scroll = WatchUiState {
        journal_scroll: ScrollState { scroll: 0 },
        log_scroll: ScrollState { scroll: 0 },
        llm_scroll: ScrollState { scroll: 0 },
        positions_scroll: ScrollState { scroll: 0 },
    };

    loop {
        let health = config
            .agent_health
            .as_ref()
            .and_then(|h| h.lock().ok())
            .map(|g| g.clone())
            .unwrap_or_default();

        terminal.draw(|f| {
            draw_ui(
                f,
                f.area(),
                &ctx,
                tab,
                &status_msg,
                &mut scroll,
                agent_mode_str,
                config.dry_run,
                config.simulate,
                &health,
                &config.market_conditions,
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
                        KeyCode::Char('2') => tab = WatchTab::Positions,
                        KeyCode::Char('3') => tab = WatchTab::Candidates,
                        KeyCode::Char('4') => tab = WatchTab::Capital,
                        KeyCode::Char('5') => tab = WatchTab::Journal,
                        KeyCode::Char('6') => tab = WatchTab::Llm,
                        KeyCode::Char('j') | KeyCode::Down => {
                            scroll_tab(tab, &mut scroll, 1, &ctx)
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            scroll_tab(tab, &mut scroll, -1, &ctx)
                        }
                        KeyCode::Char('r') => match load_watch_context(&config.rules_path, &config.live) {
                            Ok(c) => {
                                ctx = c;
                                last_refresh = Instant::now();
                                status_msg = "refreshed".into();
                            }
                            Err(e) => status_msg = format!("refresh failed: {e:#}"),
                        },
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        _ => {}
                    }
                }
            }
        } else if last_refresh.elapsed() >= REFRESH_INTERVAL {
            if let Ok(c) = load_watch_context(&config.rules_path, &config.live) {
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

fn load_watch_context(
    rules_path: &Path,
    live: &Arc<RwLock<WatchLiveSnapshot>>,
) -> Result<WatchContext> {
    let snapshot = live.read().ok().map(|g| g.clone());
    WatchContext::load_with_live(rules_path, snapshot)
}

fn scroll_tab(tab: WatchTab, state: &mut WatchUiState, delta: i16, ctx: &WatchContext) {
    if tab == WatchTab::Positions {
        let monitors = list_position_monitors(
            &ctx.rules,
            &ctx.state,
            ctx.live.as_ref(),
            chrono::Utc::now(),
        );
        let max = positions_content_height(&monitors).saturating_sub(CARD_HEIGHT);
        let s = &mut state.positions_scroll.scroll;
        if delta < 0 {
            *s = s.saturating_sub(delta.unsigned_abs());
        } else {
            *s = (*s + delta as u16).min(max);
        }
        return;
    }

    let scroll = match tab {
        WatchTab::Journal => &mut state.journal_scroll.scroll,
        WatchTab::Llm => &mut state.llm_scroll.scroll,
        WatchTab::Overview => &mut state.log_scroll.scroll,
        _ => return,
    };
    if delta < 0 {
        *scroll = scroll.saturating_sub(delta.unsigned_abs());
    } else {
        *scroll = scroll.saturating_add(delta as u16);
    }
}

fn wrap_paragraph<'a>(content: impl Into<ratatui::text::Text<'a>>) -> Paragraph<'a> {
    Paragraph::new(content).wrap(Wrap { trim: true })
}

fn panel_block(title: &str) -> Block<'_> {
    theme::panel_block(title)
}

fn draw_ui(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &WatchContext,
    tab: WatchTab,
    status_msg: &str,
    scroll: &mut WatchUiState,
    agent_mode: &str,
    dry_run: bool,
    simulate: bool,
    health: &crate::ui::health::AgentHealth,
    market_conditions: &Arc<std::sync::Mutex<MarketConditionsSnapshot>>,
) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);

    f.render_widget(
        wrap_paragraph(header_line(ctx, agent_mode, dry_run, simulate))
            .block(theme::chrome_block("Schwab Trader")),
        outer[0],
    );

    let titles: Vec<Line> = WatchTab::all()
        .iter()
        .enumerate()
        .map(|(i, t)| {
            Line::from(vec![
                Span::styled(format!(" {} ", i + 1), Style::default().fg(theme::MUTED)),
                Span::raw(t.title()),
            ])
        })
        .collect();
    let tabs = Tabs::new(titles)
        .style(Style::default().fg(theme::MUTED))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│")
        .select(tab as usize)
        .padding(" ", " ");
    f.render_widget(tabs.block(footer_block()), outer[1]);

    let content = outer[2];
    match tab {
        WatchTab::Overview => render_overview(
            f,
            content,
            ctx,
            agent_mode,
            dry_run,
            simulate,
            health,
            scroll,
            market_conditions,
        ),
        WatchTab::Positions => render_positions_tab(f, content, ctx, scroll),
        WatchTab::Candidates => {
            f.render_widget(
                wrap_paragraph(candidate_lines(ctx)).block(panel_block("Scan / Candidates")),
                content,
            );
        }
        WatchTab::Capital => {
            f.render_widget(
                wrap_paragraph(capital_lines(ctx)).block(panel_block("Capital Ledger")),
                content,
            );
        }
        WatchTab::Journal => {
            let bottom = ctx.journal_file().display().to_string();
            render_scroll(
                f,
                content,
                journal_lines(ctx),
                scroll.journal_scroll.scroll,
                "Journal",
                bottom,
            );
        }
        WatchTab::Llm => {
            render_scroll(
                f,
                content,
                llm_lines(ctx),
                scroll.llm_scroll.scroll,
                "LLM Review",
                String::new(),
            );
        }
    }

    let footer = Line::from(vec![
        Span::styled(" Tab ", key_style()),
        Span::styled("/1-6 switch  ", theme::label_style()),
        Span::styled("j/k ", key_style()),
        Span::styled("scroll  ", theme::label_style()),
        Span::styled("r ", key_style()),
        Span::styled("refresh  ", theme::label_style()),
        Span::styled("q ", key_style()),
        Span::styled("quit  ", theme::label_style()),
        Span::styled("│ ", theme::label_style()),
        Span::styled(live_quote_footer(ctx), theme::label_style()),
        Span::raw("  "),
        Span::styled(status_msg, theme::label_style()),
    ]);
    f.render_widget(wrap_paragraph(footer).block(footer_block()), outer[3]);
}

fn live_quote_footer(ctx: &WatchContext) -> String {
    if let Some(live) = &ctx.live {
        if let Some(at) = live.last_fetch {
            let age = (chrono::Utc::now() - at).num_seconds();
            return format!("quotes {}s ago", age.max(0));
        }
        if let Some(err) = &live.last_error {
            let short = if err.len() > 40 {
                format!("{}…", &err[..37])
            } else {
                err.clone()
            };
            return format!("quotes: {short}");
        }
    }
    String::new()
}

fn render_positions_tab(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &WatchContext,
    scroll: &WatchUiState,
) {
    let monitors = list_position_monitors(
        &ctx.rules,
        &ctx.state,
        ctx.live.as_ref(),
        chrono::Utc::now(),
    );
    let count = monitors.len();
    let title = format!("Live Positions ({count})");

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(8)])
        .split(area);

    f.render_widget(
        wrap_paragraph(position_rules_context_lines(ctx))
            .block(panel_block("Regime / Rules")),
        outer[0],
    );

    let vertical = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(outer[1]);

    let inner = panel_block(&title).inner(vertical[0]);
    f.render_widget(panel_block(&title), vertical[0]);
    render_positions_panel(
        f,
        inner,
        &monitors,
        scroll.positions_scroll.scroll,
        ctx.live.as_ref(),
        ctx.rules.is_intraday(),
    );

    let total = positions_content_height(&monitors);
    if total > inner.height {
        let mut sb = ScrollbarState::new(total as usize)
            .position(scroll.positions_scroll.scroll as usize);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓")),
            vertical[1],
            &mut sb,
        );
    }
}

fn render_overview(
    f: &mut ratatui::Frame,
    area: Rect,
    ctx: &WatchContext,
    agent_mode: &str,
    dry_run: bool,
    simulate: bool,
    health: &crate::ui::health::AgentHealth,
    scroll: &mut WatchUiState,
    market_conditions: &Arc<std::sync::Mutex<MarketConditionsSnapshot>>,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Min(3),
        ])
        .split(area);

    let conditions = market_conditions
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    f.render_widget(
        wrap_paragraph(market_conditions_panel_lines(&conditions))
            .block(panel_block("Market")),
        rows[0],
    );

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    let agent_title = if simulate {
        "Agent (simulation)"
    } else if dry_run {
        "Agent (dry-run)"
    } else {
        "Agent"
    };
    f.render_widget(
        wrap_paragraph(overview_agent_lines(ctx, health, agent_mode))
            .block(panel_block(agent_title)),
        top[0],
    );
    f.render_widget(
        wrap_paragraph(rules_summary(ctx)).block(panel_block("Playbook")),
        top[1],
    );

    f.render_widget(
        wrap_paragraph(capital_lines(ctx)).block(panel_block("Capital")),
        rows[2],
    );

    f.render_widget(
        wrap_paragraph(if ctx.state.open_positions.is_empty() {
            entry_attempt_lines(ctx)
        } else {
            position_lines(ctx)
        })
        .block(panel_block(if ctx.state.open_positions.is_empty() {
            "Last Entry"
        } else {
            "Live Positions (preview)"
        })),
        rows[3],
    );

    render_scroll(
        f,
        rows[4],
        log_lines(ctx),
        scroll.log_scroll.scroll,
        "Agent Log",
        String::new(),
    );
}

fn render_scroll(
    f: &mut ratatui::Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    scroll_pos: u16,
    title: &str,
    bottom: String,
) {
    let line_count = lines.len() as u16;
    let scroll = scroll_pos.min(line_count.saturating_sub(1));
    let vertical = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let mut block = panel_block(title);
    if !bottom.is_empty() {
        block = block.title_bottom(format!(" {bottom} "));
    }

    f.render_widget(
        wrap_paragraph(lines)
            .style(Style::default().fg(Color::Gray))
            .scroll((scroll, 0))
            .block(block),
        vertical[0],
    );

    let mut sb = ScrollbarState::new(line_count as usize).position(scroll as usize);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight),
        vertical[1],
        &mut sb,
    );
}

pub fn short_path(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
