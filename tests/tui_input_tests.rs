use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use zone_router::tui::app::{FocusPanel, InputMode, TuiState};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn make_tui_and_state() -> (
    TuiState,
    std::sync::Arc<tokio::sync::RwLock<zone_router::state::AppState>>,
    tokio::runtime::Runtime,
    tempfile::TempDir,
) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![
            zone_router::config::Backend {
                name: "a".into(),
                url: "http://a".into(),
                token: "ta".into(),
                active: true,
                auth_type: zone_router::config::AuthType::default(),
                model_map: None,
            },
            zone_router::config::Backend {
                name: "b".into(),
                url: "http://b".into(),
                token: "tb".into(),
                active: false,
                auth_type: zone_router::config::AuthType::default(),
                model_map: None,
            },
            zone_router::config::Backend {
                name: "c".into(),
                url: "http://c".into(),
                token: "tc".into(),
                active: false,
                auth_type: zone_router::config::AuthType::default(),
                model_map: None,
            },
        ],
    };
    let state = std::sync::Arc::new(tokio::sync::RwLock::new(
        zone_router::state::AppState::new(config, dir.path().join("tui-test.toml")).unwrap(),
    ));
    (TuiState::default(), state, rt, dir)
}

#[test]
fn tab_switches_focus() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    assert_eq!(tui.focus, FocusPanel::Backends);

    zone_router::tui::input::handle_input(key(KeyCode::Tab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::RequestLog);

    zone_router::tui::input::handle_input(key(KeyCode::Tab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::InternalLog);

    zone_router::tui::input::handle_input(key(KeyCode::Tab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::Backends);
}

#[test]
fn backtab_switches_focus() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    zone_router::tui::input::handle_input(key(KeyCode::BackTab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::InternalLog);

    zone_router::tui::input::handle_input(key(KeyCode::BackTab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::RequestLog);

    zone_router::tui::input::handle_input(key(KeyCode::BackTab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::Backends);
}

#[test]
fn j_k_navigate_backends() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    assert_eq!(tui.cursor, 0);

    zone_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
    assert_eq!(tui.cursor, 1);

    zone_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
    assert_eq!(tui.cursor, 2);

    // j at end stays at end
    zone_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
    assert_eq!(tui.cursor, 2);

    zone_router::tui::input::handle_input(key(KeyCode::Char('k')), &mut tui, &state, rt.handle());
    assert_eq!(tui.cursor, 1);
}

#[test]
fn big_g_goes_to_end() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    zone_router::tui::input::handle_input(key(KeyCode::Char('G')), &mut tui, &state, rt.handle());
    assert_eq!(tui.cursor, 2);
}

#[test]
fn gg_goes_to_start() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    tui.cursor = 2;
    zone_router::tui::input::handle_input(key(KeyCode::Char('g')), &mut tui, &state, rt.handle());
    zone_router::tui::input::handle_input(key(KeyCode::Char('g')), &mut tui, &state, rt.handle());
    assert_eq!(tui.cursor, 0);
}

#[test]
fn enter_switches_active_backend() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    tui.cursor = 1;
    zone_router::tui::input::handle_input(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    let s = rt.block_on(state.read());
    assert_eq!(s.active_index, 1);
}

#[test]
fn number_keys_switch_backend() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    zone_router::tui::input::handle_input(key(KeyCode::Char('2')), &mut tui, &state, rt.handle());
    let s = rt.block_on(state.read());
    assert_eq!(s.active_index, 1);
    assert_eq!(tui.cursor, 1);
}

#[test]
fn a_enters_add_mode() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    zone_router::tui::input::handle_input(key(KeyCode::Char('a')), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddName);
}

#[test]
fn e_enters_edit_mode() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    zone_router::tui::input::handle_input(key(KeyCode::Char('e')), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditName);
}

#[test]
fn d_deletes_backend() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    tui.cursor = 2;
    zone_router::tui::input::handle_input(key(KeyCode::Char('d')), &mut tui, &state, rt.handle());
    let s = rt.block_on(state.read());
    assert_eq!(s.config.backends.len(), 2);
}

