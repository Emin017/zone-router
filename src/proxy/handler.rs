use crate::config::{AuthType, ModelMap};
use crate::state::AppState;
use crate::stats::{RequestLogEntry, TokenUsage, TransferType};
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::Utc;
use futures_util::Stream;
use std::pin::Pin;
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

#[allow(clippy::too_many_arguments)]
fn make_log_entry(
    backend: &str,
    start: Instant,
    method: String,
    path: String,
    status: u16,
    model: Option<String>,
    transfer_type: TransferType,
    usage: Option<TokenUsage>,
) -> RequestLogEntry {
    RequestLogEntry::new(
        Utc::now(),
        backend.to_owned(),
        start.elapsed().as_millis() as u64,
        method,
        path,
        status,
        model,
        transfer_type,
        usage,
    )
}

fn extract_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from))
}

fn extract_usage(body: &[u8]) -> Option<TokenUsage> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let usage = v.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage.get("input_tokens")?.as_u64()?,
        output_tokens: usage.get("output_tokens")?.as_u64()?,
    })
}

const SSE_TAIL_CAP: usize = 8 * 1024;

struct SseTailStream<S> {
    inner: S,
    tail: Vec<u8>,
    done_tx: Option<tokio::sync::oneshot::Sender<Option<TokenUsage>>>,
}

impl<S> Stream for SseTailStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll = Pin::new(&mut self.inner).poll_next(cx);
        if let Poll::Ready(Some(Ok(ref chunk))) = poll {
            self.tail.extend_from_slice(chunk);
            if self.tail.len() > SSE_TAIL_CAP {
                let start = self.tail.len() - SSE_TAIL_CAP;
                self.tail = self.tail[start..].to_vec();
            }
        }
        poll.map(|opt| opt.map(|r| r.map_err(std::io::Error::other)))
    }
}

impl<S> Drop for SseTailStream<S> {
    fn drop(&mut self) {
        if let Some(tx) = self.done_tx.take() {
            let usage = extract_usage_from_tail(&self.tail);
            let _ = tx.send(usage);
        }
    }
}

fn extract_usage_from_tail(tail: &[u8]) -> Option<TokenUsage> {
    let text = std::str::from_utf8(tail).ok()?;
    for line in text.lines().rev() {
        let payload = match line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            Some(p) => p,
            None => continue,
        };
        if !payload.contains("usage") {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(payload).ok()?;
        let usage = v.get("usage")?;
        return Some(TokenUsage {
            input_tokens: usage.get("input_tokens")?.as_u64()?,
            output_tokens: usage.get("output_tokens")?.as_u64()?,
        });
    }
    None
}

fn rewrite_model(body: Bytes, mm: &ModelMap) -> (Bytes, bool) {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (body, false);
    };
    let mut changed = false;

    // Top-level model field (single message requests)
    if let Some(model_str) = value.get("model").and_then(|v| v.as_str()) {
        if let Some(replacement) = mm.resolve(model_str) {
            value["model"] = serde_json::Value::String(replacement.to_owned());
            changed = true;
        }
    }

    // Batch requests: requests[*].params.model
    if let Some(requests) = value.get_mut("requests").and_then(|v| v.as_array_mut()) {
        for req in requests.iter_mut() {
            if let Some(model_str) = req
                .get("params")
                .and_then(|p| p.get("model"))
                .and_then(|v| v.as_str())
            {
                if let Some(replacement) = mm.resolve(model_str) {
                    req["params"]["model"] = serde_json::Value::String(replacement.to_owned());
                    changed = true;
                }
            }
        }
    }

    if !changed {
        return (body, false);
    }
    match serde_json::to_vec(&value) {
        Ok(v) => (Bytes::from(v), true),
        Err(_) => (body, false),
    }
}

pub async fn proxy_handler(
    State(state): State<std::sync::Arc<RwLock<AppState>>>,
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

    let request_token = match headers.get(AUTH_HEADER) {
        Some(v) => v.to_str().unwrap_or(""),
        None => headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.get(7..)
                    .filter(|_| v[..7].eq_ignore_ascii_case("Bearer "))
            })
            .unwrap_or(""),
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

    let (body_bytes, body_changed) = match backend.model_map.as_ref().filter(|mm| mm.has_any()) {
        Some(mm) => {
            let (rewritten, changed) = rewrite_model(body_bytes, mm);
            (rewritten, changed)
        }
        None => (body_bytes, false),
    };

    let model = extract_model(&body_bytes);

    // Strip body-dependent headers only when the payload actually changed;
    // reqwest will recalculate Content-Length from the actual body.
    let forwarded_headers = if body_changed {
        let mut h = forwarded_headers;
        h.remove(reqwest::header::CONTENT_LENGTH);
        h.remove("content-md5");
        h.remove("digest");
        h.remove("content-digest");
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
                let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Option<TokenUsage>>();
                let wrapped = SseTailStream {
                    inner: resp.bytes_stream(),
                    tail: Vec::new(),
                    done_tx: Some(done_tx),
                };
                let body = Body::from_stream(wrapped);

                tokio::spawn(async move {
                    let usage = done_rx.await.unwrap_or(None);
                    let entry = make_log_entry(
                        &backend_name,
                        start,
                        method_str,
                        path,
                        status,
                        model,
                        TransferType::Streaming,
                        usage,
                    );
                    state_clone.write().await.stats.record(entry);
                });

                (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                    response_headers,
                    body,
                )
                    .into_response()
            } else {
                let resp_body_bytes = resp.bytes().await.unwrap_or_default();
                let usage = extract_usage(&resp_body_bytes);
                let entry = make_log_entry(
                    &backend_name,
                    start,
                    method_str,
                    path,
                    status,
                    model,
                    TransferType::Json,
                    usage,
                );
                state.write().await.stats.record(entry);

                (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                    response_headers,
                    resp_body_bytes,
                )
                    .into_response()
            }
        }
        Err(_) => {
            let entry = make_log_entry(
                &backend_name,
                start,
                method_str,
                path,
                502,
                model,
                TransferType::Json,
                None,
            );
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
