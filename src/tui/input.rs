use crate::config::{AuthType, Backend, ModelMap};
use crate::state::AppState;
use crossterm::event::{KeyCode, KeyEvent};
use std::sync::Arc;
use tokio::sync::RwLock;

use super::app::{FocusPanel, InputMode, TuiState};

/// Resolve auth-type from the input buffer, returning `None` on invalid input.
/// When input is empty and not previously rejected, returns the provided `default`.
fn resolve_auth_type(input: &str, default: AuthType, rejected: bool) -> Option<AuthType> {
    let input_empty = input.trim().is_empty();
    if input_empty && !rejected {
        Some(default)
    } else {
        AuthType::from_input(input).filter(|_| !input_empty)
    }
}

/// Resolve model-map from the input buffer, returning `None` on invalid/rejected input.
/// When input is empty and not previously rejected, returns `Some(None)` (skip).
/// Valid input returns `Some(Some(map))`. Invalid or post-rejection empty returns `None`.
fn resolve_model_map(input: &str, rejected: bool) -> Option<Option<ModelMap>> {
    match parse_model_map_input(input) {
        Some(mm) => Some(Some(mm)),
        None if input.trim().is_empty() && !rejected => Some(None),
        None => None,
    }
}

/// Returns true if the TUI should exit.
pub fn handle_input(
    key: KeyEvent,
    tui: &mut TuiState,
    state: &Arc<RwLock<AppState>>,
    rt: &tokio::runtime::Handle,
) -> bool {
    let (backend_count, log_len) = {
        let s = rt.block_on(state.read());
        (s.config.backends.len(), s.stats.log.len())
    };

    // Reset g_pressed for all keys except 'g' (which manages it internally)
    let was_g_pressed = tui.g_pressed;
    tui.g_pressed = false;

    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('j') => match tui.focus {
            FocusPanel::Backends => {
                if backend_count > 0 {
                    tui.cursor = (tui.cursor + 1).min(backend_count - 1);
                }
            }
            FocusPanel::RequestLog => {
                if log_len > 0 {
                    tui.log_cursor = (tui.log_cursor + 1).min(log_len.saturating_sub(1));
                }
            }
        },
        KeyCode::Char('k') => match tui.focus {
            FocusPanel::Backends => {
                tui.cursor = tui.cursor.saturating_sub(1);
            }
            FocusPanel::RequestLog => {
                tui.log_cursor = tui.log_cursor.saturating_sub(1);
            }
        },
        KeyCode::Char('G') => {
            if tui.focus == FocusPanel::Backends && backend_count > 0 {
                tui.cursor = backend_count - 1;
            }
        }
        KeyCode::Char('g') => {
            if tui.focus == FocusPanel::Backends && was_g_pressed {
                tui.cursor = 0;
            } else if tui.focus == FocusPanel::Backends {
                tui.g_pressed = true;
            }
        }
        KeyCode::Enter => {
            if tui.focus == FocusPanel::Backends {
                rt.block_on(state.write()).switch_backend(tui.cursor);
            } else if tui.focus == FocusPanel::RequestLog && log_len > 0 {
                tui.detail_scroll = 0;
                tui.body_expanded = false;
                let deque_idx =
                    log_len.saturating_sub(1) - tui.log_cursor.min(log_len.saturating_sub(1));
                let s = rt.block_on(state.read());
                tui.detail_entry_id = s.stats.log.get(deque_idx).map(|e| e.id);
                drop(s);
                tui.mode = InputMode::DetailView;
            }
        }
        KeyCode::Char(c @ '1'..='9') => {
            if tui.focus == FocusPanel::Backends {
                let idx = (c as usize) - ('1' as usize);
                if idx < backend_count {
                    tui.cursor = idx;
                    rt.block_on(state.write()).switch_backend(idx);
                }
            }
        }
        KeyCode::Char('a') => {
            if tui.focus == FocusPanel::Backends {
                tui.mode = InputMode::AddName;
                tui.input_buffer.clear();
                tui.pending_name.clear();
                tui.pending_url.clear();
                tui.pending_token.clear();
            }
        }
        KeyCode::Char('d') => {
            if tui.focus == FocusPanel::Backends && backend_count > 0 {
                rt.block_on(state.write()).remove_backend(tui.cursor);
                let new_count = rt.block_on(state.read()).config.backends.len();
                if tui.cursor >= new_count && new_count > 0 {
                    tui.cursor = new_count - 1;
                }
            }
        }
        KeyCode::Char('e') => {
            if tui.focus == FocusPanel::Backends && tui.cursor < backend_count {
                let s = rt.block_on(state.read());
                let b = &s.config.backends[tui.cursor];
                tui.pending_name = b.name.clone();
                tui.pending_url = b.url.clone();
                tui.input_buffer = b.name.clone();
                tui.mode = InputMode::EditName;
            }
        }
        KeyCode::Char('t') => {
            tui.mode = InputMode::ShowToken;
        }
        KeyCode::Char('/') => {
            tui.mode = InputMode::Search;
            tui.input_buffer.clear();
            tui.search_query.clear();
        }
        KeyCode::Tab | KeyCode::BackTab => {
            tui.focus = tui.focus.toggle();
        }
        _ => {}
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
            if tui.mode != InputMode::DetailView {
                tui.input_buffer.clear();
                tui.auth_type_rejected = false;
                tui.model_map_rejected = false;
            }
            tui.mode = InputMode::Normal;
        }
        KeyCode::Enter if tui.mode == InputMode::DetailView => {
            tui.body_expanded = !tui.body_expanded;
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
                tui.pending_token = tui.input_buffer.clone();
                tui.input_buffer.clear();
                tui.mode = InputMode::AddAuthType;
            }
            InputMode::AddAuthType => {
                let Some(auth_type) = resolve_auth_type(
                    &tui.input_buffer,
                    AuthType::default(),
                    tui.auth_type_rejected,
                ) else {
                    tui.input_buffer.clear();
                    tui.auth_type_rejected = true;
                    return;
                };
                let backend = Backend {
                    name: tui.pending_name.clone(),
                    url: tui.pending_url.clone(),
                    token: tui.pending_token.clone(),
                    active: false,
                    auth_type,
                    model_map: None,
                };
                rt.block_on(state.write()).add_backend(backend);
                tui.input_buffer.clear();
                tui.auth_type_rejected = false;
                tui.pending_auth_type = None;
                tui.mode = InputMode::Normal;
            }
            InputMode::AddModelMap => {
                let Some(model_map) = resolve_model_map(&tui.input_buffer, tui.model_map_rejected)
                else {
                    tui.input_buffer.clear();
                    tui.model_map_rejected = true;
                    return;
                };
                let backend = Backend {
                    name: tui.pending_name.clone(),
                    url: tui.pending_url.clone(),
                    token: tui.pending_token.clone(),
                    active: false,
                    auth_type: tui.pending_auth_type.unwrap_or_default(),
                    model_map,
                };
                rt.block_on(state.write()).add_backend(backend);
                tui.input_buffer.clear();
                tui.pending_auth_type = None;
                tui.model_map_rejected = false;
                tui.mode = InputMode::Normal;
            }
            InputMode::EditName => {
                tui.pending_name = tui.input_buffer.clone();
                tui.input_buffer = rt
                    .block_on(state.read())
                    .config
                    .backends
                    .get(tui.cursor)
                    .map(|b| b.url.clone())
                    .unwrap_or_default();
                tui.mode = InputMode::EditUrl;
            }
            InputMode::EditUrl => {
                tui.pending_url = tui.input_buffer.clone();
                tui.input_buffer = rt
                    .block_on(state.read())
                    .config
                    .backends
                    .get(tui.cursor)
                    .map(|b| b.token.clone())
                    .unwrap_or_default();
                tui.mode = InputMode::EditToken;
            }
            InputMode::EditToken => {
                tui.pending_token = tui.input_buffer.clone();
                tui.pending_auth_type = rt
                    .block_on(state.read())
                    .config
                    .backends
                    .get(tui.cursor)
                    .map(|b| b.auth_type);
                tui.input_buffer.clear();
                tui.mode = InputMode::EditAuthType;
            }
            InputMode::EditAuthType => {
                let Some(auth_type) = resolve_auth_type(
                    &tui.input_buffer,
                    tui.pending_auth_type.unwrap_or_default(),
                    tui.auth_type_rejected,
                ) else {
                    tui.input_buffer.clear();
                    tui.auth_type_rejected = true;
                    return;
                };
                let existing_mm = rt
                    .block_on(state.read())
                    .config
                    .backends
                    .get(tui.cursor)
                    .and_then(|b| b.model_map.clone());
                rt.block_on(state.write()).update_backend(
                    tui.cursor,
                    tui.pending_name.clone(),
                    tui.pending_url.clone(),
                    tui.pending_token.clone(),
                    auth_type,
                    existing_mm,
                );
                tui.input_buffer.clear();
                tui.auth_type_rejected = false;
                tui.pending_auth_type = None;
                tui.mode = InputMode::Normal;
            }
            InputMode::EditModelMap => {
                let Some(model_map) = resolve_model_map(&tui.input_buffer, tui.model_map_rejected)
                else {
                    tui.input_buffer.clear();
                    tui.model_map_rejected = true;
                    return;
                };
                rt.block_on(state.write()).update_backend(
                    tui.cursor,
                    tui.pending_name.clone(),
                    tui.pending_url.clone(),
                    tui.pending_token.clone(),
                    tui.pending_auth_type.unwrap_or_default(),
                    model_map,
                );
                tui.input_buffer.clear();
                tui.auth_type_rejected = false;
                tui.pending_auth_type = None;
                tui.model_map_rejected = false;
                tui.mode = InputMode::Normal;
            }
            InputMode::Search => {
                tui.search_query = tui.input_buffer.clone();
                let s = rt.block_on(state.read());
                let found = s.config.backends.iter().position(|b| {
                    b.name
                        .to_lowercase()
                        .contains(&tui.search_query.to_lowercase())
                });
                if let Some(idx) = found {
                    tui.cursor = idx;
                }
                tui.mode = InputMode::Normal;
            }
            InputMode::ShowToken => {
                tui.mode = InputMode::Normal;
            }
            InputMode::Normal | InputMode::DetailView => {}
        },
        KeyCode::Backspace => {
            tui.input_buffer.pop();
        }
        KeyCode::Tab if tui.mode == InputMode::AddAuthType => {
            let Some(auth_type) = resolve_auth_type(
                &tui.input_buffer,
                AuthType::default(),
                tui.auth_type_rejected,
            ) else {
                tui.input_buffer.clear();
                tui.auth_type_rejected = true;
                return;
            };
            tui.pending_auth_type = Some(auth_type);
            tui.input_buffer.clear();
            tui.auth_type_rejected = false;
            tui.mode = InputMode::AddModelMap;
        }
        KeyCode::Tab if tui.mode == InputMode::EditAuthType => {
            let Some(auth_type) = resolve_auth_type(
                &tui.input_buffer,
                tui.pending_auth_type.unwrap_or_default(),
                tui.auth_type_rejected,
            ) else {
                tui.input_buffer.clear();
                tui.auth_type_rejected = true;
                return;
            };
            tui.pending_auth_type = Some(auth_type);
            tui.auth_type_rejected = false;
            tui.input_buffer = format_model_map(
                rt.block_on(state.read())
                    .config
                    .backends
                    .get(tui.cursor)
                    .and_then(|b| b.model_map.as_ref()),
            );
            tui.mode = InputMode::EditModelMap;
        }
        KeyCode::Char(c) => {
            if tui.mode == InputMode::DetailView {
                match c {
                    'j' => {
                        tui.detail_scroll = tui.detail_scroll.saturating_add(1);
                    }
                    'k' => {
                        tui.detail_scroll = tui.detail_scroll.saturating_sub(1);
                    }
                    'n' => {
                        if let Some(current_id) = tui.detail_entry_id {
                            let s = rt.block_on(state.read());
                            let pos = s.stats.log.iter().position(|e| e.id == current_id);
                            if let Some(idx) = pos {
                                if idx > 0 {
                                    tui.detail_entry_id = Some(s.stats.log[idx - 1].id);
                                    tui.log_cursor =
                                        s.stats.log.len().saturating_sub(1) - (idx - 1);
                                    tui.detail_scroll = 0;
                                    tui.body_expanded = false;
                                }
                            }
                        }
                    }
                    'p' => {
                        if let Some(current_id) = tui.detail_entry_id {
                            let s = rt.block_on(state.read());
                            let pos = s.stats.log.iter().position(|e| e.id == current_id);
                            if let Some(idx) = pos {
                                if idx + 1 < s.stats.log.len() {
                                    tui.detail_entry_id = Some(s.stats.log[idx + 1].id);
                                    tui.log_cursor =
                                        s.stats.log.len().saturating_sub(1) - (idx + 1);
                                    tui.detail_scroll = 0;
                                    tui.body_expanded = false;
                                }
                            }
                        }
                    }
                    'h' => {
                        tui.mode = InputMode::Normal;
                    }
                    _ => {}
                }
            } else if tui.mode == InputMode::ShowToken {
                tui.mode = InputMode::Normal;
            } else {
                tui.input_buffer.push(c);
            }
        }
        _ => {}
    }
}

/// Parse comma-separated `key=value` pairs into a `ModelMap`.
/// Valid keys: haiku, sonnet, opus. Returns `None` on invalid input.
pub fn parse_model_map_input(input: &str) -> Option<ModelMap> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut mm = ModelMap::default();
    for pair in trimmed.split(',') {
        let pair = pair.trim();
        let (key, value) = pair.split_once('=')?;
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        match key {
            "haiku" => mm.haiku = Some(value.to_string()),
            "sonnet" => mm.sonnet = Some(value.to_string()),
            "opus" => mm.opus = Some(value.to_string()),
            _ => return None,
        }
    }
    if mm.has_any() {
        Some(mm)
    } else {
        None
    }
}

/// Format a `ModelMap` as a comma-separated `key=value` string for pre-filling input.
pub fn format_model_map(mm: Option<&ModelMap>) -> String {
    let Some(mm) = mm else {
        return String::new();
    };
    [
        ("haiku", &mm.haiku),
        ("sonnet", &mm.sonnet),
        ("opus", &mm.opus),
    ]
    .into_iter()
    .filter_map(|(key, val)| val.as_deref().map(|v| format!("{key}={v}")))
    .collect::<Vec<_>>()
    .join(",")
}
