/// Log instrumentation verification tests.
///
/// Uses an in-memory tracing capture helper to assert that the required log
/// categories are emitted and that token values never appear in log output.
mod common;

use std::io;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

/// In-memory writer for capturing tracing output.
#[derive(Clone)]
struct CaptureWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl CaptureWriter {
    fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap()).to_string()
    }
}

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Build a local subscriber that captures all zone_router events to a buffer.
fn make_capture_subscriber(
    writer: CaptureWriter,
) -> impl tracing::Subscriber + Send + Sync + 'static {
    tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_filter(EnvFilter::new("zone_router=debug")),
    )
}

// --- Positive tests: verify each instrumentation category is emitted ---

#[tokio::test]
async fn auth_failure_logs_warning() {
    let writer = CaptureWriter::new();
    let subscriber = make_capture_subscriber(writer.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let state = common::make_state(
        vec![("test-be", "http://localhost:1", "secret")],
        "valid-tok",
    );
    let router = zone_router::proxy::server::build_router(state);

    let req = Request::builder()
        .uri("/v1/messages")
        .header("x-api-key", "wrong-token")
        .body(Body::empty())
        .unwrap();

    let _resp = router.oneshot(req).await.unwrap();
    drop(_guard);

    let output = writer.contents();
    assert!(
        output.contains("auth failed"),
        "auth failure should be logged. Got: {output}"
    );
    assert!(
        output.contains("WARN"),
        "auth failure should be at WARN level. Got: {output}"
    );
}

#[tokio::test]
async fn backend_request_failure_logs_error() {
    let writer = CaptureWriter::new();
    let subscriber = make_capture_subscriber(writer.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let state = common::make_state(
        vec![("failing-be", "http://127.0.0.1:1", "be-tok")],
        "local-tok",
    );
    let router = zone_router::proxy::server::build_router(state);

    let req = Request::builder()
        .uri("/v1/messages")
        .header("x-api-key", "local-tok")
        .body(Body::from("{}"))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    drop(_guard);

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let output = writer.contents();
    assert!(
        output.contains("backend request failed"),
        "backend failure should be logged. Got: {output}"
    );
    assert!(
        output.contains("ERROR"),
        "backend failure should be at ERROR level. Got: {output}"
    );
    assert!(
        output.contains("failing-be"),
        "backend name should appear in error log. Got: {output}"
    );
    assert!(
        output.contains("127.0.0.1:1"),
        "backend URL should appear in error log. Got: {output}"
    );
}

#[test]
fn config_change_logs_info() {
    let writer = CaptureWriter::new();
    let subscriber = make_capture_subscriber(writer.clone());

    let dir = tempfile::tempdir().unwrap();
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "tok".into(),
        },
        backends: vec![
            zone_router::config::Backend {
                name: "be-a".into(),
                url: "http://a".into(),
                token: "ta".into(),
                active: true,
                auth_type: zone_router::config::AuthType::default(),
                model_map: None,
            },
            zone_router::config::Backend {
                name: "be-b".into(),
                url: "http://b".into(),
                token: "tb".into(),
                active: false,
                auth_type: zone_router::config::AuthType::default(),
                model_map: None,
            },
        ],
    };
    let mut state = zone_router::state::AppState::new(config, dir.path().join("cfg.toml")).unwrap();

    tracing::subscriber::with_default(subscriber, || {
        state.switch_backend(1);
        state.add_backend(zone_router::config::Backend {
            name: "be-c".into(),
            url: "http://c".into(),
            token: "tc".into(),
            active: false,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        });
        state.remove_backend(2);
    });

    let output = writer.contents();
    assert!(
        output.contains("switched active backend"),
        "backend switch should be logged. Got: {output}"
    );
    assert!(
        output.contains("INFO"),
        "backend switch should be at INFO level. Got: {output}"
    );
    assert!(
        output.contains("be-a") && output.contains("be-b"),
        "backend switch should include from/to names. Got: {output}"
    );
    assert!(
        output.contains("backend added"),
        "backend add should be logged. Got: {output}"
    );
    assert!(
        output.contains("backend removed"),
        "backend remove should be logged. Got: {output}"
    );
}

// --- Negative test: token values must never appear in logs ---

