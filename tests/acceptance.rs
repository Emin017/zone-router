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
    let router = zone_router::proxy::server::build_router(state);

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
    let router = zone_router::proxy::server::build_router(state.clone());

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
    let result = zone_router::config::Config::load_or_create(&path);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("parse"),
        "error should mention parse: {err_msg}"
    );
}

#[test]
fn config_auto_creates_default_at_custom_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom").join("nested").join("config.toml");
    assert!(!path.exists());
    let config = zone_router::config::Config::load_or_create(&path).unwrap();
    assert!(path.exists());
    assert_eq!(config.proxy.listen, "127.0.0.1:8080");
}

#[test]
fn config_custom_path_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("my-config.toml");
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "0.0.0.0:5555".into(),
            local_token: "custom-tok".into(),
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
    config.save(&path).unwrap();
    let loaded = zone_router::config::Config::load_or_create(&path).unwrap();
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
    let state1 = zone_router::state::AppState::new(
        zone_router::config::Config::load_or_create(&path).unwrap(),
        path.clone(),
    )
    .unwrap();
    let token1 = state1.local_token.clone();
    assert!(token1.starts_with("sk-local-"));

    // Second load should reuse the persisted token
    let state2 = zone_router::state::AppState::new(
        zone_router::config::Config::load_or_create(&path).unwrap(),
        path.clone(),
    )
    .unwrap();
    assert_eq!(
        state2.local_token, token1,
        "token should be stable across loads"
    );
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
    let router = zone_router::proxy::server::build_router(state.clone());
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
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "in-flight request should complete successfully"
    );
}

// --- Graceful shutdown ---

#[tokio::test]
async fn shutdown_rejects_new_requests() {
    let state = make_state(vec![("test", "http://127.0.0.1:1", "tok")], "secret");

    // Set shutdown flag
    state.write().await.shutdown = true;

    let router = zone_router::proxy::server::build_router(state);
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
    // Signaling backend: accepts a connection, signals when the forwarded
    // request arrives, then blocks forever.
    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let signal_tx = Arc::new(tokio::sync::Mutex::new(Some(signal_tx)));

    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = backend_listener.local_addr().unwrap();

    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let tx = signal_tx.clone();
            async move {
                // Signal that the request has been admitted
                if let Some(tx) = tx.lock().await.take() {
                    let _ = tx.send(());
                }
                // Block forever
                futures_util::future::pending::<&str>().await
            }
        }),
    );
    tokio::spawn(async move { axum::serve(backend_listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "secret".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "hang".into(),
            url: format!("http://{backend_addr}"),
            token: "tok".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    let app_state =
        zone_router::state::AppState::new(config, dir.path().join("shutdown.toml")).unwrap();
    let listener = tokio::net::TcpListener::bind(&app_state.config.proxy.listen)
        .await
        .unwrap();
    let proxy_addr = listener.local_addr().unwrap();

    let state = Arc::new(tokio::sync::RwLock::new(app_state));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let server_state = state.clone();
    let mut server_handle = tokio::spawn(async move {
        let _ =
            zone_router::proxy::server::start_with_listener(server_state, listener, shutdown_rx)
                .await;
    });

    // Send a request that will hang forever
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .no_proxy()
            .read_timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let _ = client
            .post(format!("http://{proxy_addr}/v1/messages"))
            .header("x-api-key", "secret")
            .body("test")
            .send()
            .await;
    });

    // Wait for the backend to confirm the request is in-flight
    tokio::time::timeout(Duration::from_secs(5), signal_rx)
        .await
        .expect("timed out waiting for backend signal")
        .expect("signal channel dropped");

    // Signal graceful shutdown (equivalent to TUI exiting)
    let _ = shutdown_tx.send(true);

    // Call the production shutdown function — the same code path as main()
    let start = std::time::Instant::now();
    zone_router::proxy::server::force_shutdown(state, &mut server_handle).await;
    let elapsed = start.elapsed();

    // The server should NOT have exited gracefully (the request hangs forever),
    // so the 5s timeout must have been needed.
    assert!(
        elapsed >= Duration::from_secs(4),
        "shutdown completed in {elapsed:?} — too fast, the 5s forced-abort was not exercised"
    );

    // Prove the server task was actually terminated by the abort.
    // Await the handle — it should complete promptly since abort was called.
    let join_result = tokio::time::timeout(Duration::from_secs(1), server_handle).await;
    assert!(
        join_result.is_ok(),
        "server task should complete promptly after force_shutdown abort"
    );
    // The join result is Err(JoinError::Cancelled) because the task was aborted
    let task_result = join_result.unwrap();
    assert!(
        task_result.unwrap_err().is_cancelled(),
        "server task should have been cancelled by abort"
    );
}

