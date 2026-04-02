use crate::state::AppState;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use super::app::{InputMode, TuiState};

pub fn draw(frame: &mut Frame, state: &AppState, tui: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // status bar
            Constraint::Min(6),    // main area
            Constraint::Min(8),    // request log
            Constraint::Length(3), // help / input bar
        ])
        .split(frame.area());

    draw_status_bar(frame, state, tui, chunks[0]);
    draw_main_area(frame, state, tui, chunks[1]);
    draw_request_log(frame, state, tui, chunks[2]);
    draw_help_bar(frame, tui, state, chunks[3]);
}

fn draw_status_bar(frame: &mut Frame, state: &AppState, _tui: &TuiState, area: Rect) {
    let token_display = state.local_token.chars().take(12).collect::<String>();
    let text = format!(
        " ● Proxy: {}  Token: {}...",
        state.config.proxy.listen, token_display
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" api-router ");
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
            let marker = if i == state.active_index { "✓ active" } else { "" };
            let cursor = if i == tui.cursor { "►" } else { " " };
            let line = Line::from(vec![
                Span::raw(format!("{cursor} [{n}] {name}  ", n = i + 1, name = b.name)),
                Span::styled(marker, Style::default().fg(Color::Green)),
            ]);
            let style = if i == tui.cursor {
                Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(" Backends ");
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
            let status_color = if entry.status < 400 { Color::Green } else { Color::Red };
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
        .title(format!(" Request Log ({log_len}) "));
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_help_bar(frame: &mut Frame, tui: &TuiState, state: &AppState, area: Rect) {
    let content = match &tui.mode {
        InputMode::Normal => {
            Line::from(" [1-9] switch  [a] add  [d] delete  [e] edit  [t] token  [/] search  [q] quit")
        }
        InputMode::ShowToken => {
            Line::from(format!(" Token: {}  (press any key to dismiss)", state.local_token))
        }
        InputMode::AddName => Line::from(format!(" Add backend - Name: {}_", tui.input_buffer)),
        InputMode::AddUrl => Line::from(format!(" Add backend - URL: {}_", tui.input_buffer)),
        InputMode::AddToken => Line::from(format!(" Add backend - Token: {}_", tui.input_buffer)),
        InputMode::EditName => Line::from(format!(" Edit - Name: {}_", tui.input_buffer)),
        InputMode::EditUrl => Line::from(format!(" Edit - URL: {}_", tui.input_buffer)),
        InputMode::EditToken => Line::from(format!(" Edit - Token: {}_", tui.input_buffer)),
        InputMode::Search => Line::from(format!(" Search: {}_", tui.input_buffer)),
    };
    let block = Block::default().borders(Borders::ALL);
    let paragraph = Paragraph::new(content).block(block);
    frame.render_widget(paragraph, area);
}
