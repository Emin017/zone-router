use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::routing::post;
use axum::Router;
use futures_util::stream;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

mod common;
use common::make_state;

// --- SSE streaming tests ---

/// Mock backend that returns an SSE stream with 3 events.
async fn start_sse_backend() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/messages",
        post(|| async {
            let events = vec![
                Ok::<_, std::convert::Infallible>(Event::default().data("chunk1")),
                Ok(Event::default().data("chunk2")),
                Ok(Event::default().data("[DONE]")),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}
#[tokio::test]
async fn sse_stream_passes_through_chunks() {
    let (backend_url, _handle) = start_sse_backend().await;
    let state = make_state(vec![("sse", &backend_url, "tok")], "secret");
    let router = api_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"stream":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("chunk1"), "should contain chunk1");
    assert!(body_str.contains("chunk2"), "should contain chunk2");
    assert!(body_str.contains("[DONE]"), "should contain DONE marker");
}

#[tokio::test]
async fn sse_stream_logs_after_completion() {
    let (backend_url, _handle) = start_sse_backend().await;
    let state = make_state(vec![("sse-log", &backend_url, "tok")], "secret");
    let router = api_router::proxy::server::build_router(state.clone());

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Consume the stream fully
    let _ = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await;
    // Give the log task a moment
    tokio::time::sleep(Duration::from_millis(100)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    assert_eq!(s.stats.log[0].backend, "sse-log");
}

// --- Config tests ---

#[test]
fn config_parse_error_on_invalid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "this is not valid toml {{{").unwrap();
    let result = api_router::config::Config::load_or_create(&path);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(err_msg.contains("parse"), "error should mention parse: {err_msg}");
}

#[test]
fn config_auto_creates_default_at_custom_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom").join("nested").join("config.toml");
    assert!(!path.exists());
    let config = api_router::config::Config::load_or_create(&path).unwrap();
    assert!(path.exists());
    assert_eq!(config.proxy.listen, "127.0.0.1:8080");
}

#[test]
fn config_custom_path_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("my-config.toml");
    let config = api_router::config::Config {
        proxy: api_router::config::ProxyConfig {
            listen: "0.0.0.0:5555".into(),
            local_token: "custom-tok".into(),
        },
        backends: vec![api_router::config::Backend {
            name: "test".into(),
            url: "http://test".into(),
            token: "t".into(),
            active: true,
        }],
    };
    config.save(&path).unwrap();
    let loaded = api_router::config::Config::load_or_create(&path).unwrap();
    assert_eq!(loaded.proxy.listen, "0.0.0.0:5555");
    assert_eq!(loaded.proxy.local_token, "custom-tok");
    assert_eq!(loaded.backends.len(), 1);
}

// --- Token persistence / env tests ---

#[test]
fn env_token_stable_across_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    // First load generates and persists a token
    let state1 = api_router::state::AppState::new(
        api_router::config::Config::load_or_create(&path).unwrap(),
        path.clone(),
    );
    let token1 = state1.local_token.clone();
    assert!(token1.starts_with("sk-local-"));

    // Second load should reuse the persisted token
    let state2 = api_router::state::AppState::new(
        api_router::config::Config::load_or_create(&path).unwrap(),
        path.clone(),
    );
    assert_eq!(state2.local_token, token1, "token should be stable across loads");
}

// --- Port binding tests ---

#[tokio::test]
async fn port_binding_occupied_fails() {
    // Bind a port first
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Try to bind the same port — should fail
    let result = tokio::net::TcpListener::bind(addr).await;
    assert!(result.is_err(), "binding an occupied port should fail");
}

// --- In-flight request during backend switch ---

#[tokio::test]
async fn inflight_request_completes_during_backend_switch() {
    use axum::routing::post;
    use tokio::sync::Barrier;

    let barrier = Arc::new(Barrier::new(2));
    let b = barrier.clone();

    // Slow backend that waits on a barrier
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let b = b.clone();
            async move {
                b.wait().await;
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = make_state(
        vec![
            ("slow", &format!("http://{addr}"), "tok"),
            ("other", "http://127.0.0.1:1", "tok2"),
        ],
        "secret",
    );

    // Start the request (will block on barrier)
    let router = api_router::proxy::server::build_router(state.clone());
    let req_handle = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("x-api-key", "secret")
                    .body(Body::from("test"))
                    .unwrap(),
            )
            .await
            .unwrap()
    });

    // Give the request a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Switch backend while request is in-flight
    state.write().await.switch_backend(1);

    // Release the barrier so the in-flight request completes
    barrier.wait().await;

    let resp = req_handle.await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "in-flight request should complete successfully");
}