#[test]
fn t_shows_token() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    zone_router::tui::input::handle_input(key(KeyCode::Char('t')), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::ShowToken);
}

#[test]
fn slash_enters_search() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    zone_router::tui::input::handle_input(key(KeyCode::Char('/')), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Search);
}

#[test]
fn normal_keys_ignored_in_input_mode() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    tui.mode = InputMode::AddName;
    // 'j' in input mode should type 'j', not navigate
    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('j')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(tui.input_buffer, "j");
    assert_eq!(tui.cursor, 0); // cursor unchanged
}

#[test]
fn q_exits_tui() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let should_exit = zone_router::tui::input::handle_input(
        key(KeyCode::Char('q')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert!(should_exit);
}

#[test]
fn add_flow_advances_to_auth_type_after_token() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Start add flow
    zone_router::tui::input::handle_input(key(KeyCode::Char('a')), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddName);

    // Enter name
    for c in "new-backend".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddUrl);

    // Enter URL
    for c in "http://new".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddToken);

    // Enter token
    for c in "tok-new".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // Must now be in AddAuthType — not Normal
    assert_eq!(tui.mode, InputMode::AddAuthType);
}

#[test]
fn add_flow_defaults_to_api_key_on_empty_input() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Fast-forward to AddAuthType
    tui.mode = InputMode::AddAuthType;
    tui.pending_name = "defaulted".into();
    tui.pending_url = "http://d".into();
    tui.pending_token = "td".into();

    // Submit empty input → should default to ApiKey and complete
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);

    let s = rt.block_on(state.read());
    assert_eq!(s.config.backends.len(), initial_count + 1);
    let added = s.config.backends.last().unwrap();
    assert_eq!(added.name, "defaulted");
    assert_eq!(added.auth_type, zone_router::config::AuthType::ApiKey);
}

#[test]
fn add_flow_cannot_skip_auth_type_step() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Start add flow and go through name, url, token
    zone_router::tui::input::handle_input(key(KeyCode::Char('a')), &mut tui, &state, rt.handle());

    // Name
    for c in "skip-test".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // URL
    for c in "http://skip".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // Token — after submitting, mode should be AddAuthType, NOT Normal
    for c in "tok".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // Backend should NOT have been created yet
    assert_eq!(tui.mode, InputMode::AddAuthType);
    let count = rt.block_on(state.read()).config.backends.len();
    assert_eq!(
        count, initial_count,
        "backend must not be created before auth type step"
    );
}

#[test]
fn edit_flow_can_change_auth_type_to_bearer() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Verify initial auth type is ApiKey
    {
        let s = rt.block_on(state.read());
        assert_eq!(
            s.config.backends[0].auth_type,
            zone_router::config::AuthType::ApiKey
        );
    }

    // Start edit flow on backend 0
    zone_router::tui::input::handle_input(key(KeyCode::Char('e')), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditName);

    // Accept current name
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditUrl);

    // Accept current URL
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditToken);

    // Accept current token
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditAuthType);

    // Clear the pre-filled value and type "bearer"
    // First clear out the pre-filled "api-key" text
    for _ in 0..tui.input_buffer.len() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Backspace),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    for c in "bearer".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);

    let s = rt.block_on(state.read());
    assert_eq!(
        s.config.backends[0].auth_type,
        zone_router::config::AuthType::Bearer,
        "auth type should have changed to Bearer"
    );
}

#[test]
fn invalid_auth_type_input_stays_in_mode_add() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Fast-forward to AddAuthType
    tui.mode = InputMode::AddAuthType;
    tui.pending_name = "invalid-test".into();
    tui.pending_url = "http://inv".into();
    tui.pending_token = "tinv".into();

    // Type invalid input
    for c in "wrong".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // Should still be in AddAuthType, backend NOT created
    assert_eq!(tui.mode, InputMode::AddAuthType);
    let count = rt.block_on(state.read()).config.backends.len();
    assert_eq!(
        count, initial_count,
        "invalid auth type should not create backend"
    );
}