#[tokio::test]
async fn tokens_never_appear_in_logs() {
    let writer = CaptureWriter::new();
    let subscriber = make_capture_subscriber(writer.clone());

    let local_token = "sk-local-super-secret-12345";
    let backend_token = "sk-ant-backend-secret-67890";

    let state = common::make_state(
        vec![("token-be", "http://127.0.0.1:1", backend_token)],
        local_token,
    );
    let router = zone_router::proxy::server::build_router(state.clone());

    let req1 = Request::builder()
        .uri("/v1/messages")
        .header("x-api-key", "wrong")
        .body(Body::empty())
        .unwrap();

    let req2 = Request::builder()
        .uri("/v1/messages")
        .header("x-api-key", local_token)
        .body(Body::from("{}"))
        .unwrap();

    let router2 = zone_router::proxy::server::build_router(state.clone());

    let _guard = tracing::subscriber::set_default(subscriber);
    let _ = router.oneshot(req1).await;
    let _ = router2.oneshot(req2).await;

    {
        let mut s = state.write().await;
        s.switch_backend(0);
        s.add_backend(zone_router::config::Backend {
            name: "extra".into(),
            url: "http://x".into(),
            token: "sk-extra-secret".into(),
            active: false,
            auth_type: zone_router::config::AuthType::default(),
            model_map: None,
        });
    }
    drop(_guard);

    let output = writer.contents();
    assert!(
        !output.contains(local_token),
        "local token must never appear in logs. Got: {output}"
    );
    assert!(
        !output.contains(backend_token),
        "backend token must never appear in logs. Got: {output}"
    );
    assert!(
        !output.contains("sk-extra-secret"),
        "added backend token must never appear in logs. Got: {output}"
    );
}

// --- Runtime info tests ---

#[tokio::test]
async fn model_rewrite_logs_debug() {
    use axum::routing::post;

    let echo_app =
        axum::Router::new().route("/v1/messages", post(|body: String| async move { body }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _handle = tokio::spawn(async move {
        axum::serve(listener, echo_app).await.unwrap();
    });
    let backend_url = format!("http://{addr}");

    let mm = zone_router::config::ModelMap {
        haiku: None,
        sonnet: Some("custom-model-v1".into()),
        opus: None,
    };
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: "rewrite-tok".into(),
        },
        backends: vec![zone_router::config::Backend {
            name: "rewrite-be".into(),
            url: backend_url,
            token: "be-tok".into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map: Some(mm),
        }],
    };
    let state = std::sync::Arc::new(tokio::sync::RwLock::new(
        zone_router::state::AppState::new(config, std::path::PathBuf::from("/tmp/test-rw.toml"))
            .unwrap(),
    ));
    let router = zone_router::proxy::server::build_router(state);

    let writer = CaptureWriter::new();
    let subscriber = make_capture_subscriber(writer.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", "rewrite-tok")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"claude-sonnet-4-20250514","messages":[]}"#,
        ))
        .unwrap();

    let _resp = router.oneshot(req).await.unwrap();
    drop(_guard);

    let output = writer.contents();
    assert!(
        output.contains("model rewritten"),
        "model rewrite should emit debug log. Got: {output}"
    );
    assert!(
        output.contains("DEBUG"),
        "model rewrite should be at DEBUG level. Got: {output}"
    );
    assert!(
        output.contains("claude-sonnet-4-20250514"),
        "model rewrite log should include original model. Got: {output}"
    );
    assert!(
        output.contains("custom-model-v1"),
        "model rewrite log should include rewritten model. Got: {output}"
    );
}

#[tokio::test]
async fn shutdown_logs_info() {
    let writer = CaptureWriter::new();
    let subscriber = make_capture_subscriber(writer.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let state = common::make_state(vec![("sd-be", "http://127.0.0.1:1", "tok")], "sd-tok");

    let mut handle = tokio::spawn(async {});

    zone_router::proxy::server::force_shutdown(state, &mut handle).await;
    drop(_guard);

    let output = writer.contents();
    assert!(
        output.contains("shutting down"),
        "shutdown should emit info log. Got: {output}"
    );
    assert!(
        output.contains("INFO"),
        "shutdown should be at INFO level. Got: {output}"
    );
}

#[tokio::test]
async fn startup_logs_info() {
    let writer = CaptureWriter::new();
    let subscriber = make_capture_subscriber(writer.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    // Call the production helper extracted from main.rs
    zone_router::logging::log_server_started("127.0.0.1:8080");
    drop(_guard);

    let output = writer.contents();
    assert!(
        output.contains("server started"),
        "startup should emit info log. Got: {output}"
    );
    assert!(
        output.contains("INFO"),
        "startup should be at INFO level. Got: {output}"
    );
    assert!(
        output.contains("127.0.0.1:8080"),
        "startup log should include address. Got: {output}"
    );
}