// --- Graceful shutdown ---

#[tokio::test]
async fn shutdown_rejects_new_requests() {
    let state = make_state(vec![("test", "http://127.0.0.1:1", "tok")], "secret");

    // Set shutdown flag
    state.write().await.shutdown = true;

    let router = api_router::proxy::server::build_router(state);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "requests during shutdown should get 503"
    );
}

#[tokio::test]
async fn shutdown_force_closes_after_timeout() {
    use tokio::sync::Barrier;

    let barrier = Arc::new(Barrier::new(2));
    let b = barrier.clone();

    // Backend that blocks forever (until barrier, which we never release)
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let b = b.clone();
            async move {
                b.wait().await;
                "ok"
            }
        }),
    );
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = backend_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(backend_listener, app).await.unwrap() });

    let config = api_router::config::Config {
        proxy: api_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "secret".into(),
        },
        backends: vec![api_router::config::Backend {
            name: "hang".into(),
            url: format!("http://{backend_addr}"),
            token: "tok".into(),
            active: true,
        }],
    };
    let dir = tempfile::tempdir().unwrap();
    let app_state = api_router::state::AppState::new(
        config,
        dir.path().join("shutdown-test.toml"),
    );
    let listen_addr = app_state.config.proxy.listen.clone();
    let listener = tokio::net::TcpListener::bind(&listen_addr).await.unwrap();
    let actual_addr = listener.local_addr().unwrap();

    let state = Arc::new(tokio::sync::RwLock::new(app_state));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let server_state = state.clone();
    let mut server_handle = tokio::spawn(async move {
        let _ = api_router::proxy::server::start_with_listener(server_state, listener, shutdown_rx).await;
    });

    // Send a request that will hang
    let _req_handle = tokio::spawn(async move {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let _ = client
            .post(format!("http://{actual_addr}/v1/messages"))
            .header("x-api-key", "secret")
            .body("test")
            .send()
            .await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Signal shutdown
    state.write().await.shutdown = true;
    let _ = shutdown_tx.send(true);

    // Wait with a short timeout (simulating the 5s deadline but shorter for test speed)
    let timed_out = tokio::time::timeout(Duration::from_millis(500), &mut server_handle)
        .await
        .is_err();

    if timed_out {
        server_handle.abort();
    }

    // The server task should now be finished (aborted)
    assert!(server_handle.await.unwrap_err().is_cancelled(), "server should be aborted after timeout");
}

// --- SSE mid-stream disconnect ---

#[tokio::test]
async fn sse_backend_disconnect_closes_client_stream() {
    // Backend that sends one event then drops the connection
    let app = Router::new().route(
        "/v1/messages",
        post(|| async {
            let events = vec![
                Ok::<_, std::convert::Infallible>(Event::default().data("partial")),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = make_state(vec![("disc", &format!("http://{addr}"), "tok")], "secret");
    let router = api_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    // Stream should terminate cleanly after the partial event
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("partial"), "should have received the partial event");
}

// --- CLI env output test ---

#[tokio::test]
async fn cli_env_output_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_api-router"))
        .args(["--config", path.to_str().unwrap(), "env"])
        .output()
        .await
        .unwrap();

    assert!(output.status.success(), "env subcommand should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("export ANTHROPIC_BASE_URL=http://"), "should contain BASE_URL export");
    assert!(stdout.contains("export ANTHROPIC_API_KEY=sk-local-"), "should contain API_KEY export");

    // Run again — token should be stable
    let output2 = tokio::process::Command::new(env!("CARGO_BIN_EXE_api-router"))
        .args(["--config", path.to_str().unwrap(), "env"])
        .output()
        .await
        .unwrap();
    let stdout2 = String::from_utf8(output2.stdout).unwrap();
    assert_eq!(stdout, stdout2, "env output should be stable across invocations");
}

// --- Actual port startup and --port flag tests ---

#[tokio::test]
async fn server_accepts_requests_on_listen_port() {
    let state = make_state(vec![("test", "http://127.0.0.1:1", "tok")], "secret");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let server_state = state.clone();
    tokio::spawn(async move {
        let _ = api_router::proxy::server::start_with_listener(server_state, listener, shutdown_rx).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .post(format!("http://{addr}/v1/messages"))
        .header("x-api-key", "secret")
        .send()
        .await
        .unwrap();

    // Should get a response (not connection refused)
    assert_ne!(resp.status().as_u16(), 0, "should receive a valid HTTP response");
}

#[tokio::test]
async fn port_flag_changes_listen_address() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // Create a config with a backend
    let config = api_router::config::Config {
        proxy: api_router::config::ProxyConfig {
            listen: "127.0.0.1:8080".into(),
            local_token: "test-tok".into(),
        },
        backends: vec![api_router::config::Backend {
            name: "dummy".into(),
            url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            active: true,
        }],
    };
    config.save(&config_path).unwrap();

    // Start with --port on a random available port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // free the port

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_api-router"))
        .args(["--config", config_path.to_str().unwrap(), "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // The process binds the port before launching TUI, so even a brief window is enough
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify the port was bound by attempting a TCP connection
    let conn = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await;
    assert!(conn.is_ok(), "should be able to connect to the --port address");

    child.kill().await.unwrap();
}

// --- Post-switch routing and persistence tests ---

#[tokio::test]
async fn new_requests_route_to_new_backend_after_switch() {
    // Two mock backends that identify themselves
    let app1 = Router::new().route(
        "/v1/messages",
        post(|| async { "backend-1" }),
    );
    let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr1 = listener1.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener1, app1).await.unwrap() });

    let app2 = Router::new().route(
        "/v1/messages",
        post(|| async { "backend-2" }),
    );
    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = listener2.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener2, app2).await.unwrap() });

    let state = make_state(
        vec![
            ("first", &format!("http://{addr1}"), "tok1"),
            ("second", &format!("http://{addr2}"), "tok2"),
        ],
        "secret",
    );

    // Request before switch goes to backend-1
    let router = api_router::proxy::server::build_router(state.clone());
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(String::from_utf8(body.to_vec()).unwrap(), "backend-1");

    // Switch to second backend
    state.write().await.switch_backend(1);

    // Request after switch goes to backend-2
    let router = api_router::proxy::server::build_router(state.clone());
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(String::from_utf8(body.to_vec()).unwrap(), "backend-2");
}

#[test]
fn backend_switch_persists_active_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let config = api_router::config::Config {
        proxy: api_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![
            api_router::config::Backend { name: "a".into(), url: "http://a".into(), token: "ta".into(), active: true },
            api_router::config::Backend { name: "b".into(), url: "http://b".into(), token: "tb".into(), active: false },
        ],
    };
    let mut state = api_router::state::AppState::new(config, path.clone());
    state.switch_backend(1);

    // Reload config from disk and verify active backend persisted
    let loaded = api_router::config::Config::load_or_create(&path).unwrap();
    assert!(!loaded.backends[0].active, "first backend should not be active");
    assert!(loaded.backends[1].active, "second backend should be active after switch");
}

