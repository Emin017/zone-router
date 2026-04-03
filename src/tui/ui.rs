use crate::state::AppState;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use super::app::{FocusPanel, InputMode, TuiState};

fn focus_border_style(panel: &FocusPanel, current: &FocusPanel) -> Style {
    if panel == current {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

pub fn draw(frame: &mut Frame, state: &AppState, tui: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // status bar
            Constraint::Min(6),    // main area
            Constraint::Min(8),    // request log
            Constraint::Length(3), // help / input bar
        ])
        .split(frame.area());

    draw_status_bar(frame, state, chunks[0]);
    draw_main_area(frame, state, tui, chunks[1]);
    draw_request_log(frame, state, tui, chunks[2]);
    draw_help_bar(frame, tui, state, chunks[3]);
}

fn draw_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let token_display = state.local_token.chars().take(12).collect::<String>();
    let text = format!(
        " ● Proxy: {}  Token: {}...",
        state.config.proxy.listen, token_display
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" zone-router ");
    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_main_area(frame: &mut Frame, state: &AppState, tui: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    draw_backend_list(frame, state, tui, chunks[0]);
    draw_stats_panel(frame, state, chunks[1]);
}

fn draw_backend_list(frame: &mut Frame, state: &AppState, tui: &TuiState, area: Rect) {
    let items: Vec<ListItem> = state
        .config
        .backends
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let marker = if i == state.active_index {
                "✓ active"
            } else {
                ""
            };
            let model_marker = if b.model_map.as_ref().is_some_and(|m| m.has_any()) {
                " [M]"
            } else {
                ""
            };
            let cursor = if i == tui.cursor { "►" } else { " " };
            let line = Line::from(vec![
                Span::raw(format!(
                    "{cursor} [{n}] {name}{model_marker}  ",
                    n = i + 1,
                    name = b.name
                )),
                Span::styled(marker, Style::default().fg(Color::Green)),
            ]);
            let style = if i == tui.cursor {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Backends ")
        .border_style(focus_border_style(&FocusPanel::Backends, &tui.focus));
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_stats_panel(frame: &mut Frame, state: &AppState, area: Rect) {
    let stats_text = state
        .active_backend()
        .and_then(|b| state.stats.per_backend.get(&b.name))
        .map(|s| {
            vec![
                Line::from(format!(" Reqs:  {}", s.total_requests)),
                Line::from(format!(" OK:    {}", s.success_count)),
                Line::from(format!(" Err:   {}", s.error_count)),
                Line::from(format!(" Avg:   {:.0}ms", s.avg_latency_ms())),
            ]
        })
        .unwrap_or_else(|| vec![Line::from(" No stats yet")]);

    let block = Block::default().borders(Borders::ALL).title(" Stats ");
    let paragraph = Paragraph::new(stats_text).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_request_log(frame: &mut Frame, state: &AppState, tui: &TuiState, area: Rect) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let log_len = state.stats.log.len();
    let start = log_len.saturating_sub(visible_height + tui.log_scroll);
    let end = log_len.saturating_sub(tui.log_scroll);

    let items: Vec<ListItem> = state
        .stats
        .log
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .rev()
        .map(|entry| {
            let status_color = if entry.status < 400 {
                Color::Green
            } else {
                Color::Red
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!(" {} ", entry.timestamp.format("%H:%M:%S"))),
                Span::styled(
                    format!("{:<12}", entry.backend),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!(" {:<6} {:<20} ", entry.method, entry.path)),
                Span::styled(
                    format!("{}", entry.status),
                    Style::default().fg(status_color),
                ),
                Span::raw(format!("  {}ms", entry.latency_ms)),
            ]))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Request Log ({log_len}) "))
        .border_style(focus_border_style(&FocusPanel::RequestLog, &tui.focus));
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_help_bar(frame: &mut Frame, tui: &TuiState, state: &AppState, area: Rect) {
    let content = match &tui.mode {
        InputMode::Normal => Line::from(
            " [1-9] switch  [a] add  [d] delete  [e] edit  [t] token  [/] search  [q] quit",
        ),
        InputMode::ShowToken => Line::from(format!(
            " Token: {}  (press any key to dismiss)",
            state.local_token
        )),
        InputMode::AddName => input_prompt("Add backend - Name", &tui.input_buffer),
        InputMode::AddUrl => input_prompt("Add backend - URL", &tui.input_buffer),
        InputMode::AddToken => input_prompt("Add backend - Token", &tui.input_buffer),
        InputMode::AddAuthType => input_prompt(
            "Add backend - Auth type (1=api-key, 2=bearer)",
            &tui.input_buffer,
        ),
        InputMode::AddModelMap => input_prompt(
            "Model map (e.g. haiku=x,sonnet=y,opus=z) or Enter to skip",
            &tui.input_buffer,
        ),
        InputMode::EditName => input_prompt("Edit - Name", &tui.input_buffer),
        InputMode::EditUrl => input_prompt("Edit - URL", &tui.input_buffer),
        InputMode::EditToken => input_prompt("Edit - Token", &tui.input_buffer),
        InputMode::EditAuthType => {
            input_prompt("Edit - Auth type (1=api-key, 2=bearer)", &tui.input_buffer)
        }
        InputMode::EditModelMap => input_prompt(
            "Edit - Model map (e.g. haiku=x,sonnet=y,opus=z) or Enter to clear",
            &tui.input_buffer,
        ),
        InputMode::Search => input_prompt("Search", &tui.input_buffer),
    };
    let block = Block::default().borders(Borders::ALL);
    let paragraph = Paragraph::new(content).block(block);
    frame.render_widget(paragraph, area);
}

fn input_prompt(label: &str, buffer: &str) -> Line<'static> {
    Line::from(format!(" {label}: {buffer}_"))
}
