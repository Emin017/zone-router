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
    pub internal_log_rx: mpsc::UnboundedReceiver<LogEntry>,
    pub internal_log_cursor: usize,
    pub internal_log_unread: usize,
}

impl Default for TuiState {
    fn default() -> Self {
        let (_tx, rx) = mpsc::unbounded_channel();
        Self::new(rx)
    }
}

impl TuiState {
    pub fn new(log_rx: mpsc::UnboundedReceiver<LogEntry>) -> Self {
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
            internal_log_unread: 0,
        }
    }

    /// Drain pending log entries from the channel into the ring buffer.
    pub fn drain_log_channel(&mut self) {
        while let Ok(entry) = self.internal_log_rx.try_recv() {
            self.internal_log.push_back(entry);
            if self.internal_log.len() > INTERNAL_LOG_CAP {
                self.internal_log.pop_front();
                self.internal_log_cursor = self.internal_log_cursor.saturating_sub(1);
            }
            self.internal_log_unread += 1;
        }
    }
}

pub fn run_tui(
    state: Arc<RwLock<AppState>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    log_rx: mpsc::UnboundedReceiver<LogEntry>,
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
