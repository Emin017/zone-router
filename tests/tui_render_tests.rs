use zone_router::tui::app::{FocusPanel, InputMode, TuiState};

fn make_app_state_with_log(
    dir: &tempfile::TempDir,
) -> std::sync::Arc<tokio::sync::RwLock<zone_router::state::AppState>> {
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:8080".into(),
            local_token: "sk-local-test".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "openai".into(),
            url: "http://openai".into(),
            token: "tok".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    let mut app =
        zone_router::state::AppState::new(config, dir.path().join("render.toml")).unwrap();
    app.stats.record(zone_router::stats::RequestLogEntry::new(
        chrono::Utc::now(),
        "openai".into(),
        245,
        zone_router::stats::CapturedRequest {
            method: "POST".into(),
            path: "/v1/messages".into(),
            headers: zone_router::stats::HeaderPairs(vec![(
                "content-type".into(),
                "application/json".into(),
            )]),
            body: Some(r#"{"model":"claude"}"#.into()),
        },
        zone_router::stats::CapturedResponse {
            status: 200,
            headers: zone_router::stats::HeaderPairs(vec![(
                "content-type".into(),
                "application/json".into(),
            )]),
            body: Some(r#"{"id":"msg_123","type":"message"}"#.into()),
        },
    ));
    std::sync::Arc::new(tokio::sync::RwLock::new(app))
}

fn render_to_string(
    state: &zone_router::state::AppState,
    tui: &TuiState,
    width: u16,
    height: u16,
) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| zone_router::tui::ui::draw(frame, state, tui))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            output.push_str(cell.symbol());
        }
        output.push('\n');
    }
    output
}

#[test]
fn render_normal_mode_shows_request_log_and_help_bar() {
    let dir = tempfile::tempdir().unwrap();
    let state_arc = make_app_state_with_log(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(state_arc.read()).clone();
    let tui = TuiState {
        focus: FocusPanel::RequestLog,
        ..TuiState::default()
    };

    let output = render_to_string(&state, &tui, 100, 30);

    assert!(
        output.contains("Request Log"),
        "should show Request Log title"
    );
    assert!(output.contains("POST"), "should show request method");
    assert!(output.contains("/v1/messages"), "should show request path");
    assert!(
        output.contains("[1-9] switch"),
        "Normal help bar should show switch key"
    );
    assert!(
        output.contains("[q] quit"),
        "Normal help bar should show quit key"
    );
    assert!(
        !output.contains("Request Detail"),
        "should NOT show detail panel in Normal mode"
    );
}

#[test]
fn render_detail_view_shows_floating_panel_with_content() {
    let dir = tempfile::tempdir().unwrap();
    let state_arc = make_app_state_with_log(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(state_arc.read()).clone();
    let tui = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        ..TuiState::default()
    };

    let output = render_to_string(&state, &tui, 100, 50);

    // Panel title
    assert!(
        output.contains("Request Detail"),
        "should show detail panel title, got:\n{output}"
    );

    // Underlying log should still be partially visible
    assert!(
        output.contains("Request Log"),
        "log title should be visible around panel edges"
    );

    // Panel content sections
    assert!(
        output.contains("Timestamp:"),
        "panel should display Timestamp field"
    );
    assert!(
        output.contains("Backend:"),
        "panel should display Backend field"
    );
    assert!(
        output.contains("openai"),
        "panel should display backend name"
    );
    assert!(
        output.contains("Method:"),
        "panel should display Method field"
    );
    assert!(output.contains("POST"), "panel should display method value");
    assert!(output.contains("/v1/messages"), "panel should display path");
    assert!(
        output.contains("Status:"),
        "panel should display Status field"
    );
    assert!(output.contains("200"), "panel should display status value");
    assert!(
        output.contains("Latency:"),
        "panel should display Latency field"
    );
    assert!(
        output.contains("Request Headers"),
        "panel should display Request Headers section"
    );
    assert!(
        output.contains("content-type"),
        "panel should display captured header"
    );
    assert!(
        output.contains("Request Body"),
        "panel should display Request Body section"
    );
    assert!(
        output.contains("Response Headers"),
        "panel should display Response Headers section"
    );
    assert!(
        output.contains("Response Body"),
        "panel should display Response Body section"
    );
}

#[test]
fn render_detail_view_help_bar_shows_detail_keys() {
    let dir = tempfile::tempdir().unwrap();
    let state_arc = make_app_state_with_log(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(state_arc.read()).clone();
    let tui = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        ..TuiState::default()
    };

    let output = render_to_string(&state, &tui, 100, 30);

    // Help bar should show DetailView keys
    assert!(
        output.contains("[j/k] scroll"),
        "DetailView help bar should show scroll keys"
    );
    assert!(
        output.contains("[Esc/h] close"),
        "DetailView help bar should show close keys"
    );
    assert!(
        output.contains("[n/p] next/prev"),
        "DetailView help bar should show next/prev keys"
    );
    // Should NOT show Normal mode keys
    assert!(
        !output.contains("[q] quit"),
        "DetailView help bar should NOT show Normal quit key"
    );
    assert!(
        !output.contains("[a] add"),
        "DetailView help bar should NOT show Normal add key"
    );
}

#[test]
fn render_help_bar_restores_normal_keys_after_close() {
    let dir = tempfile::tempdir().unwrap();
    let state_arc = make_app_state_with_log(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(state_arc.read()).clone();

    // First verify DetailView
    let tui_detail = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        ..TuiState::default()
    };
    let output_detail = render_to_string(&state, &tui_detail, 100, 30);
    assert!(output_detail.contains("[Esc/h] close"));

    // Now verify Normal after closing
    let tui_normal = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::Normal,
        log_cursor: 0,
        ..TuiState::default()
    };
    let output_normal = render_to_string(&state, &tui_normal, 100, 30);
    assert!(
        output_normal.contains("[q] quit"),
        "Normal help bar should be restored after close"
    );
    assert!(
        !output_normal.contains("[Esc/h] close"),
        "DetailView keys should be gone after close"
    );
}

