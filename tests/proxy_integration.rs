use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream;
use tower::ServiceExt;

mod common;
use common::make_state;

async fn start_mock_backend() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/messages",
        post(|headers: axum::http::HeaderMap, body: String| async move {
            let token = headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none");
            format!("token={token},body={body}")
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
async fn unauthorized_without_token() {
    let state = make_state(vec![("test", "http://localhost:1", "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unauthorized_with_wrong_token() {
    let state = make_state(vec![("test", "http://localhost:1", "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "wrong-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn service_unavailable_no_backends() {
    let state = make_state(vec![], "secret");
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

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn forwards_to_backend_with_token_replacement() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(
        vec![("mock", &backend_url, "real-backend-token")],
        "local-secret",
    );
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "local-secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"claude"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("token=real-backend-token"));
    assert!(body_str.contains(r#"body={"model":"claude"}"#));
}

#[tokio::test]
async fn bad_gateway_on_unreachable_backend() {
    let state = make_state(vec![("dead", "http://127.0.0.1:1", "tok")], "secret");
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

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn headers_pass_through() {
    let app = Router::new().route(
        "/v1/messages",
        post(|headers: axum::http::HeaderMap| async move {
            let version = headers
                .get("anthropic-version")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("missing");
            format!("version={version}")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = make_state(vec![("hdr", &format!("http://{addr}"), "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("anthropic-version", "2024-01-01")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(String::from_utf8(body.to_vec())
        .unwrap()
        .contains("version=2024-01-01"));
}

#[tokio::test]
async fn logs_request_after_completion() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(vec![("log-test", &backend_url, "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state.clone());

    let _resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::from("test"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Give async log recording a moment
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    assert_eq!(s.stats.log[0].backend, "log-test");
    assert_eq!(s.stats.log[0].response.status, 200);
}

// --- Bearer inbound auth tests ---

#[tokio::test]
async fn bearer_auth_accepted() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(vec![("mock", &backend_url, "tok")], "local-secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Bearer local-secret")
                .body(Body::from("test"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn bearer_auth_wrong_token_rejected() {
    let state = make_state(vec![("test", "http://localhost:1", "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Bearer wrong-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn basic_auth_rejected() {
    let state = make_state(vec![("test", "http://localhost:1", "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Basic secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn x_api_key_takes_precedence_over_bearer() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(vec![("mock", &backend_url, "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    // Valid x-api-key + invalid Bearer → should succeed (x-api-key wins)
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("authorization", "Bearer wrong")
                .body(Body::from("test"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_x_api_key_with_valid_bearer_rejected() {
    let state = make_state(vec![("test", "http://localhost:1", "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    // Invalid x-api-key + valid Bearer → should fail (x-api-key takes precedence)
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "wrong")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// --- Case-insensitive Bearer parsing ---

#[tokio::test]
async fn bearer_auth_case_insensitive() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(vec![("mock", &backend_url, "tok")], "local-secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "bearer local-secret")
                .body(Body::from("test"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn bearer_auth_mixed_case() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(vec![("mock", &backend_url, "tok")], "local-secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "BEARER local-secret")
                .body(Body::from("test"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

// --- Client Authorization header stripping ---

#[tokio::test]
async fn client_authorization_stripped_when_authed_via_api_key() {
    let app = Router::new().route(
        "/v1/messages",
        post(|headers: axum::http::HeaderMap| async move {
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("missing");
            format!("authorization={auth}")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = make_state(vec![("fwd", &format!("http://{addr}"), "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("authorization", "Bearer user-jwt-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body_str.contains("authorization=missing"),
        "client Authorization header should be stripped even when authed via x-api-key, got: {body_str}"
    );
}

#[tokio::test]
async fn client_authorization_stripped_when_authed_via_bearer() {
    // When client authenticates via Authorization: Bearer, their auth header
    // must be stripped (it's the proxy auth, not a passthrough credential)
    let (backend_url, _handle) = start_auth_echo_backend().await;

    let state = make_state(vec![("fwd", &backend_url, "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    // The backend uses ApiKey auth_type by default, so outbound sets x-api-key.
    // The client's Authorization (which was used for proxy auth) must be stripped.
    assert!(
        body_str.contains("x-api-key=tok"),
        "should set outbound x-api-key, got: {body_str}"
    );
}

// --- Outbound auth type tests ---

/// Mock backend that echoes both x-api-key and authorization headers.
async fn start_auth_echo_backend() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/messages",
        post(|headers: axum::http::HeaderMap| async move {
            let api_key = headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none");
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none");
            format!("x-api-key={api_key},authorization={auth}")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

fn make_state_with_auth_type(
    name: &str,
    url: &str,
    token: &str,
    local_token: &str,
    auth_type: zone_router::config::AuthType,
) -> std::sync::Arc<tokio::sync::RwLock<zone_router::state::AppState>> {
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: local_token.into(),
        },
        backends: vec![zone_router::config::Backend {
            name: name.into(),
            url: url.into(),
            token: token.into(),
            active: true,
            auth_type,
            model_map: None,
        }],
    };
    std::sync::Arc::new(tokio::sync::RwLock::new(
        zone_router::state::AppState::new(
            config,
            std::path::PathBuf::from("/tmp/test-auth-type.toml"),
        )
        .unwrap(),
    ))
}

#[tokio::test]
async fn outbound_api_key_auth_type() {
    let (backend_url, _handle) = start_auth_echo_backend().await;
    let state = make_state_with_auth_type(
        "test",
        &backend_url,
        "real-token",
        "secret",
        zone_router::config::AuthType::ApiKey,
    );
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

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body_str.contains("x-api-key=real-token"),
        "should send x-api-key header, got: {body_str}"
    );
    assert!(
        body_str.contains("authorization=none"),
        "should NOT send authorization header, got: {body_str}"
    );
}

#[tokio::test]
async fn outbound_bearer_auth_type() {
    let (backend_url, _handle) = start_auth_echo_backend().await;
    let state = make_state_with_auth_type(
        "test",
        &backend_url,
        "real-token",
        "secret",
        zone_router::config::AuthType::Bearer,
    );
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

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body_str.contains("authorization=Bearer real-token"),
        "should send Authorization: Bearer header, got: {body_str}"
    );
    assert!(
        body_str.contains("x-api-key=none"),
        "should NOT send x-api-key header, got: {body_str}"
    );
}

// --- Model map rewriting tests ---

async fn start_body_echo_backend() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route("/v1/messages", post(|body: String| async move { body }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

fn make_state_with_model_map(
    name: &str,
    url: &str,
    token: &str,
    local_token: &str,
    model_map: Option<zone_router::config::ModelMap>,
) -> std::sync::Arc<tokio::sync::RwLock<zone_router::state::AppState>> {
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: local_token.into(),
        },
        backends: vec![zone_router::config::Backend {
            name: name.into(),
            url: url.into(),
            token: token.into(),
            active: true,
            auth_type: zone_router::config::AuthType::default(),
            model_map,
        }],
    };
    std::sync::Arc::new(tokio::sync::RwLock::new(
        zone_router::state::AppState::new(
            config,
            std::path::PathBuf::from("/tmp/test-model-map.toml"),
        )
        .unwrap(),
    ))
}

#[tokio::test]
async fn model_map_rewrites_sonnet() {
    let (backend_url, _handle) = start_body_echo_backend().await;
    let mm = zone_router::config::ModelMap {
        haiku: None,
        sonnet: Some("glm-5-turbo".into()),
        opus: None,
    };
    let state = make_state_with_model_map("mm", &backend_url, "tok", "secret", Some(mm));
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"claude-sonnet-4-20250514","messages":[]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["model"], "glm-5-turbo");
    assert_eq!(v["messages"], serde_json::json!([]));
}

#[tokio::test]
async fn no_model_map_passes_body_unchanged() {
    let (backend_url, _handle) = start_body_echo_backend().await;
    let state = make_state_with_model_map("plain", &backend_url, "tok", "secret", None);
    let router = zone_router::proxy::server::build_router(state);

    let original = r#"{"model":"claude-sonnet-4-20250514","messages":[]}"#;
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(original))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), original.as_bytes());
}

#[tokio::test]
async fn model_map_no_tier_match_passes_through() {
    let (backend_url, _handle) = start_body_echo_backend().await;
    let mm = zone_router::config::ModelMap {
        haiku: Some("h".into()),
        sonnet: Some("s".into()),
        opus: Some("o".into()),
    };
    let state = make_state_with_model_map("mm", &backend_url, "tok", "secret", Some(mm));
    let router = zone_router::proxy::server::build_router(state);

    let original = r#"{"model":"gpt-4o","messages":[]}"#;
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(original))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["model"], "gpt-4o");
}

#[tokio::test]
async fn partial_model_map_only_rewrites_matching_tier() {
    let (backend_url, _handle) = start_body_echo_backend().await;
    let mm = zone_router::config::ModelMap {
        haiku: None,
        sonnet: Some("glm-5-turbo".into()),
        opus: None,
    };
    let state = make_state_with_model_map("mm", &backend_url, "tok", "secret", Some(mm));

    // Sonnet should be rewritten
    let router = zone_router::proxy::server::build_router(state.clone());
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"claude-sonnet-4-20250514"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["model"], "glm-5-turbo");

    // Opus should pass through (not mapped)
    let router = zone_router::proxy::server::build_router(state);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"claude-opus-4-6"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["model"], "claude-opus-4-6");
}

#[tokio::test]
async fn non_json_body_passes_through() {
    let (backend_url, _handle) = start_body_echo_backend().await;
    let mm = zone_router::config::ModelMap {
        haiku: Some("h".into()),
        sonnet: Some("s".into()),
        opus: Some("o".into()),
    };
    let state = make_state_with_model_map("mm", &backend_url, "tok", "secret", Some(mm));
    let router = zone_router::proxy::server::build_router(state);

    let original = "this is not json";
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::from(original))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), original.as_bytes());
}

#[tokio::test]
async fn json_without_model_field_passes_through() {
    let (backend_url, _handle) = start_body_echo_backend().await;
    let mm = zone_router::config::ModelMap {
        haiku: Some("h".into()),
        sonnet: Some("s".into()),
        opus: Some("o".into()),
    };
    let state = make_state_with_model_map("mm", &backend_url, "tok", "secret", Some(mm));
    let router = zone_router::proxy::server::build_router(state);

    let original = r#"{"messages":[]}"#;
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(original))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["messages"], serde_json::json!([]));
}

#[tokio::test]
async fn model_map_rewrites_body_for_sse_response() {
    // Backend that validates the forwarded body model field and returns SSE
    let app = Router::new().route(
        "/v1/messages",
        post(|body: String| async move {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            let model = v["model"].as_str().unwrap().to_string();
            let events = vec![
                Ok::<_, std::convert::Infallible>(Event::default().data(format!("model={model}"))),
                Ok(Event::default().data("[DONE]")),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mm = zone_router::config::ModelMap {
        haiku: None,
        sonnet: Some("glm-5-turbo".into()),
        opus: None,
    };
    let state = make_state_with_model_map(
        "sse-mm",
        &format!("http://{addr}"),
        "tok",
        "secret",
        Some(mm),
    );
    let router = zone_router::proxy::server::build_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"claude-sonnet-4-20250514","stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    // The backend echoes the model it received — should be the rewritten name
    assert!(
        body_str.contains("model=glm-5-turbo"),
        "SSE backend should receive rewritten model, got: {body_str}"
    );
}

// --- Captured request/response data tests ---

#[tokio::test]
async fn captures_post_request_headers_and_body() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(vec![("cap", &backend_url, "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state.clone());

    let _resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    let entry = &s.stats.log[0];
    assert!(
        !entry.request.headers.0.is_empty(),
        "request headers should be non-empty"
    );
    assert!(
        entry.request.body.is_some(),
        "POST request body should be Some"
    );
    assert!(
        entry.request.body.as_deref().unwrap().contains("model"),
        "request body should contain the sent JSON"
    );
}

#[tokio::test]
async fn captures_get_request_with_no_body() {
    let app = Router::new().route("/v1/models", get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = make_state(
        vec![("get-test", &format!("http://{addr}"), "tok")],
        "secret",
    );
    let router = zone_router::proxy::server::build_router(state.clone());

    let _resp = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("x-api-key", "secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    let entry = &s.stats.log[0];
    assert!(
        entry.request.body.is_none(),
        "GET with no body should have request.body=None"
    );
}

#[tokio::test]
async fn captures_response_headers_and_body() {
    let (backend_url, _handle) = start_mock_backend().await;
    let state = make_state(vec![("resp-test", &backend_url, "tok")], "secret");
    let router = zone_router::proxy::server::build_router(state.clone());

    let _resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", "secret")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    let entry = &s.stats.log[0];
    assert!(
        !entry.response.headers.0.is_empty(),
        "response headers should be captured"
    );
    assert!(
        entry.response.body.is_some(),
        "response body should be captured for non-SSE responses"
    );
}

// --- SSE preview truncation tests ---

async fn start_sse_backend(event_count: usize) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/messages",
        post(move |_body: String| async move {
            let events: Vec<_> = (0..event_count)
                .map(|i| {
                    Ok::<_, std::convert::Infallible>(Event::default().data(format!("event-{i}")))
                })
                .collect();
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
async fn sse_under_20_events_captures_all() {
    let (backend_url, _handle) = start_sse_backend(5).await;
    let state = make_state(vec![("sse5", &backend_url, "tok")], "secret");
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

    // Consume the full SSE stream so LogOnDrop fires
    let _body = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    let entry = &s.stats.log[0];
    let body = entry.response.body.as_deref().unwrap();
    assert!(
        body.contains("event-0"),
        "should contain first event, got: {body}"
    );
    assert!(
        body.contains("event-4"),
        "should contain last event, got: {body}"
    );
    assert!(
        !body.contains("truncated"),
        "<=20 events should not have truncation marker, got: {body}"
    );
}

#[tokio::test]
async fn sse_over_20_events_truncates_with_marker() {
    let (backend_url, _handle) = start_sse_backend(30).await;
    let state = make_state(vec![("sse30", &backend_url, "tok")], "secret");
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

    let _body = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    let entry = &s.stats.log[0];
    let body = entry.response.body.as_deref().unwrap();
    assert!(
        body.contains("event-0"),
        "should contain first event, got: {body}"
    );
    assert!(
        body.contains("truncated"),
        ">20 events should have truncation marker, got: {body}"
    );
    assert!(
        body.contains("30 events total"),
        "should report total event count, got: {body}"
    );
    // Events beyond 20 should NOT be in the buffer
    assert!(
        !body.contains("event-25"),
        "should not contain events beyond the 20th, got: {body}"
    );
}

#[tokio::test]
async fn sse_multi_data_line_event_counts_as_one() {
    // A single SSE event can have multiple data: lines.
    // Each event is delimited by a blank line (\n\n).
    // We need 25 events where each has 3 data: lines — still only 25 logical events.
    let app = Router::new().route(
        "/v1/messages",
        post(|| async {
            let mut raw = String::new();
            for i in 0..25 {
                // Each event has 3 data: lines but is one logical event
                raw.push_str(&format!(
                    "data: line-a-{i}\ndata: line-b-{i}\ndata: line-c-{i}\n\n"
                ));
            }
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                raw,
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = make_state(
        vec![("sse-multi", &format!("http://{addr}"), "tok")],
        "secret",
    );
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

    let _body = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let s = state.read().await;
    assert_eq!(s.stats.log.len(), 1);
    let entry = &s.stats.log[0];
    let body = entry.response.body.as_deref().unwrap();

    // First event should be fully captured (all 3 data: lines)
    assert!(
        body.contains("line-a-0"),
        "first event's first data line should be present, got: {body}"
    );
    assert!(
        body.contains("line-c-0"),
        "first event's last data line should be present, got: {body}"
    );

    // Event 19 (0-indexed) should be captured (the 20th event)
    assert!(
        body.contains("line-a-19"),
        "20th event should be captured, got: {body}"
    );

    // Event 20 (the 21st) should NOT be in the buffer
    assert!(
        !body.contains("line-a-20"),
        "21st event should NOT be captured, got: {body}"
    );

    // Should have truncation marker with 25 total events (not 75 data: lines)
    assert!(
        body.contains("truncated"),
        "should have truncation marker, got: {body}"
    );
    assert!(
        body.contains("25 events total"),
        "should count 25 logical events, not 75 data: markers, got: {body}"
    );
}
