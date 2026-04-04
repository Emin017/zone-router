use crate::state::AppState;
use crate::stats::RequestLogEntry;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

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

    // Derive scroll from cursor position to keep cursor visible.
    // log_cursor=0 is the newest entry (displayed at top). The VecDeque stores
    // oldest-first, so display index i maps to VecDeque index (log_len - 1 - i).
    let scroll = if tui.log_cursor >= visible_height {
        tui.log_cursor - visible_height + 1
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
            let is_selected = display_idx == tui.log_cursor && tui.focus == FocusPanel::RequestLog;

            let cursor_marker = if is_selected { ">" } else { " " };
            let status_color = if entry.response.status < 400 {
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
                Span::raw(format!(
                    " {:<6} {:<20} ",
                    entry.request.method, entry.request.path
                )),
                Span::styled(
                    format!("{}", entry.response.status),
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
    let w = area.width * width_pct / 100;
    let h = area.height * height_pct / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn build_detail_lines(entry: &RequestLogEntry, body_expanded: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    lines.push(Line::from(format!(
        " Timestamp: {}",
        entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    )));
    lines.push(Line::from(format!(" Backend:   {}", entry.backend)));
    lines.push(Line::from(format!(" Method:    {}", entry.request.method)));
    lines.push(Line::from(format!(" Path:      {}", entry.request.path)));
    lines.push(Line::from(vec![
        Span::raw(" Status:    ".to_string()),
        Span::styled(
            format!("{}", entry.response.status),
            Style::default().fg(if entry.response.status < 400 {
                Color::Green
            } else {
                Color::Red
            }),
        ),
        Span::raw(format!("    Latency: {}ms", entry.latency_ms)),
    ]));
    lines.push(Line::from(""));

    // Request Headers
    lines.push(Line::styled(
        " ─── Request Headers ───────────────────────────────",
        Style::default().fg(Color::DarkGray),
    ));
    for (name, value) in &entry.request.headers.0 {
        lines.push(Line::from(format!(" {name}: {value}")));
    }
    if entry.request.headers.0.is_empty() {
        lines.push(Line::from(" (none)"));
    }
    lines.push(Line::from(""));

    // Request Body (collapsible)
    match (&entry.request.body, body_expanded) {
        (Some(body), false) => {
            lines.push(Line::from(format!(
                " ▸ Request Body ({} bytes)",
                body.len()
            )));
        }
        (Some(body), true) => {
            lines.push(Line::styled(
                " ▾ Request Body",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            for line in body.lines() {
                lines.push(Line::from(format!(" {line}")));
            }
        }
        (None, _) => {
            lines.push(Line::from(" ▸ Request Body (empty)"));
        }
    }
    lines.push(Line::from(""));

    // Response Headers
    lines.push(Line::styled(
        " ─── Response Headers ──────────────────────────────",
        Style::default().fg(Color::DarkGray),
    ));
    for (name, value) in &entry.response.headers.0 {
        lines.push(Line::from(format!(" {name}: {value}")));
    }
    if entry.response.headers.0.is_empty() {
        lines.push(Line::from(" (none)"));
    }
    lines.push(Line::from(""));

    // Response Body
    lines.push(Line::styled(
        " ─── Response Body ─────────────────────────────────",
        Style::default().fg(Color::DarkGray),
    ));
    match &entry.response.body {
        Some(body) => {
            for line in body.lines() {
                lines.push(Line::from(format!(" {line}")));
            }
        }
        None => {
            lines.push(Line::from(" (empty)"));
        }
    }

    lines
}

fn draw_detail_panel(frame: &mut Frame, state: &AppState, tui: &TuiState, log_area: Rect) {
    let log_len = state.stats.log.len();
    if log_len == 0 {
        return;
    }

    let clamped_cursor = tui.log_cursor.min(log_len.saturating_sub(1));
    let deque_idx = log_len.saturating_sub(1) - clamped_cursor;
    let Some(entry) = state.stats.log.get(deque_idx) else {
        return;
    };

    let panel_area = centered_rect(log_area, 80, 90);

    let lines = build_detail_lines(entry, tui.body_expanded);
    let max_scroll = lines
        .len()
        .saturating_sub(panel_area.height.saturating_sub(2) as usize);
    let scroll = tui.detail_scroll.min(max_scroll);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Request Detail ")
        .border_style(Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, panel_area);
    frame.render_widget(paragraph, panel_area);
}

fn draw_help_bar(frame: &mut Frame, tui: &TuiState, state: &AppState, area: Rect) {
    let content = match &tui.mode {
        InputMode::Normal => Line::from(
            " [1-9] switch  [a] add  [d] delete  [e] edit  [t] token  [/] search  [q] quit",
        ),
        InputMode::DetailView => {
            Line::from(" [j/k] scroll  [Enter] toggle body  [n/p] next/prev  [Esc/h] close")
        }
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