#[test]
fn render_request_body_collapsed_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let state_arc = make_app_state_with_log(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(state_arc.read()).clone();

    // Collapsed (default)
    let tui_collapsed = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        body_expanded: false,
        ..TuiState::default()
    };
    let output = render_to_string(&state, &tui_collapsed, 100, 40);
    assert!(
        output.contains("▸ Request Body"),
        "collapsed body should show ▸ marker"
    );
    assert!(
        output.contains("bytes"),
        "collapsed body should show byte count"
    );

    // Expanded
    let tui_expanded = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        body_expanded: true,
        ..TuiState::default()
    };
    let output = render_to_string(&state, &tui_expanded, 100, 40);
    assert!(
        output.contains("▾ Request Body"),
        "expanded body should show ▾ marker"
    );
    assert!(
        output.contains("claude"),
        "expanded body should show body content"
    );
}

#[test]
fn render_non_ascii_body_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "test".into(),
            url: "http://test".into(),
            token: "t".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    let mut app =
        zone_router::state::AppState::new(config, dir.path().join("unicode.toml")).unwrap();
    app.stats.record(zone_router::stats::RequestLogEntry::new(
        chrono::Utc::now(),
        "test".into(),
        10,
        zone_router::stats::CapturedRequest {
            method: "POST".into(),
            path: "/api".into(),
            headers: zone_router::stats::HeaderPairs(vec![(
                "x-custom".into(),
                "日本語ヘッダー".into(),
            )]),
            body: Some("こんにちは世界🌍".into()),
        },
        zone_router::stats::CapturedResponse {
            status: 200,
            headers: zone_router::stats::HeaderPairs::default(),
            body: Some("Ÿéponse avéc dés àccents et emoji 🚀✨".into()),
        },
    ));

    let tui = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        body_expanded: true,
        ..TuiState::default()
    };

    // This should not panic even with multibyte characters
    let output = render_to_string(&app, &tui, 60, 40);
    assert!(
        output.contains("Request Detail"),
        "panel should render with non-ASCII content"
    );
}

