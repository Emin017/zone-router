use crate::config::{AuthType, ModelMap};
use crate::state::AppState;
use crate::stats::RequestLogEntry;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::Utc;
use futures_util::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::sync::RwLock;

const AUTH_HEADER: &str = "x-api-key";
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .no_proxy()
        .build()
        .expect("failed to build HTTP client")
}

std::thread_local! {
    static CLIENT: reqwest::Client = build_client();
}

/// A stream wrapper that sends a signal when dropped (stream fully consumed or connection closed).
struct LogOnDropStream<S> {
    inner: S,
    done_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl<S> Stream for LogOnDropStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner)
            .poll_next(cx)
            .map(|opt| opt.map(|r| r.map_err(std::io::Error::other)))
    }
}

impl<S> Drop for LogOnDropStream<S> {
    fn drop(&mut self) {
        if let Some(tx) = self.done_tx.take() {
            let _ = tx.send(());
        }
    }
}

fn make_log_entry(
    backend: &str,
    method: &str,
    path: &str,
    status: u16,
    start: Instant,
) -> RequestLogEntry {
    RequestLogEntry {
        timestamp: Utc::now(),
        backend: backend.to_owned(),
        method: method.to_owned(),
        path: path.to_owned(),
        status,
        latency_ms: start.elapsed().as_millis() as u64,
    }
}

fn rewrite_model(body: Bytes, mm: &ModelMap) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(model_str) = value.get("model").and_then(|v| v.as_str()) else {
        return body;
    };
    let Some(replacement) = mm.resolve(model_str) else {
        return body;
    };
    value["model"] = serde_json::Value::String(replacement.to_owned());
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

pub async fn proxy_handler(
    State(state): State<Arc<RwLock<AppState>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let start = Instant::now();

    let (local_token, backend_info) = {
        let s = state.read().await;
        if s.shutdown {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let backend = s.active_backend().cloned();
        (s.local_token.clone(), backend)
    };

    let request_token = if headers.get(AUTH_HEADER).is_some() {
        headers
            .get(AUTH_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    } else {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.get(7..)
                    .filter(|_| v[..7].eq_ignore_ascii_case("Bearer "))
            })
            .unwrap_or("")
    };

    if request_token != local_token {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let backend = match backend_info {
        Some(b) => b,
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let target_url = format!(
        "{}{}",
        backend.url.trim_end_matches('/'),
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
    );

    let forwarded_headers: reqwest::header::HeaderMap = headers
        .iter()
        .filter(|(name, _)| {
            let n = name.as_str().to_lowercase();
            n != AUTH_HEADER && n != "host" && n != "authorization"
        })
        .filter_map(|(name, value)| {
            let n = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()).ok()?;
            let v = reqwest::header::HeaderValue::from_bytes(value.as_bytes()).ok()?;
            Some((n, v))
        })
        .collect();

    let body_bytes = match axum::body::to_bytes(body, 200 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let may_rewrite = matches!(backend.model_map, Some(ref mm) if mm.has_any());
    let body_bytes = if may_rewrite {
        rewrite_model(body_bytes, backend.model_map.as_ref().unwrap())
    } else {
        body_bytes
    };

    // Strip content-length when the body may have been rewritten to a different
    // size; reqwest will recalculate it from the actual body.
    let forwarded_headers = if may_rewrite {
        let mut h = forwarded_headers;
        h.remove(reqwest::header::CONTENT_LENGTH);
        h
    } else {
        forwarded_headers
    };

    let client = CLIENT.with(|c| c.clone());
    let auth_header = match backend.auth_type {
        AuthType::ApiKey => (AUTH_HEADER, backend.token.clone()),
        AuthType::Bearer => ("authorization", format!("Bearer {}", backend.token)),
    };
    let response = client
        .request(reqwest_method(&method), &target_url)
        .headers(forwarded_headers)
        .header(auth_header.0, auth_header.1)
        .body(body_bytes)
        .send()
        .await;

    let backend_name = backend.name.clone();
    let path = uri.path().to_string();
    let method_str = method.to_string();

    match response {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let is_stream = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.contains("text/event-stream"));

            let response_headers: HeaderMap = resp
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    let n = axum::http::HeaderName::from_bytes(name.as_str().as_bytes()).ok()?;
                    let v = HeaderValue::from_bytes(value.as_bytes()).ok()?;
                    Some((n, v))
                })
                .collect();

            if is_stream {
                let state_clone = state.clone();
                let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
                let wrapped = LogOnDropStream {
                    inner: resp.bytes_stream(),
                    done_tx: Some(done_tx),
                };
                let body = Body::from_stream(wrapped);

                tokio::spawn(async move {
                    let _ = done_rx.await;
                    let entry = make_log_entry(&backend_name, &method_str, &path, status, start);
                    state_clone.write().await.stats.record(entry);
                });

                (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                    response_headers,
                    body,
                )
                    .into_response()
            } else {
                let resp_body = resp.bytes().await.unwrap_or_default();
                let entry = make_log_entry(&backend_name, &method_str, &path, status, start);
                state.write().await.stats.record(entry);

                (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                    response_headers,
                    resp_body,
                )
                    .into_response()
            }
        }
        Err(_) => {
            let entry = make_log_entry(&backend_name, &method_str, &path, 502, start);
            state.write().await.stats.record(entry);
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

fn reqwest_method(method: &Method) -> reqwest::Method {
    match *method {
        Method::GET => reqwest::Method::GET,
        Method::POST => reqwest::Method::POST,
        Method::PUT => reqwest::Method::PUT,
        Method::DELETE => reqwest::Method::DELETE,
        Method::PATCH => reqwest::Method::PATCH,
        Method::HEAD => reqwest::Method::HEAD,
        Method::OPTIONS => reqwest::Method::OPTIONS,
        _ => reqwest::Method::GET,
    }
}