#[test]
fn invalid_auth_type_input_stays_in_mode_edit() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Fast-forward to EditAuthType
    tui.mode = InputMode::EditAuthType;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();

    // Remember original auth type
    let original_auth_type = {
        let s = rt.block_on(state.read());
        s.config.backends[0].auth_type
    };

    // Type invalid input
    for c in "xyz".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // Should still be in EditAuthType, backend NOT modified
    assert_eq!(tui.mode, InputMode::EditAuthType);
    let s = rt.block_on(state.read());
    assert_eq!(
        s.config.backends[0].auth_type, original_auth_type,
        "invalid auth type should not change backend"
    );
}

#[test]
fn empty_enter_after_rejected_auth_type_stays_in_mode_add() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Fast-forward to AddAuthType
    tui.mode = InputMode::AddAuthType;
    tui.pending_name = "reject-test".into();
    tui.pending_url = "http://rej".into();
    tui.pending_token = "trej".into();

    // Type invalid input and press Enter (rejected)
    for c in "garbage".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddAuthType);

    // Now press Enter again with empty buffer — must NOT silently default to api-key
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    assert_eq!(
        tui.mode,
        InputMode::AddAuthType,
        "empty Enter after rejection must not silently default to api-key"
    );
    let count = rt.block_on(state.read()).config.backends.len();
    assert_eq!(
        count, initial_count,
        "backend must not be created by empty Enter after rejection"
    );
}

#[test]
fn empty_enter_after_rejected_auth_type_stays_in_mode_edit() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    tui.mode = InputMode::EditAuthType;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();

    let original_auth_type = {
        let s = rt.block_on(state.read());
        s.config.backends[0].auth_type
    };

    // Type invalid input and press Enter (rejected)
    for c in "nope".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditAuthType);

    // Now press Enter again with empty buffer — must NOT silently default to api-key
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    assert_eq!(
        tui.mode,
        InputMode::EditAuthType,
        "empty Enter after rejection must not silently default to api-key"
    );
    let s = rt.block_on(state.read());
    assert_eq!(
        s.config.backends[0].auth_type, original_auth_type,
        "backend auth type must not change from empty Enter after rejection"
    );
}

// --- Model map TUI input parsing tests ---

#[test]
fn parse_model_map_valid_full() {
    let result = zone_router::tui::input::parse_model_map_input(
        "haiku=glm-4.5-air,sonnet=glm-5-turbo,opus=glm-5.1",
    );
    let mm = result.unwrap();
    assert_eq!(mm.haiku.as_deref(), Some("glm-4.5-air"));
    assert_eq!(mm.sonnet.as_deref(), Some("glm-5-turbo"));
    assert_eq!(mm.opus.as_deref(), Some("glm-5.1"));
}

#[test]
fn parse_model_map_partial() {
    let result = zone_router::tui::input::parse_model_map_input("sonnet=glm-5-turbo");
    let mm = result.unwrap();
    assert!(mm.haiku.is_none());
    assert_eq!(mm.sonnet.as_deref(), Some("glm-5-turbo"));
    assert!(mm.opus.is_none());
}

#[test]
fn parse_model_map_empty_returns_none() {
    assert!(zone_router::tui::input::parse_model_map_input("").is_none());
    assert!(zone_router::tui::input::parse_model_map_input("  ").is_none());
}

#[test]
fn parse_model_map_invalid_key_returns_none() {
    assert!(zone_router::tui::input::parse_model_map_input("foo=bar").is_none());
}

#[test]
fn parse_model_map_invalid_format_returns_none() {
    assert!(zone_router::tui::input::parse_model_map_input("haiku:x").is_none());
}

#[test]
fn add_flow_with_model_map() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Fast-forward to AddModelMap
    tui.mode = InputMode::AddModelMap;
    tui.pending_name = "mapped".into();
    tui.pending_url = "http://m".into();
    tui.pending_token = "tm".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Type model map input
    for c in "sonnet=glm-5-turbo,opus=glm-5.1".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);

    let s = rt.block_on(state.read());
    assert_eq!(s.config.backends.len(), initial_count + 1);
    let added = s.config.backends.last().unwrap();
    assert_eq!(added.name, "mapped");
    let mm = added.model_map.as_ref().unwrap();
    assert_eq!(mm.sonnet.as_deref(), Some("glm-5-turbo"));
    assert_eq!(mm.opus.as_deref(), Some("glm-5.1"));
    assert!(mm.haiku.is_none());
}