#[test]
fn render_popup_geometry_is_centered_in_log_area() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let dir = tempfile::tempdir().unwrap();
    let state_arc = make_app_state_with_log(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(state_arc.read()).clone();
    let tui = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        ..TuiState::default()
    };

    let width: u16 = 100;
    let height: u16 = 50;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| zone_router::tui::ui::draw(frame, &state, &tui))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();

    // Find the popup by scanning cell symbols directly for border corners.
    let mut panel_title_y = None;
    let mut panel_left: Option<u16> = None;
    let mut panel_right: Option<u16> = None;
    for y in 0..buffer.area.height {
        let mut row = String::new();
        for x in 0..buffer.area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        if row.contains("Request Detail") {
            panel_title_y = Some(y);
            for x in 0..buffer.area.width {
                let sym = buffer[(x, y)].symbol();
                if sym == "┌" || sym == "╭" {
                    panel_left = Some(x);
                }
                if sym == "┐" || sym == "╮" {
                    panel_right = Some(x);
                }
            }
            break;
        }
    }

    let panel_y = panel_title_y.expect("panel title should be found in buffer");

    // Panel should be inside the log area, not at the top of the frame
    assert!(
        panel_y > 5,
        "panel top edge should be below status+main area, got y={panel_y}"
    );

    // Verify panel width and centering using cell coordinates
    let left = panel_left.expect("left border should be found");
    let right = panel_right.expect("right border should be found");
    let panel_width = right - left + 1;
    assert!(
        panel_width >= 60,
        "panel should be at least 60 cols wide (~80% of 100), got {panel_width}"
    );
    let left_margin = left as i32;
    let right_margin = (width - right - 1) as i32;
    let margin_diff = (left_margin - right_margin).unsigned_abs();
    assert!(
        margin_diff <= 5,
        "panel should be roughly centered, left_margin={left_margin}, right_margin={right_margin}"
    );
}

#[test]
fn render_scroll_reachability_for_long_body() {
    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "test".into(),
            url: "http://test".into(),
            token: "t".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    let mut app =
        zone_router::state::AppState::new(config, dir.path().join("scroll.toml")).unwrap();

    // Create a long response body with a distinctive tail marker
    let mut long_body = String::new();
    for i in 0..100 {
        long_body.push_str(&format!("line-{i}: some content here\n"));
    }
    long_body.push_str("TAIL_MARKER_END_OF_CONTENT");

    app.stats.record(zone_router::stats::RequestLogEntry::new(
        chrono::Utc::now(),
        "test".into(),
        10,
        zone_router::stats::CapturedRequest {
            method: "GET".into(),
            path: "/api".into(),
            headers: zone_router::stats::HeaderPairs::default(),
            body: None,
        },
        zone_router::stats::CapturedResponse {
            status: 200,
            headers: zone_router::stats::HeaderPairs::default(),
            body: Some(long_body),
        },
    ));

    let width: u16 = 100;
    let height: u16 = 40;

    // At scroll=0, the tail marker should NOT be visible
    let tui_top = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        detail_scroll: 0,
        ..TuiState::default()
    };
    let output_top = render_to_string(&app, &tui_top, width, height);
    assert!(
        !output_top.contains("TAIL_MARKER"),
        "tail should NOT be visible at scroll=0"
    );

    // At a large scroll value, the tail marker SHOULD be visible
    let tui_bottom = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        detail_scroll: 200, // intentionally over-large; should clamp
        ..TuiState::default()
    };
    let output_bottom = render_to_string(&app, &tui_bottom, width, height);
    assert!(
        output_bottom.contains("TAIL_MARKER"),
        "tail should be visible when scrolled to bottom"
    );

    // Verify scroll clamping: scrolling beyond content should still show the tail
    let tui_clamped = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        detail_scroll: 9999,
        ..TuiState::default()
    };
    let output_clamped = render_to_string(&app, &tui_clamped, width, height);
    assert!(
        output_clamped.contains("TAIL_MARKER"),
        "over-scrolling should clamp and still show tail content"
    );
}

