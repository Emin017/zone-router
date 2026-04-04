use crate::state::AppState;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use super::input::{handle_input, handle_input_mode};
use super::ui::draw;

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
}

impl FocusPanel {
    pub fn toggle(&self) -> Self {
        match self {
            Self::Backends => Self::RequestLog,
            Self::RequestLog => Self::Backends,
        }
    }
}

#[derive(Debug, Clone)]
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
}

impl Default for TuiState {
    fn default() -> Self {
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
        }
    }
}

pub fn run_tui(
    state: Arc<RwLock<AppState>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut tui_state = TuiState::default();

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
