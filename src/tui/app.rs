use crate::logging::LogEntry;
use crate::state::AppState;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::collections::VecDeque;
use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};

use super::input::{handle_input, handle_input_mode};
use super::ui::draw;

const INTERNAL_LOG_CAP: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    Normal,
    AddName,
    AddUrl,
    AddToken,
    AddAuthType,
    AddModelMap,
    EditName,
    EditUrl,
    EditToken,
    EditAuthType,
    EditModelMap,
    Search,
    ShowToken,
    DetailView,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FocusPanel {
    Backends,
    RequestLog,
    InternalLog,
}

impl FocusPanel {
    pub fn next(&self) -> Self {
        match self {
            Self::Backends => Self::RequestLog,
            Self::RequestLog => Self::InternalLog,
            Self::InternalLog => Self::Backends,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Backends => Self::InternalLog,
            Self::RequestLog => Self::Backends,
            Self::InternalLog => Self::RequestLog,
        }
    }
}

pub struct TuiState {
    pub cursor: usize,
    pub mode: InputMode,
    pub input_buffer: String,
    pub pending_name: String,
    pub pending_url: String,
    pub pending_token: String,
    pub search_query: String,
    pub g_pressed: bool,
    pub log_cursor: usize,
    pub log_cursor_id: Option<u64>,
    pub log_scroll: usize,
    pub focus: FocusPanel,
    pub detail_scroll: usize,
    pub detail_entry_id: Option<u64>,
    pub auth_type_rejected: bool,
    pub model_map_rejected: bool,
    pub pending_auth_type: Option<crate::config::AuthType>,
    // Internal log panel state
    pub internal_log: VecDeque<LogEntry>,
    pub internal_log_rx: mpsc::Receiver<LogEntry>,
    pub internal_log_cursor: usize,
    pub internal_log_cursor_id: Option<u64>,
    pub internal_log_unread: usize,
}

impl Default for TuiState {
    fn default() -> Self {
        let (_tx, rx) = mpsc::channel(1);
        Self::new(rx)
    }
}

impl TuiState {
    pub fn new(log_rx: mpsc::Receiver<LogEntry>) -> Self {
        Self {
            cursor: 0,
            mode: InputMode::Normal,
            input_buffer: String::new(),
            pending_name: String::new(),
            pending_url: String::new(),
            pending_token: String::new(),
            search_query: String::new(),
            g_pressed: false,
            log_cursor: 0,
            log_cursor_id: None,
            log_scroll: 0,
            focus: FocusPanel::Backends,
            detail_scroll: 0,
            detail_entry_id: None,
            auth_type_rejected: false,
            model_map_rejected: false,
            pending_auth_type: None,
            internal_log: VecDeque::new(),
            internal_log_rx: log_rx,
            internal_log_cursor: 0,
            internal_log_cursor_id: None,
            internal_log_unread: 0,
        }
    }

    /// Drain pending log entries from the channel into the ring buffer.
    /// Only increments the unread counter when the panel is not focused,
    /// so entries seen while expanded don't appear as "new" on collapse.
    pub fn drain_log_channel(&mut self) {
        let track_unread = self.focus != FocusPanel::InternalLog;
        while let Ok(entry) = self.internal_log_rx.try_recv() {
            self.internal_log.push_back(entry);
            if self.internal_log.len() > INTERNAL_LOG_CAP {
                self.internal_log.pop_front();
                self.internal_log_cursor = self.internal_log_cursor.saturating_sub(1);
            }
            if track_unread {
                self.internal_log_unread += 1;
            }
        }
        // Keep the display index in sync with the stable entry id so that
        // new arrivals don't shift the selection.
        if let Some(id) = self.internal_log_cursor_id {
            if let Some(pos) = self.internal_log.iter().position(|e| e.id == id) {
                let display = self.internal_log.len().saturating_sub(1) - pos;
                self.internal_log_cursor = display;
            } else {
                self.internal_log_cursor = 0;
                self.internal_log_cursor_id = None;
            }
        }
    }