#[test]
fn render_popup_exact_centered_geometry() {
    use ratatui::backend::TestBackend;
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::Terminal;

    let dir = tempfile::tempdir().unwrap();
    let state_arc = make_app_state_with_log(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(state_arc.read()).clone();
    let tui = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        ..TuiState::default()
    };

    let width: u16 = 100;
    let height: u16 = 50;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| zone_router::tui::ui::draw(frame, &state, &tui))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();

    // Compute the expected Request Log area using the same layout constraints
    let frame_area = ratatui::layout::Rect::new(0, 0, width, height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame_area);
    let log_area = chunks[2];

    // Compute exact centered popup rect (~80% width, ~90% height of log area)
    let popup_w = log_area.width * 80 / 100;
    let popup_h = log_area.height * 90 / 100;
    let popup_x = log_area.x + (log_area.width.saturating_sub(popup_w)) / 2;
    let popup_y = log_area.y + (log_area.height.saturating_sub(popup_h)) / 2;

    // Assert top-left border corner
    let tl_sym = buffer[(popup_x, popup_y)].symbol();
    assert_eq!(
        tl_sym, "┌",
        "top-left corner at ({popup_x},{popup_y}) should be ┌, got {tl_sym}"
    );

    // Assert top-right border corner
    let tr_x = popup_x + popup_w - 1;
    let tr_sym = buffer[(tr_x, popup_y)].symbol();
    assert_eq!(
        tr_sym, "┐",
        "top-right corner at ({tr_x},{popup_y}) should be ┐, got {tr_sym}"
    );

    // Assert bottom-left border corner
    let bl_y = popup_y + popup_h - 1;
    let bl_sym = buffer[(popup_x, bl_y)].symbol();
    assert_eq!(
        bl_sym, "└",
        "bottom-left corner at ({popup_x},{bl_y}) should be └, got {bl_sym}"
    );

    // Assert bottom-right border corner
    let br_sym = buffer[(tr_x, bl_y)].symbol();
    assert_eq!(
        br_sym, "┘",
        "bottom-right corner at ({tr_x},{bl_y}) should be ┘, got {br_sym}"
    );

    // Verify title is on the top border row
    let mut title_row = String::new();
    for x in popup_x..popup_x + popup_w {
        title_row.push_str(buffer[(x, popup_y)].symbol());
    }
    assert!(
        title_row.contains("Request Detail"),
        "title row should contain 'Request Detail', got: {title_row}"
    );

    // Verify log content is visible OUTSIDE the popup (e.g. the log title row)
    let mut log_title_row = String::new();
    for x in 0..width {
        log_title_row.push_str(buffer[(x, log_area.y)].symbol());
    }
    assert!(
        log_title_row.contains("Request Log"),
        "Request Log title should be visible outside the popup"
    );
}

