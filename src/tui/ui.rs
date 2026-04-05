use crate::state::AppState;
use crate::stats::RequestLogEntry;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use super::app::{FocusPanel, InputMode, TuiState};

fn focus_border_style(panel: &FocusPanel, current: &FocusPanel) -> Style {
    if panel == current {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

pub fn draw(frame: &mut Frame, state: &AppState, tui: &mut TuiState) {
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

    if tui.mode == InputMode::DetailView {
        draw_detail_panel(frame, state, tui, chunks[2]);
    }

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

    // Resolve log_cursor from the stable entry ID so the highlighted row
    // doesn't shift when new entries arrive.
    let log_cursor = tui
        .log_cursor_id
        .and_then(|id| {
            state
                .stats
                .log
                .iter()
                .position(|e| e.id == id)
                .map(|deque_idx| log_len.saturating_sub(1) - deque_idx)
        })
        .unwrap_or(tui.log_cursor);

    let scroll = if log_cursor >= visible_height {
        log_cursor - visible_height + 1
    } else {
        0
    };

    let items: Vec<ListItem> = (0..log_len)
        .rev()
        .skip(scroll)
        .take(visible_height)
        .filter_map(|deque_idx| {
            let entry = state.stats.log.get(deque_idx)?;
            let display_idx = log_len.saturating_sub(1) - deque_idx;
            let is_selected = display_idx == log_cursor && tui.focus == FocusPanel::RequestLog;

            let cursor_marker = if is_selected { ">" } else { " " };
            let status_color = if entry.status < 400 {
                Color::Green
            } else {
                Color::Red
            };
            let line = Line::from(vec![
                Span::raw(format!(
                    "{} {} ",
                    cursor_marker,
                    entry.timestamp.format("%H:%M:%S")
                )),
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
            ]);
            let style = if is_selected {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray)
            } else {
                Style::default()
            };
            Some(ListItem::new(line).style(style))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Request Log ({log_len}) "))
        .border_style(focus_border_style(&FocusPanel::RequestLog, &tui.focus));
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn centered_rect(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let w = (area.width as u32 * width_pct as u32 / 100) as u16;
    let h = (area.height as u32 * height_pct as u32 / 100) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn build_detail_lines(entry: &RequestLogEntry) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    lines.push(Line::from(format!(
        " Timestamp: {}",
        entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    )));
    lines.push(Line::from(format!(" Backend:   {}", entry.backend)));
    lines.push(Line::from(format!(" Method:    {}", entry.method)));
    lines.push(Line::from(format!(" Path:      {}", entry.path)));
    lines.push(Line::from(vec![
        Span::raw(" Status:    ".to_string()),
        Span::styled(
            format!("{}", entry.status),
            Style::default().fg(if entry.status < 400 {
                Color::Green
            } else {
                Color::Red
            }),
        ),
    ]));
    lines.push(Line::from(format!(" Latency:   {}ms", entry.latency_ms)));
    lines.push(Line::from(""));

    lines.push(Line::from(format!(
        " Model:     {}",
        entry.model.as_deref().unwrap_or("(none)")
    )));
    lines.push(Line::from(format!(" Transfer:  {}", entry.transfer_type)));
    lines.push(Line::from(""));

    match &entry.usage {
        Some(usage) => {
            lines.push(Line::styled(
                " Token Usage",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(format!("   Input:  {}", usage.input_tokens)));
            lines.push(Line::from(format!("   Output: {}", usage.output_tokens)));
            lines.push(Line::from(format!(
                "   Total:  {}",
                usage.input_tokens + usage.output_tokens
            )));
        }
        None => {
            lines.push(Line::from(" Token Usage: (not available)"));
        }
    }

    lines
}

fn draw_detail_panel(frame: &mut Frame, state: &AppState, tui: &mut TuiState, log_area: Rect) {
    let log_len = state.stats.log.len();
    if log_len == 0 {
        return;
    }

    let entry = match tui.detail_entry_id {
        Some(id) => state
            .stats
            .log
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| {
                // Entry was evicted — fall back to the current cursor position
                let clamped = tui.log_cursor.min(log_len.saturating_sub(1));
                let deque_idx = log_len.saturating_sub(1) - clamped;
                &state.stats.log[deque_idx]
            }),
        None => {
            let clamped = tui.log_cursor.min(log_len.saturating_sub(1));
            let deque_idx = log_len.saturating_sub(1) - clamped;
            match state.stats.log.get(deque_idx) {
                Some(e) => e,
                None => return,
            }
        }
    };

    let panel_area = centered_rect(log_area, 80, 90);

    let lines = build_detail_lines(entry);
    let max_scroll = lines
        .len()
        .saturating_sub(panel_area.height.saturating_sub(2) as usize);
    tui.detail_scroll = tui.detail_scroll.min(max_scroll);
    let scroll = tui.detail_scroll;

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Request Detail ")
        .border_style(Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0));

    frame.render_widget(Clear, panel_area);
    frame.render_widget(paragraph, panel_area);
}

fn draw_help_bar(frame: &mut Frame, tui: &TuiState, state: &AppState, area: Rect) {
    let content = match &tui.mode {
        InputMode::Normal => Line::from(
            " [1-9] switch  [a] add  [d] delete  [e] edit  [t] token  [/] search  [q] quit",
        ),
        InputMode::DetailView => Line::from(" [j/k] scroll  [n/p] next/prev  [Esc/h] close"),
        InputMode::ShowToken => Line::from(format!(
            " Token: {}  (press any key to dismiss)",
            state.local_token
        )),
        InputMode::AddName => input_prompt("Add backend - Name", &tui.input_buffer),
        InputMode::AddUrl => input_prompt("Add backend - URL", &tui.input_buffer),
        InputMode::AddToken => input_prompt("Add backend - Token", &tui.input_buffer),
        InputMode::AddAuthType => input_prompt(
            "Auth (1=api-key, 2=bearer) Enter=done, Tab=model map",
            &tui.input_buffer,
        ),
        InputMode::AddModelMap => input_prompt(
            "Model map (e.g. haiku=x,sonnet=y,opus=z) or Enter to skip",
            &tui.input_buffer,
        ),
        InputMode::EditName => input_prompt("Edit - Name", &tui.input_buffer),
        InputMode::EditUrl => input_prompt("Edit - URL", &tui.input_buffer),
        InputMode::EditToken => input_prompt("Edit - Token", &tui.input_buffer),
        InputMode::EditAuthType => input_prompt(
            "Auth (1=api-key, 2=bearer) Enter=done, Tab=model map",
            &tui.input_buffer,
        ),
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