    /// Move the internal log cursor forward (toward older entries) by one.
    pub fn internal_log_cursor_down(&mut self) {
        let len = self.internal_log.len();
        if len > 0 {
            self.internal_log_cursor = (self.internal_log_cursor + 1).min(len.saturating_sub(1));
            let deque_idx = len.saturating_sub(1) - self.internal_log_cursor;
            self.internal_log_cursor_id = self.internal_log.get(deque_idx).map(|e| e.id);
        }
    }

    /// Move the internal log cursor backward (toward newer entries) by one.
    pub fn internal_log_cursor_up(&mut self) {
        let len = self.internal_log.len();
        if len > 0 {
            self.internal_log_cursor = self.internal_log_cursor.saturating_sub(1);
            let deque_idx = len.saturating_sub(1) - self.internal_log_cursor;
            self.internal_log_cursor_id = self.internal_log.get(deque_idx).map(|e| e.id);
        }
    }
}

pub fn run_tui(
    state: Arc<RwLock<AppState>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    log_rx: mpsc::Receiver<LogEntry>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut tui_state = TuiState::new(log_rx);

    let result = run_event_loop(&mut terminal, &state, &mut tui_state);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    let _ = shutdown_tx.send(true);
    result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: &Arc<RwLock<AppState>>,
    tui_state: &mut TuiState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rt = tokio::runtime::Handle::current();

    loop {
        tui_state.drain_log_channel();
        let app_state = rt.block_on(state.read()).clone();
        terminal.draw(|frame| draw(frame, &app_state, tui_state))?;

        if event::poll(Duration::from_millis(33))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(());
                }

                match tui_state.mode {
                    InputMode::Normal => {
                        if handle_input(key, tui_state, state, &rt) {
                            return Ok(());
                        }
                    }
                    _ => handle_input_mode(key, tui_state, state, &rt),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::LogEntry;
    use tracing::Level;

    fn make_entry(id: u64) -> LogEntry {
        LogEntry {
            id,
            timestamp: chrono::Local::now(),
            level: Level::INFO,
            target: "zone_router::test".into(),
            message: format!("entry {id}"),
        }
    }

    #[test]
    fn ring_buffer_caps_at_500() {
        let (tx, rx) = mpsc::channel(600);
        let mut state = TuiState::new(rx);
        for i in 0..600 {
            tx.try_send(make_entry(i)).unwrap();
        }
        state.drain_log_channel();
        assert_eq!(state.internal_log.len(), INTERNAL_LOG_CAP);
        // Oldest entries evicted — first entry should be id 100
        assert_eq!(state.internal_log.front().unwrap().id, 100);
        assert_eq!(state.internal_log.back().unwrap().id, 599);
    }

    #[test]
    fn cursor_clamped_on_eviction() {
        let (tx, rx) = mpsc::channel(600);
        let mut state = TuiState::new(rx);
        // Fill to exactly cap, cursor at end
        for i in 0..INTERNAL_LOG_CAP {
            tx.try_send(make_entry(i as u64)).unwrap();
        }
        state.drain_log_channel();
        state.internal_log_cursor = INTERNAL_LOG_CAP - 1; // last entry

        // Add one more — oldest evicted, cursor should decrement
        tx.try_send(make_entry(500)).unwrap();
        state.drain_log_channel();
        assert_eq!(state.internal_log_cursor, INTERNAL_LOG_CAP - 2);
    }

    #[test]
    fn unread_increments_only_when_not_focused() {
        let (tx, rx) = mpsc::channel(600);
        let mut state = TuiState::new(rx);
        assert_eq!(state.focus, FocusPanel::Backends);

        // Not focused on InternalLog — unread should increment
        tx.try_send(make_entry(0)).unwrap();
        state.drain_log_channel();
        assert_eq!(state.internal_log_unread, 1);

        // Switch focus to InternalLog
        state.focus = FocusPanel::InternalLog;
        tx.try_send(make_entry(1)).unwrap();
        state.drain_log_channel();
        // Entry added but unread NOT incremented while focused
        assert_eq!(state.internal_log.len(), 2);
        assert_eq!(state.internal_log_unread, 1);
    }
}