#[test]
fn add_flow_skip_model_map() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    tui.mode = InputMode::AddModelMap;
    tui.pending_name = "plain".into();
    tui.pending_url = "http://p".into();
    tui.pending_token = "tp".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Press Enter on empty input → skip
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);

    let s = rt.block_on(state.read());
    assert_eq!(s.config.backends.len(), initial_count + 1);
    assert!(s.config.backends.last().unwrap().model_map.is_none());
}

#[test]
fn edit_flow_clears_model_map_on_empty() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Set a model_map on backend 0
    {
        let mut s = rt.block_on(state.write());
        s.config.backends[0].model_map = Some(zone_router::config::ModelMap {
            haiku: None,
            sonnet: Some("s".into()),
            opus: None,
        });
    }

    // Fast-forward to EditModelMap
    tui.mode = InputMode::EditModelMap;
    tui.cursor = 0;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());
    tui.input_buffer.clear();

    // Submit empty → clears model_map
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);

    let s = rt.block_on(state.read());
    assert!(s.config.backends[0].model_map.is_none());
}

#[test]
fn edit_flow_prefills_model_map_input_buffer() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Set a model_map on backend 0
    {
        let mut s = rt.block_on(state.write());
        s.config.backends[0].model_map = Some(zone_router::config::ModelMap {
            haiku: Some("h-model".into()),
            sonnet: None,
            opus: Some("o-model".into()),
        });
    }

    // Walk through edit flow to reach EditModelMap
    tui.cursor = 0;
    zone_router::tui::input::handle_input(key(KeyCode::Char('e')), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditName);

    // Accept name
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    // Accept URL
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    // Accept token
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditAuthType);

    // Tab to continue to model map editor (opt-in)
    zone_router::tui::input::handle_input_mode(key(KeyCode::Tab), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditModelMap);

    // Verify input_buffer is pre-filled with existing model_map
    assert!(
        tui.input_buffer.contains("haiku=h-model"),
        "should pre-fill haiku mapping, got: {}",
        tui.input_buffer
    );
    assert!(
        tui.input_buffer.contains("opus=o-model"),
        "should pre-fill opus mapping, got: {}",
        tui.input_buffer
    );
    assert!(
        !tui.input_buffer.contains("sonnet"),
        "should not pre-fill unmapped sonnet, got: {}",
        tui.input_buffer
    );
}

#[test]
fn add_model_map_invalid_input_stays_in_mode_then_retry_succeeds() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Fast-forward to AddModelMap
    tui.mode = InputMode::AddModelMap;
    tui.pending_name = "retry".into();
    tui.pending_url = "http://r".into();
    tui.pending_token = "tr".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Type invalid input (bad format: colon instead of equals)
    for c in "haiku:bad".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // Should stay in AddModelMap mode
    assert_eq!(tui.mode, InputMode::AddModelMap);
    // Backend should not have been added
    assert_eq!(
        rt.block_on(state.read()).config.backends.len(),
        initial_count
    );

    // Now type valid input and retry
    for c in "sonnet=glm-5".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    assert_eq!(tui.mode, InputMode::Normal);
    let s = rt.block_on(state.read());
    assert_eq!(s.config.backends.len(), initial_count + 1);
    let added = s.config.backends.last().unwrap();
    assert_eq!(added.name, "retry");
    assert_eq!(
        added.model_map.as_ref().unwrap().sonnet.as_deref(),
        Some("glm-5")
    );
}

