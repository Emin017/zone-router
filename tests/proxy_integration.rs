use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
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
    assert!(
        String::from_utf8(body.to_vec())
            .unwrap()
            .contains("version=2024-01-01")
    );
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
    assert_eq!(s.stats.log[0].status, 200);
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
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let state = make_state(vec![("fwd", &format!("http://{addr}"), "tok")], "secret");
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
