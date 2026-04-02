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