#[test]
fn edit_model_map_invalid_input_stays_in_mode_then_retry_succeeds() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Fast-forward to EditModelMap
    tui.mode = InputMode::EditModelMap;
    tui.cursor = 0;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    let original_name = rt.block_on(state.read()).config.backends[0].name.clone();

    // Type invalid input (unknown key)
    for c in "foo=bar".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    // Should stay in EditModelMap mode
    assert_eq!(tui.mode, InputMode::EditModelMap);
    // Backend should be unchanged
    assert_eq!(
        rt.block_on(state.read()).config.backends[0].name,
        original_name
    );

    // Now type valid input and retry
    for c in "opus=o-model".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());

    assert_eq!(tui.mode, InputMode::Normal);
    let s = rt.block_on(state.read());
    assert_eq!(
        s.config.backends[0]
            .model_map
            .as_ref()
            .unwrap()
            .opus
            .as_deref(),
        Some("o-model")
    );
}

#[test]
fn add_flow_tab_continues_to_model_map() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Fast-forward to AddAuthType
    tui.mode = InputMode::AddAuthType;
    tui.pending_name = "tab-test".into();
    tui.pending_url = "http://t".into();
    tui.pending_token = "tt".into();

    // Tab → should continue to AddModelMap instead of saving
    zone_router::tui::input::handle_input_mode(key(KeyCode::Tab), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddModelMap);
    // Backend should NOT have been added yet
    assert_eq!(
        rt.block_on(state.read()).config.backends.len(),
        initial_count
    );

    // Type model map and submit
    for c in "sonnet=glm-5".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);

    let s = rt.block_on(state.read());
    assert_eq!(s.config.backends.len(), initial_count + 1);
    let added = s.config.backends.last().unwrap();
    assert_eq!(added.name, "tab-test");
    assert_eq!(
        added.model_map.as_ref().unwrap().sonnet.as_deref(),
        Some("glm-5")
    );
}

#[test]
fn edit_flow_tab_continues_to_model_map() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Fast-forward to EditAuthType
    tui.mode = InputMode::EditAuthType;
    tui.cursor = 0;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Tab → should continue to EditModelMap
    zone_router::tui::input::handle_input_mode(key(KeyCode::Tab), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditModelMap);

    // Submit with model map
    for c in "opus=o-model".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);

    let s = rt.block_on(state.read());
    assert_eq!(
        s.config.backends[0]
            .model_map
            .as_ref()
            .unwrap()
            .opus
            .as_deref(),
        Some("o-model")
    );
}

#[test]
fn backend_list_shows_m_marker_when_model_map_present() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![
            zone_router::config::Backend {
                name: "mapped".into(),
                url: "http://m".into(),
                token: "tm".into(),
                active: true,
                auth_type: zone_router::config::AuthType::default(),
                model_map: Some(zone_router::config::ModelMap {
                    haiku: None,
                    sonnet: Some("glm-5-turbo".into()),
                    opus: None,
                }),
            },
            zone_router::config::Backend {
                name: "plain".into(),
                url: "http://p".into(),
                token: "tp".into(),
                active: false,
                auth_type: zone_router::config::AuthType::default(),
                model_map: None,
            },
        ],
    };
    let state = std::sync::Arc::new(tokio::sync::RwLock::new(
        zone_router::state::AppState::new(config, dir.path().join("m-marker.toml")).unwrap(),
    ));
    let app_state = rt.block_on(state.read()).clone();
    let mut tui_state = TuiState::default();

    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| zone_router::tui::ui::draw(frame, &app_state, &mut tui_state))
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered.contains("[M]"),
        "backend with model_map should show [M] marker in rendered output"
    );
    // "plain" backend should NOT have [M] next to it
    // Find the line with "plain" and verify no [M] on that line
    for line in rendered.lines() {
        if line.contains("plain") {
            assert!(
                !line.contains("[M]"),
                "backend without model_map should not show [M], got: {line}"
            );
        }
    }
}

#[test]
fn edit_model_map_empty_enter_after_rejection_preserves_existing() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Set a model_map on backend 0
    {
        let mut s = rt.block_on(state.write());
        s.config.backends[0].model_map = Some(zone_router::config::ModelMap {
            haiku: None,
            sonnet: Some("existing-model".into()),
            opus: None,
        });
    }

    // Fast-forward to EditModelMap
    tui.mode = InputMode::EditModelMap;
    tui.cursor = 0;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Type invalid input
    for c in "haiku:bad".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditModelMap);

    // Press Enter again on empty buffer — should NOT wipe model_map
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(
        tui.mode,
        InputMode::EditModelMap,
        "empty Enter after rejection should stay in EditModelMap"
    );

    // Verify model_map is still intact
    let s = rt.block_on(state.read());
    assert_eq!(
        s.config.backends[0]
            .model_map
            .as_ref()
            .unwrap()
            .sonnet
            .as_deref(),
        Some("existing-model"),
        "existing model_map should not be wiped by empty Enter after rejection"
    );
}