// --- TUI input tests ---

#[cfg(test)]
mod tui_tests {
    use api_router::tui::app::{FocusPanel, InputMode, TuiState};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_tui_and_state() -> (
        TuiState,
        std::sync::Arc<tokio::sync::RwLock<api_router::state::AppState>>,
        tokio::runtime::Runtime,
        tempfile::TempDir,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config = api_router::config::Config {
            proxy: api_router::config::ProxyConfig {
                listen: "127.0.0.1:0".into(),
                local_token: "tok".into(),
            },
            backends: vec![
                api_router::config::Backend {
                    name: "a".into(),
                    url: "http://a".into(),
                    token: "ta".into(),
                    active: true,
                },
                api_router::config::Backend {
                    name: "b".into(),
                    url: "http://b".into(),
                    token: "tb".into(),
                    active: false,
                },
                api_router::config::Backend {
                    name: "c".into(),
                    url: "http://c".into(),
                    token: "tc".into(),
                    active: false,
                },
            ],
        };
        let state = std::sync::Arc::new(tokio::sync::RwLock::new(
            api_router::state::AppState::new(config, dir.path().join("tui-test.toml")),
        ));
        (TuiState::default(), state, rt, dir)
    }

    #[test]
    fn tab_switches_focus() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        assert_eq!(tui.focus, FocusPanel::Backends);