// --- SSE mid-stream disconnect ---

#[tokio::test]
async fn sse_backend_disconnect_closes_client_stream() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Raw TCP server: sends valid HTTP response with one correctly-framed
    // chunked SSE event, then drops the socket mid-stream.
    let raw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let raw_addr = raw_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut socket, _) = raw_listener.accept().await.unwrap();

        // Read until end-of-headers
        let mut buf = vec![0u8; 8192];
        let mut total = 0;
        loop {
            let n = socket.read(&mut buf[total..]).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            total += n;
            if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }

        // Valid HTTP response with chunked transfer encoding
        let header = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        socket.write_all(header.as_bytes()).await.unwrap();

        // First chunk: a valid, correctly-sized SSE event
        // "data: event1\n\n" = 14 bytes = 0xe hex
        let chunk1 = "e\r\ndata: event1\n\n\r\n";
        socket.write_all(chunk1.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();

        // Give the proxy time to forward the first chunk to the client
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop the socket mid-stream (no terminating 0-length chunk)
        drop(socket);
    });

    let state = make_state(
        vec![("disc", &format!("http://{raw_addr}"), "tok")],
        "secret",
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let server_state = state.clone();
    tokio::spawn(async move {
        let _ =
            zone_router::proxy::server::start_with_listener(server_state, listener, shutdown_rx)
                .await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut resp = client
        .post(format!("http://{proxy_addr}/v1/messages"))
        .header("x-api-key", "secret")
        .body("{}")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);

    // Phase 1: Read the first chunk — should contain the valid SSE event
    let first_chunk = tokio::time::timeout(Duration::from_secs(3), resp.chunk())
        .await
        .expect("timed out waiting for first chunk")
        .expect("error reading first chunk");

    let first_bytes = first_chunk.expect("should have received at least one chunk");
    let chunk_data = String::from_utf8_lossy(&first_bytes);
    assert!(
        chunk_data.contains("event1"),
        "first chunk should contain the SSE event data, got: {chunk_data}"
    );

    // Phase 2: The next read should terminate (EOF or error) because the
    // backend dropped the socket. It must not hang.
    let second_read = tokio::time::timeout(Duration::from_secs(3), resp.chunk()).await;
    assert!(
        second_read.is_ok(),
        "stream should terminate after backend disconnect, not hang"
    );
}

// --- CLI env output test ---

#[tokio::test]
async fn cli_env_output_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_zone-router"))
        .args(["--config", path.to_str().unwrap(), "env"])
        .output()
        .await
        .unwrap();

    assert!(output.status.success(), "env subcommand should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("export ANTHROPIC_BASE_URL=http://"),
        "should contain BASE_URL export"
    );
    assert!(
        stdout.contains("export ANTHROPIC_API_KEY=sk-local-"),
        "should contain API_KEY export"
    );
    assert!(
        stdout.contains("export ANTHROPIC_AUTH_TOKEN=sk-local-"),
        "should contain AUTH_TOKEN export"
    );

    // Run again — token should be stable
    let output2 = tokio::process::Command::new(env!("CARGO_BIN_EXE_zone-router"))
        .args(["--config", path.to_str().unwrap(), "env"])
        .output()
        .await
        .unwrap();
    let stdout2 = String::from_utf8(output2.stdout).unwrap();
    assert_eq!(
        stdout, stdout2,
        "env output should be stable across invocations"
    );
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
        let _ =
            zone_router::proxy::server::start_with_listener(server_state, listener, shutdown_rx)
                .await;
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
    assert_ne!(
        resp.status().as_u16(),
        0,
        "should receive a valid HTTP response"
    );
}