#[test]
fn add_model_map_empty_enter_after_rejection_stays_in_mode() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Fast-forward to AddModelMap
    tui.mode = InputMode::AddModelMap;
    tui.pending_name = "post-reject".into();
    tui.pending_url = "http://pr".into();
    tui.pending_token = "tpr".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Type invalid input
    for c in "foo=bar".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddModelMap);

    // Press Enter again on empty buffer — should NOT create backend
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(
        tui.mode,
        InputMode::AddModelMap,
        "empty Enter after rejection should stay in AddModelMap"
    );
    assert_eq!(
        rt.block_on(state.read()).config.backends.len(),
        initial_count,
        "backend should not be created by empty Enter after rejection"
    );
}

#[test]
fn add_model_map_esc_after_rejection_resets_flag_and_allows_skip() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    let initial_count = rt.block_on(state.read()).config.backends.len();

    // Enter AddModelMap
    tui.mode = InputMode::AddModelMap;
    tui.pending_name = "esc-test".into();
    tui.pending_url = "http://esc".into();
    tui.pending_token = "tesc".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Type invalid input and trigger rejection
    for c in "bad:input".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::AddModelMap);
    assert!(tui.model_map_rejected);

    // Press Esc — should reset to Normal and clear the flag
    zone_router::tui::input::handle_input_mode(key(KeyCode::Esc), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);
    assert!(
        !tui.model_map_rejected,
        "Esc should reset model_map_rejected"
    );

    // Re-enter AddModelMap with same pending fields
    tui.mode = InputMode::AddModelMap;
    tui.pending_name = "esc-test".into();
    tui.pending_url = "http://esc".into();
    tui.pending_token = "tesc".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Empty Enter should skip model_map (create backend with None)
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(
        tui.mode,
        InputMode::Normal,
        "empty Enter after Esc reset should skip model_map"
    );
    assert_eq!(
        rt.block_on(state.read()).config.backends.len(),
        initial_count + 1,
        "backend should be created after Esc reset"
    );
    assert!(
        rt.block_on(state.read())
            .config
            .backends
            .last()
            .unwrap()
            .model_map
            .is_none(),
        "backend should have model_map: None when skipped"
    );
}

#[test]
fn edit_model_map_esc_after_rejection_resets_flag() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();

    // Seed a model_map on backend 0
    {
        let mut s = rt.block_on(state.write());
        s.config.backends[0].model_map = Some(zone_router::config::ModelMap {
            haiku: None,
            sonnet: Some("keep-me".into()),
            opus: None,
        });
    }

    // Enter EditModelMap
    tui.mode = InputMode::EditModelMap;
    tui.cursor = 0;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());

    // Type invalid input and trigger rejection
    for c in "haiku:bad".chars() {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
    }
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::EditModelMap);
    assert!(tui.model_map_rejected);

    // Press Esc — should reset to Normal and clear the flag
    zone_router::tui::input::handle_input_mode(key(KeyCode::Esc), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);
    assert!(
        !tui.model_map_rejected,
        "Esc should reset model_map_rejected"
    );

    // Verify model_map was not touched during rejection/Esc
    {
        let s = rt.block_on(state.read());
        assert_eq!(
            s.config.backends[0]
                .model_map
                .as_ref()
                .unwrap()
                .sonnet
                .as_deref(),
            Some("keep-me"),
            "model_map should be untouched after Esc"
        );
    }

    // Re-enter EditModelMap with empty input_buffer
    tui.mode = InputMode::EditModelMap;
    tui.cursor = 0;
    tui.pending_name = "a".into();
    tui.pending_url = "http://a".into();
    tui.pending_token = "ta".into();
    tui.pending_auth_type = Some(zone_router::config::AuthType::default());
    tui.input_buffer.clear();

    // Empty Enter should restore normal behavior (clear model_map)
    zone_router::tui::input::handle_input_mode(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(
        tui.mode,
        InputMode::Normal,
        "empty Enter after Esc reset should transition to Normal"
    );
    assert!(
        rt.block_on(state.read()).config.backends[0]
            .model_map
            .is_none(),
        "empty Enter after Esc reset should clear model_map"
    );
}