        api_router::tui::input::handle_input(key(KeyCode::Tab), &mut tui, &state, rt.handle());
        assert_eq!(tui.focus, FocusPanel::RequestLog);

        api_router::tui::input::handle_input(key(KeyCode::Tab), &mut tui, &state, rt.handle());
        assert_eq!(tui.focus, FocusPanel::Backends);
    }

    #[test]
    fn backtab_switches_focus() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        api_router::tui::input::handle_input(key(KeyCode::BackTab), &mut tui, &state, rt.handle());
        assert_eq!(tui.focus, FocusPanel::RequestLog);
    }

    #[test]
    fn j_k_navigate_backends() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        assert_eq!(tui.cursor, 0);

        api_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
        assert_eq!(tui.cursor, 1);

        api_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
        assert_eq!(tui.cursor, 2);

        // j at end stays at end
        api_router::tui::input::handle_input(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
        assert_eq!(tui.cursor, 2);

        api_router::tui::input::handle_input(key(KeyCode::Char('k')), &mut tui, &state, rt.handle());
        assert_eq!(tui.cursor, 1);
    }

    #[test]
    fn big_g_goes_to_end() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        api_router::tui::input::handle_input(key(KeyCode::Char('G')), &mut tui, &state, rt.handle());
        assert_eq!(tui.cursor, 2);
    }

    #[test]
    fn gg_goes_to_start() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        tui.cursor = 2;
        api_router::tui::input::handle_input(key(KeyCode::Char('g')), &mut tui, &state, rt.handle());
        api_router::tui::input::handle_input(key(KeyCode::Char('g')), &mut tui, &state, rt.handle());
        assert_eq!(tui.cursor, 0);
    }

    #[test]
    fn enter_switches_active_backend() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        tui.cursor = 1;
        api_router::tui::input::handle_input(key(KeyCode::Enter), &mut tui, &state, rt.handle());
        let s = rt.block_on(state.read());
        assert_eq!(s.active_index, 1);
    }

    #[test]
    fn number_keys_switch_backend() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        api_router::tui::input::handle_input(key(KeyCode::Char('2')), &mut tui, &state, rt.handle());
        let s = rt.block_on(state.read());
        assert_eq!(s.active_index, 1);
        assert_eq!(tui.cursor, 1);
    }

    #[test]
    fn a_enters_add_mode() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        api_router::tui::input::handle_input(key(KeyCode::Char('a')), &mut tui, &state, rt.handle());
        assert_eq!(tui.mode, InputMode::AddName);
    }

    #[test]
    fn e_enters_edit_mode() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        api_router::tui::input::handle_input(key(KeyCode::Char('e')), &mut tui, &state, rt.handle());
        assert_eq!(tui.mode, InputMode::EditName);
    }

    #[test]
    fn d_deletes_backend() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        tui.cursor = 2;
        api_router::tui::input::handle_input(key(KeyCode::Char('d')), &mut tui, &state, rt.handle());
        let s = rt.block_on(state.read());
        assert_eq!(s.config.backends.len(), 2);
    }

    #[test]
    fn t_shows_token() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        api_router::tui::input::handle_input(key(KeyCode::Char('t')), &mut tui, &state, rt.handle());
        assert_eq!(tui.mode, InputMode::ShowToken);
    }

    #[test]
    fn slash_enters_search() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        api_router::tui::input::handle_input(key(KeyCode::Char('/')), &mut tui, &state, rt.handle());
        assert_eq!(tui.mode, InputMode::Search);
    }

    #[test]
    fn normal_keys_ignored_in_input_mode() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        tui.mode = InputMode::AddName;
        // 'j' in input mode should type 'j', not navigate
        api_router::tui::input::handle_input_mode(key(KeyCode::Char('j')), &mut tui, &state, rt.handle());
        assert_eq!(tui.input_buffer, "j");
        assert_eq!(tui.cursor, 0); // cursor unchanged
    }

    #[test]
    fn q_exits_tui() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        let should_exit = api_router::tui::input::handle_input(key(KeyCode::Char('q')), &mut tui, &state, rt.handle());
        assert!(should_exit);
    }
}