#[tokio::test]
async fn port_flag_changes_listen_address() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // Create a config with a backend
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:8080".into(),
            local_token: "test-tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "dummy".into(),
            url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    config.save(&config_path).unwrap();

    // Start with --port on a random available port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // free the port

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_zone-router"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // The process binds the port before launching TUI, so even a brief window is enough
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify the port was bound by attempting a TCP connection
    let conn = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await;
    assert!(
        conn.is_ok(),
        "should be able to connect to the --port address"
    );

    child.kill().await.unwrap();
}

// --- Post-switch routing and persistence tests ---

#[tokio::test]
async fn new_requests_route_to_new_backend_after_switch() {
    // Two mock backends that identify themselves
    let app1 = Router::new().route("/v1/messages", post(|| async { "backend-1" }));
    let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr1 = listener1.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener1, app1).await.unwrap() });

    let app2 = Router::new().route("/v1/messages", post(|| async { "backend-2" }));
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
    let router = zone_router::proxy::server::build_router(state.clone());
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
    let router = zone_router::proxy::server::build_router(state.clone());
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
        ],
    };
    let mut state = zone_router::state::AppState::new(config, path.clone()).unwrap();
    state.switch_backend(1);

    // Reload config from disk and verify active backend persisted
    let loaded = zone_router::config::Config::load_or_create(&path).unwrap();
    assert!(
        !loaded.backends[0].active,
        "first backend should not be active"
    );
    assert!(
        loaded.backends[1].active,
        "second backend should be active after switch"
    );
}

// --- Model map persistence tests ---

#[test]
fn add_backend_with_model_map_persists_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![],
    };
    let mut state = zone_router::state::AppState::new(config, path.clone()).unwrap();
    state.add_backend(zone_router::config::Backend {
        name: "mapped".into(),
        url: "http://m".into(),
        token: "tm".into(),
        active: false,
        auth_type: zone_router::config::AuthType::default(),
        model_map: Some(zone_router::config::ModelMap {
            haiku: Some("h-model".into()),
            sonnet: Some("s-model".into()),
            opus: None,
        }),
    });

    let loaded = zone_router::config::Config::load_or_create(&path).unwrap();
    let mm = loaded.backends[0]
        .model_map
        .as_ref()
        .expect("model_map should be persisted");
    assert_eq!(mm.haiku.as_deref(), Some("h-model"));
    assert_eq!(mm.sonnet.as_deref(), Some("s-model"));
    assert!(mm.opus.is_none());
}

#[test]
fn update_backend_with_model_map_persists_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "plain".into(),
            url: "http://p".into(),
            token: "tp".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        }],
    };
    let mut state = zone_router::state::AppState::new(config, path.clone()).unwrap();
    state.update_backend(
        0,
        "plain".into(),
        "http://p".into(),
        "tp".into(),
        zone_router::config::AuthType::default(),
        Some(zone_router::config::ModelMap {
            haiku: None,
            sonnet: None,
            opus: Some("o-model".into()),
        }),
    );

    let loaded = zone_router::config::Config::load_or_create(&path).unwrap();
    let mm = loaded.backends[0]
        .model_map
        .as_ref()
        .expect("model_map should be persisted after update");
    assert!(mm.haiku.is_none());
    assert!(mm.sonnet.is_none());
    assert_eq!(mm.opus.as_deref(), Some("o-model"));
}

#[test]
fn update_backend_clearing_model_map_removes_section_from_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "was-mapped".into(),
            url: "http://w".into(),
            token: "tw".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: Some(zone_router::config::ModelMap {
                haiku: Some("h".into()),
                sonnet: Some("s".into()),
                opus: Some("o".into()),
            }),
        }],
    };
    let mut state = zone_router::state::AppState::new(config, path.clone()).unwrap();

    // Clear the model_map
    state.update_backend(
        0,
        "was-mapped".into(),
        "http://w".into(),
        "tw".into(),
        zone_router::config::AuthType::default(),
        None,
    );

    let loaded = zone_router::config::Config::load_or_create(&path).unwrap();
    assert!(
        loaded.backends[0].model_map.is_none(),
        "model_map should be None after clearing"
    );

    // Also verify the raw TOML doesn't contain [backends.model_map]
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains("[backends.model_map]"),
        "TOML should not contain [backends.model_map] section after clearing, got:\n{raw}"
    );
}

