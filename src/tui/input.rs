use crate::config::Backend;
use crate::state::AppState;
use crossterm::event::{KeyCode, KeyEvent};
use std::sync::Arc;
use tokio::sync::RwLock;

use super::app::{InputMode, TuiState};

/// Returns true if the TUI should exit.
pub fn handle_input(
    key: KeyEvent,
    tui: &mut TuiState,
    state: &Arc<RwLock<AppState>>,
    rt: &tokio::runtime::Handle,
) -> bool {
    let backend_count = rt.block_on(state.read()).config.backends.len();

    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('j') => {
            tui.g_pressed = false;
            if backend_count > 0 {
                tui.cursor = (tui.cursor + 1).min(backend_count - 1);
            }
        }
        KeyCode::Char('k') => {
            tui.g_pressed = false;
            tui.cursor = tui.cursor.saturating_sub(1);
        }
        KeyCode::Char('G') => {
            tui.g_pressed = false;
            if backend_count > 0 {
                tui.cursor = backend_count - 1;
            }
        }
        KeyCode::Char('g') => {
            if tui.g_pressed {
                tui.cursor = 0;
                tui.g_pressed = false;
            } else {
                tui.g_pressed = true;
            }
        }
        KeyCode::Enter => {
            tui.g_pressed = false;
            rt.block_on(state.write()).switch_backend(tui.cursor);
        }
        KeyCode::Char(c @ '1'..='9') => {
            tui.g_pressed = false;
            let idx = (c as usize) - ('1' as usize);
            if idx < backend_count {
                tui.cursor = idx;
                rt.block_on(state.write()).switch_backend(idx);
            }
        }
        KeyCode::Char('a') => {
            tui.g_pressed = false;
            tui.mode = InputMode::AddName;
            tui.input_buffer.clear();
            tui.pending_name.clear();
            tui.pending_url.clear();
        }
        KeyCode::Char('d') => {
            tui.g_pressed = false;
            if backend_count > 0 {
                rt.block_on(state.write()).remove_backend(tui.cursor);
                let new_count = rt.block_on(state.read()).config.backends.len();
                if tui.cursor >= new_count && new_count > 0 {
                    tui.cursor = new_count - 1;
                }
            }
        }
        KeyCode::Char('e') => {
            tui.g_pressed = false;
            if tui.cursor < backend_count {
                let s = rt.block_on(state.read());
                let b = &s.config.backends[tui.cursor];
                tui.pending_name = b.name.clone();
                tui.pending_url = b.url.clone();
                tui.input_buffer = b.name.clone();
                tui.mode = InputMode::EditName;
            }
        }
        KeyCode::Char('t') => {
            tui.g_pressed = false;
            tui.mode = InputMode::ShowToken;
        }
        KeyCode::Char('/') => {
            tui.g_pressed = false;
            tui.mode = InputMode::Search;
            tui.input_buffer.clear();
            tui.search_query.clear();
        }
        KeyCode::Tab => {
            tui.g_pressed = false;
            // cycle log scroll
            let log_len = rt.block_on(state.read()).stats.log.len();
            if log_len > 0 {
                tui.log_scroll = (tui.log_scroll + 10).min(log_len.saturating_sub(1));
            }
        }
        KeyCode::BackTab => {
            tui.g_pressed = false;
            tui.log_scroll = tui.log_scroll.saturating_sub(10);
        }
        _ => {
            tui.g_pressed = false;
        }
    }
    false
}

pub fn handle_input_mode(
    key: KeyEvent,
    tui: &mut TuiState,
    state: &Arc<RwLock<AppState>>,
    rt: &tokio::runtime::Handle,
) {
    match key.code {
        KeyCode::Esc => {
            tui.mode = InputMode::Normal;
            tui.input_buffer.clear();
        }
        KeyCode::Enter => match &tui.mode {
            InputMode::AddName => {
                tui.pending_name = tui.input_buffer.clone();
                tui.input_buffer.clear();
                tui.mode = InputMode::AddUrl;
            }
            InputMode::AddUrl => {
                tui.pending_url = tui.input_buffer.clone();
                tui.input_buffer.clear();
                tui.mode = InputMode::AddToken;
            }
            InputMode::AddToken => {
                let backend = Backend {
                    name: tui.pending_name.clone(),
                    url: tui.pending_url.clone(),
                    token: tui.input_buffer.clone(),
                    active: false,
                };
                rt.block_on(state.write()).add_backend(backend);
                tui.input_buffer.clear();
                tui.mode = InputMode::Normal;
            }
            InputMode::EditName => {
                tui.pending_name = tui.input_buffer.clone();
                tui.input_buffer = rt.block_on(state.read())
                    .config.backends.get(tui.cursor)
                    .map(|b| b.url.clone())
                    .unwrap_or_default();
                tui.mode = InputMode::EditUrl;
            }
            InputMode::EditUrl => {
                tui.pending_url = tui.input_buffer.clone();
                tui.input_buffer = rt.block_on(state.read())
                    .config.backends.get(tui.cursor)
                    .map(|b| b.token.clone())
                    .unwrap_or_default();
                tui.mode = InputMode::EditToken;
            }
            InputMode::EditToken => {
                rt.block_on(state.write()).update_backend(
                    tui.cursor,
                    tui.pending_name.clone(),
                    tui.pending_url.clone(),
                    tui.input_buffer.clone(),
                );
                tui.input_buffer.clear();
                tui.mode = InputMode::Normal;
            }
            InputMode::Search => {
                tui.search_query = tui.input_buffer.clone();
                let s = rt.block_on(state.read());
                let found = s.config.backends.iter()
                    .position(|b| b.name.to_lowercase().contains(&tui.search_query.to_lowercase()));
                if let Some(idx) = found {
                    tui.cursor = idx;
                }
                tui.mode = InputMode::Normal;
            }
            InputMode::ShowToken => {
                tui.mode = InputMode::Normal;
            }
            InputMode::Normal => {}
        },
        KeyCode::Backspace => {
            tui.input_buffer.pop();
        }
        KeyCode::Char(c) => {
            if tui.mode == InputMode::ShowToken {
                tui.mode = InputMode::Normal;
            } else {
                tui.input_buffer.push(c);
            }
        }
        _ => {}
    }
}