// --- Request Log cursor and DetailView tests ---

fn make_tui_with_log() -> (
    TuiState,
    std::sync::Arc<tokio::sync::RwLock<zone_router::state::AppState>>,
    tokio::runtime::Runtime,
    tempfile::TempDir,
) {
    let (mut tui, state, rt, dir) = make_tui_and_state();
    // Add some log entries
    {
        let mut s = rt.block_on(state.write());
        for i in 0..5 {
            s.stats.record(zone_router::stats::RequestLogEntry::new(
                chrono::Utc::now(),
                "test".into(),
                i * 10,
                "POST".into(),
                "/v1/messages".into(),
                200,
                Some("claude-sonnet-4-20250514".into()),
                zone_router::stats::TransferType::Json,
                None,
            ));
        }
    }
    tui.focus = FocusPanel::RequestLog;
    (tui, state, rt, dir)
}

#[test]
fn log_cursor_j_k_moves_within_bounds() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    assert_eq!(tui.log_cursor, 0);

    // j moves cursor down
    zone_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
    assert_eq!(tui.log_cursor, 1);

    zone_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
    assert_eq!(tui.log_cursor, 2);

    // k moves cursor up
    zone_router::tui::input::handle_input(key(KeyCode::Char('k')), &mut tui, &state, rt.handle());
    assert_eq!(tui.log_cursor, 1);

    // k at 0 stays at 0
    tui.log_cursor = 0;
    tui.log_cursor_id = None;
    zone_router::tui::input::handle_input(key(KeyCode::Char('k')), &mut tui, &state, rt.handle());
    assert_eq!(tui.log_cursor, 0);

    // j at end stays at end
    tui.log_cursor = 4;
    tui.log_cursor_id = None;
    zone_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
    assert_eq!(tui.log_cursor, 4);
}

#[test]
fn enter_opens_detail_view_from_request_log() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    assert_eq!(tui.mode, InputMode::Normal);

    // Enter on RequestLog focus opens DetailView
    zone_router::tui::input::handle_input(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::DetailView);
    assert_eq!(tui.detail_scroll, 0);
}

#[test]
fn enter_on_backends_does_not_open_detail_view() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    tui.focus = FocusPanel::Backends;

    zone_router::tui::input::handle_input(key(KeyCode::Enter), &mut tui, &state, rt.handle());
    assert_ne!(
        tui.mode,
        InputMode::DetailView,
        "Enter on Backends should not open DetailView"
    );
}

#[test]
fn detail_view_j_k_scrolls() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    tui.mode = InputMode::DetailView;
    assert_eq!(tui.detail_scroll, 0);

    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('j')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(tui.detail_scroll, 1);

    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('k')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(tui.detail_scroll, 0);

    // k at 0 stays at 0
    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('k')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(tui.detail_scroll, 0);
}

#[test]
fn detail_view_n_p_cycles_entries() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    tui.mode = InputMode::DetailView;
    tui.log_cursor = 0;

    // Get the newest entry's ID (5 entries, newest is at deque index 4)
    let newest_id = {
        let s = rt.block_on(state.read());
        s.stats.log.back().unwrap().id
    };
    tui.detail_entry_id = Some(newest_id);

    // n moves to next older entry
    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('n')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_ne!(
        tui.detail_entry_id,
        Some(newest_id),
        "n should change entry"
    );
    assert_eq!(tui.detail_scroll, 0, "n should reset scroll");
    let _older_id = tui.detail_entry_id.unwrap();

    // p moves back to newer entry
    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('p')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(
        tui.detail_entry_id,
        Some(newest_id),
        "p should return to newest"
    );

    // p at newest stays at newest
    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('p')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(tui.detail_entry_id, Some(newest_id), "p at newest stays");

    // Set to oldest entry and verify n stays there
    let oldest_id = {
        let s = rt.block_on(state.read());
        s.stats.log.front().unwrap().id
    };
    tui.detail_entry_id = Some(oldest_id);
    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('n')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(
        tui.detail_entry_id,
        Some(oldest_id),
        "n at oldest stays at oldest"
    );
}