// --- TUI input tests ---

#[cfg(test)]
mod tui_tests {
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
        assert_eq!(tui.focus, FocusPanel::Backends);
    }

    #[test]
    fn backtab_switches_focus() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        zone_router::tui::input::handle_input(key(KeyCode::BackTab), &mut tui, &state, rt.handle());
        assert_eq!(tui.focus, FocusPanel::RequestLog);
    }

    #[test]
    fn j_k_navigate_backends() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        assert_eq!(tui.cursor, 0);

        zone_router::tui::input::handle_input(
            key(KeyCode::Char('j')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.cursor, 1);

        zone_router::tui::input::handle_input(
            key(KeyCode::Char('j')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.cursor, 2);

        // j at end stays at end
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('j')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.cursor, 2);

        zone_router::tui::input::handle_input(
            key(KeyCode::Char('k')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.cursor, 1);
    }

    #[test]
    fn big_g_goes_to_end() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('G')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.cursor, 2);
    }

    #[test]
    fn gg_goes_to_start() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        tui.cursor = 2;
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('g')),
            &mut tui,
            &state,
            rt.handle(),
        );
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('g')),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('2')),
            &mut tui,
            &state,
            rt.handle(),
        );
        let s = rt.block_on(state.read());
        assert_eq!(s.active_index, 1);
        assert_eq!(tui.cursor, 1);
    }

    #[test]
    fn a_enters_add_mode() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('a')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::AddName);
    }

    #[test]
    fn e_enters_edit_mode() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('e')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditName);
    }

    #[test]
    fn d_deletes_backend() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        tui.cursor = 2;
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('d')),
            &mut tui,
            &state,
            rt.handle(),
        );
        let s = rt.block_on(state.read());
        assert_eq!(s.config.backends.len(), 2);
    }

    #[test]
    fn t_shows_token() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('t')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::ShowToken);
    }

    #[test]
    fn slash_enters_search() {
        let (mut tui, state, rt, _dir) = make_tui_and_state();
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('/')),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('a')),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('a')),
            &mut tui,
            &state,
            rt.handle(),
        );

        // Name
        for c in "skip-test".chars() {
            zone_router::tui::input::handle_input_mode(
                key(KeyCode::Char(c)),
                &mut tui,
                &state,
                rt.handle(),
            );
        }
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

        // URL
        for c in "http://skip".chars() {
            zone_router::tui::input::handle_input_mode(
                key(KeyCode::Char(c)),
                &mut tui,
                &state,
                rt.handle(),
            );
        }
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

        // Token — after submitting, mode should be AddAuthType, NOT Normal
        for c in "tok".chars() {
            zone_router::tui::input::handle_input_mode(
                key(KeyCode::Char(c)),
                &mut tui,
                &state,
                rt.handle(),
            );
        }
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('e')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditName);

        // Accept current name
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditUrl);

        // Accept current URL
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditToken);

        // Accept current token
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::AddAuthType);

        // Now press Enter again with empty buffer — must NOT silently default to api-key
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditAuthType);

        // Now press Enter again with empty buffer — must NOT silently default to api-key
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('e')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditName);

        // Accept name
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        // Accept URL
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        // Accept token
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditAuthType);

        // Tab to continue to model map editor (opt-in)
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Tab),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );

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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Tab),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Tab),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        let tui_state = TuiState::default();

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| zone_router::tui::ui::draw(frame, &app_state, &tui_state))
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditModelMap);

        // Press Enter again on empty buffer — should NOT wipe model_map
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::AddModelMap);

        // Press Enter again on empty buffer — should NOT create backend
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::AddModelMap);
        assert!(tui.model_map_rejected);

        // Press Esc — should reset to Normal and clear the flag
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Esc),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.mode, InputMode::EditModelMap);
        assert!(tui.model_map_rejected);

        // Press Esc — should reset to Normal and clear the flag
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Esc),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
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
                s.stats.record(zone_router::stats::RequestLogEntry {
                    timestamp: chrono::Utc::now(),
                    backend: "test".into(),
                    latency_ms: i * 10,
                    request: zone_router::stats::CapturedRequest {
                        method: "POST".into(),
                        path: "/v1/messages".into(),
                        headers: zone_router::stats::HeaderPairs::default(),
                        body: Some(format!("body-{i}")),
                    },
                    response: zone_router::stats::CapturedResponse {
                        status: 200,
                        headers: zone_router::stats::HeaderPairs::default(),
                        body: Some(format!("resp-{i}")),
                    },
                });
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
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('j')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.log_cursor, 1);

        zone_router::tui::input::handle_input(
            key(KeyCode::Char('j')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.log_cursor, 2);

        // k moves cursor up
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('k')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.log_cursor, 1);

        // k at 0 stays at 0
        tui.log_cursor = 0;
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('k')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.log_cursor, 0);

        // j at end stays at end
        tui.log_cursor = 4;
        zone_router::tui::input::handle_input(
            key(KeyCode::Char('j')),
            &mut tui,
            &state,
            rt.handle(),
        );
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
        assert!(!tui.body_expanded);
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
    fn detail_view_enter_toggles_body() {
        let (mut tui, state, rt, _dir) = make_tui_with_log();
        tui.mode = InputMode::DetailView;
        assert!(!tui.body_expanded);

        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert!(tui.body_expanded);

        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Enter),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert!(!tui.body_expanded);
    }

    #[test]
    fn detail_view_n_p_cycles_entries() {
        let (mut tui, state, rt, _dir) = make_tui_with_log();
        tui.mode = InputMode::DetailView;
        tui.log_cursor = 0;

        // n moves to next (older) entry
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char('n')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.log_cursor, 1);
        assert_eq!(tui.detail_scroll, 0, "n should reset scroll");

        // p moves to previous (newer) entry
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char('p')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.log_cursor, 0);

        // p at 0 stays at 0
        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Char('p')),
            &mut tui,
            &state,
            rt.handle(),
        );
        assert_eq!(tui.log_cursor, 0);
    }

    #[test]
    fn detail_view_esc_closes_panel() {
        let (mut tui, state, rt, _dir) = make_tui_with_log();
        tui.mode = InputMode::DetailView;

        zone_router::tui::input::handle_input_mode(
            key(KeyCode::Esc),
            &mut tui,
            &state,
            rt.handle(),
        );
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

    // --- Render tests using TestBackend ---

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
        app.stats.record(zone_router::stats::RequestLogEntry {
            timestamp: chrono::Utc::now(),
            backend: "openai".into(),
            latency_ms: 245,
            request: zone_router::stats::CapturedRequest {
                method: "POST".into(),
                path: "/v1/messages".into(),
                headers: zone_router::stats::HeaderPairs(vec![(
                    "content-type".into(),
                    "application/json".into(),
                )]),
                body: Some(r#"{"model":"claude"}"#.into()),
            },
            response: zone_router::stats::CapturedResponse {
                status: 200,
                headers: zone_router::stats::HeaderPairs(vec![(
                    "content-type".into(),
                    "application/json".into(),
                )]),
                body: Some(r#"{"id":"msg_123","type":"message"}"#.into()),
            },
        });
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
        app.stats.record(zone_router::stats::RequestLogEntry {
            timestamp: chrono::Utc::now(),
            backend: "test".into(),
            latency_ms: 10,
            request: zone_router::stats::CapturedRequest {
                method: "POST".into(),
                path: "/api".into(),
                headers: zone_router::stats::HeaderPairs(vec![(
                    "x-custom".into(),
                    "日本語ヘッダー".into(),
                )]),
                body: Some("こんにちは世界🌍".into()),
            },
            response: zone_router::stats::CapturedResponse {
                status: 200,
                headers: zone_router::stats::HeaderPairs::default(),
                body: Some("Ÿéponse avéc dés àccents et emoji 🚀✨".into()),
            },
        });

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

        app.stats.record(zone_router::stats::RequestLogEntry {
            timestamp: chrono::Utc::now(),
            backend: "test".into(),
            latency_ms: 10,
            request: zone_router::stats::CapturedRequest {
                method: "GET".into(),
                path: "/api".into(),
                headers: zone_router::stats::HeaderPairs::default(),
                body: None,
            },
            response: zone_router::stats::CapturedResponse {
                status: 200,
                headers: zone_router::stats::HeaderPairs::default(),
                body: Some(long_body),
            },
        });

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
}