#[test]
fn render_long_non_ascii_scroll_reachability() {
    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "test".into(),
            url: "http://test".into(),
            token: "t".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    let mut app =
        zone_router::state::AppState::new(config, dir.path().join("unicode-scroll.toml")).unwrap();

    let mut long_body = String::new();
    for i in 0..80 {
        long_body.push_str(&format!("行-{i}: こんにちは世界🌍データ\n"));
    }
    long_body.push_str("UTAIL");

    app.stats.record(zone_router::stats::RequestLogEntry::new(
        chrono::Utc::now(),
        "test".into(),
        10,
        zone_router::stats::CapturedRequest {
            method: "GET".into(),
            path: "/api".into(),
            headers: zone_router::stats::HeaderPairs::default(),
            body: None,
        },
        zone_router::stats::CapturedResponse {
            status: 200,
            headers: zone_router::stats::HeaderPairs::default(),
            body: Some(long_body),
        },
    ));

    let width: u16 = 80;
    let height: u16 = 40;

    // At scroll=0, the tail marker should NOT be visible
    let tui_top = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        detail_scroll: 0,
        ..TuiState::default()
    };
    let output_top = render_to_string(&app, &tui_top, width, height);
    assert!(
        !output_top.contains("UTAIL"),
        "non-ASCII tail should NOT be visible at scroll=0"
    );

    // At a large scroll value, the tail marker SHOULD be visible
    let tui_bottom = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        detail_scroll: 300,
        ..TuiState::default()
    };
    let output_bottom = render_to_string(&app, &tui_bottom, width, height);
    assert!(
        output_bottom.contains("UTAIL"),
        "non-ASCII tail should be visible when scrolled to bottom"
    );

    // Over-scroll should clamp and still show the tail
    let tui_clamped = TuiState {
        focus: FocusPanel::RequestLog,
        mode: InputMode::DetailView,
        log_cursor: 0,
        detail_scroll: 9999,
        ..TuiState::default()
    };
    let output_clamped = render_to_string(&app, &tui_clamped, width, height);
    assert!(
        output_clamped.contains("UTAIL"),
        "over-scrolling non-ASCII content should clamp and still show tail"
    );
}

#[test]
fn render_log_auto_scroll_and_highlight() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "test".into(),
            url: "http://test".into(),
            token: "t".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    let mut app =
        zone_router::state::AppState::new(config, dir.path().join("autoscroll.toml")).unwrap();

    // Add 30 log entries so they exceed the viewport
    for i in 0..30 {
        app.stats.record(zone_router::stats::RequestLogEntry::new(
            chrono::Utc::now(),
            format!("backend-{i}"),
            i as u64,
            zone_router::stats::CapturedRequest {
                method: "POST".into(),
                path: format!("/path-{i}"),
                headers: zone_router::stats::HeaderPairs::default(),
                body: None,
            },
            zone_router::stats::CapturedResponse {
                status: 200,
                headers: zone_router::stats::HeaderPairs::default(),
                body: None,
            },
        ));
    }

    let width: u16 = 100;
    let height: u16 = 30;

    // Cursor at 0: newest entry (backend-29) should be at or near the top
    // and the cursor marker ">" should be visible
    let tui_top = TuiState {
        focus: FocusPanel::RequestLog,
        log_cursor: 0,
        ..TuiState::default()
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| zone_router::tui::ui::draw(frame, &app, &tui_top))
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut output_top = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            output_top.push_str(buf[(x, y)].symbol());
        }
        output_top.push('\n');
    }
    assert!(
        output_top.contains("backend-29"),
        "newest entry should be visible at cursor=0"
    );
    assert!(
        output_top.contains(">"),
        "cursor marker should be visible at cursor=0"
    );

    // Move cursor to 25 (deep into the list): backend-4 is the 25th from newest
    // The selected entry should be visible and older entries near top should scroll out
    let tui_deep = TuiState {
        focus: FocusPanel::RequestLog,
        log_cursor: 25,
        ..TuiState::default()
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| zone_router::tui::ui::draw(frame, &app, &tui_deep))
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut output_deep = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            output_deep.push_str(buf[(x, y)].symbol());
        }
        output_deep.push('\n');
    }

    // The selected entry (backend-4, which is log_cursor=25 counting from newest) should be visible
    assert!(
        output_deep.contains("backend-4"),
        "selected entry backend-4 should be visible when cursor is at 25"
    );
    // The newest entry (backend-29) should have scrolled out of view
    assert!(
        !output_deep.contains("backend-29"),
        "newest entry should have scrolled out when cursor is deep in the list"
    );
    // The cursor marker ">" should still be visible for the selected row
    assert!(
        output_deep.contains(">"),
        "cursor marker should be visible for the selected row"
    );
}