#[test]
fn detail_view_esc_closes_panel() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    tui.mode = InputMode::DetailView;

    zone_router::tui::input::handle_input_mode(key(KeyCode::Esc), &mut tui, &state, rt.handle());
    assert_eq!(tui.mode, InputMode::Normal);
}

#[test]
fn detail_view_h_closes_panel() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    tui.mode = InputMode::DetailView;

    zone_router::tui::input::handle_input_mode(
        key(KeyCode::Char('h')),
        &mut tui,
        &state,
        rt.handle(),
    );
    assert_eq!(tui.mode, InputMode::Normal);
}

#[test]
fn detail_view_blocks_normal_mode_keys() {
    let (mut tui, state, rt, _dir) = make_tui_with_log();
    tui.mode = InputMode::DetailView;

    // Normal mode keys should have no effect in DetailView
    for c in ['a', 'd', 'e', 't', 'q', '1', '2'] {
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char(c)),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(
            tui.mode,
            InputMode::DetailView,
            "key '{c}' should not change mode in DetailView"
        );
    }
}

#[test]
fn arrow_down_scrolls_internal_log() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    tui.focus = FocusPanel::InternalLog;

    // Push entries directly into internal_log for test purposes
    for i in 0..5 {
        tui.internal_log.push_back(zone_router::logging::LogEntry {
            id: i,
            timestamp: chrono::Local::now(),
            level: tracing::Level::INFO,
            target: format!("zone_router::test::{i}"),
            message: format!("entry {i}"),
        });
    }
    assert_eq!(tui.internal_log_cursor, 0);

    zone_router::tui::input::handle_input(key(KeyCode::Down), &mut tui, &state, rt.handle());
    assert_eq!(tui.internal_log_cursor, 1);

    zone_router::tui::input::handle_input(key(KeyCode::Down), &mut tui, &state, rt.handle());
    assert_eq!(tui.internal_log_cursor, 2);
}

#[test]
fn arrow_up_scrolls_internal_log() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    tui.focus = FocusPanel::InternalLog;

    for i in 0..5 {
        tui.internal_log.push_back(zone_router::logging::LogEntry {
            id: i,
            timestamp: chrono::Local::now(),
            level: tracing::Level::INFO,
            target: format!("zone_router::test::{i}"),
            message: format!("entry {i}"),
        });
    }
    tui.internal_log_cursor = 3;

    zone_router::tui::input::handle_input(key(KeyCode::Up), &mut tui, &state, rt.handle());
    assert_eq!(tui.internal_log_cursor, 2);

    zone_router::tui::input::handle_input(key(KeyCode::Up), &mut tui, &state, rt.handle());
    assert_eq!(tui.internal_log_cursor, 1);
}

#[test]
fn unread_resets_on_blur() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    // Start on Backends, add some entries, switch to InternalLog
    tui.focus = FocusPanel::InternalLog;
    tui.internal_log_unread = 5;

    // Tab away from InternalLog should reset unread
    zone_router::tui::input::handle_input(key(KeyCode::Tab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::Backends);
    assert_eq!(tui.internal_log_unread, 0);
}

#[test]
fn backtab_resets_unread_on_blur() {
    let (mut tui, state, rt, _dir) = make_tui_and_state();
    tui.focus = FocusPanel::InternalLog;
    tui.internal_log_unread = 3;

    zone_router::tui::input::handle_input(key(KeyCode::BackTab), &mut tui, &state, rt.handle());
    assert_eq!(tui.focus, FocusPanel::RequestLog);
    assert_eq!(tui.internal_log_unread, 0);
}
