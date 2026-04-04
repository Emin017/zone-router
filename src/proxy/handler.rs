use crate::config::{AuthType, ModelMap};
use crate::state::AppState;
use crate::stats::{CapturedRequest, CapturedResponse, HeaderPairs, RequestLogEntry};
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

fn make_log_entry(
    backend: &str,
    start: Instant,
    request: CapturedRequest,
    response: CapturedResponse,
) -> RequestLogEntry {
    RequestLogEntry {
        timestamp: Utc::now(),
        backend: backend.to_owned(),
        latency_ms: start.elapsed().as_millis() as u64,
        request,
        response,
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

fn extract_header_pairs(headers: &HeaderMap) -> HeaderPairs {
    HeaderPairs(
        headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_owned(), v.to_owned()))
            })
            .collect(),
    )
}

const MAX_SSE_EVENTS: usize = 20;

/// Find the first blank-line delimiter in `buf`, returning the byte offset
/// just past the delimiter. Handles both `\n\n` and `\r\n\r\n`.
fn find_blank_line(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\r'
            && i + 3 < buf.len()
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some(i + 4);
        }
        if buf[i] == b'\n' && i + 1 < buf.len() && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// A stream wrapper that buffers the first N SSE events and signals completion on drop.
///
/// Buffers raw bytes to handle UTF-8 code points split across chunk boundaries.
/// Recognises both LF (`\n\n`) and CRLF (`\r\n\r\n`) blank-line event delimiters.
struct SseBufferingStream<S> {
    inner: S,
    done_tx: Option<tokio::sync::oneshot::Sender<Option<String>>>,
    /// Raw byte buffer for accumulated preview content (first N events).
    preview: Vec<u8>,
    /// Carry buffer holding bytes not yet terminated by a blank line.
    carry: Vec<u8>,
    event_count: usize,
}

impl<S> Stream for SseBufferingStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll = Pin::new(&mut self.inner).poll_next(cx);
        if let Poll::Ready(Some(Ok(ref chunk))) = poll {
            self.carry.extend_from_slice(chunk);

            while let Some(delim_end) = find_blank_line(&self.carry) {
                let event_bytes = self.carry[..delim_end].to_vec();
                self.carry = self.carry[delim_end..].to_vec();

                self.event_count += 1;
                if self.event_count <= MAX_SSE_EVENTS {
                    self.preview.extend_from_slice(&event_bytes);
                }
            }
        }
        poll.map(|opt| opt.map(|r| r.map_err(std::io::Error::other)))
    }
}

impl<S> Drop for SseBufferingStream<S> {
    fn drop(&mut self) {
        if !self.carry.is_empty() {
            self.event_count += 1;
            if self.event_count <= MAX_SSE_EVENTS {
                self.preview.extend_from_slice(&self.carry);
            }
        }

        if let Some(tx) = self.done_tx.take() {
            let body = if self.event_count > MAX_SSE_EVENTS {
                let text = String::from_utf8_lossy(&self.preview);
                Some(format!(
                    "{}\n... (truncated, {} events total)",
                    text.trim_end(),
                    self.event_count
                ))
            } else if self.preview.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&self.preview).into_owned())
            };
            let _ = tx.send(body);
        }
    }
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

    let req_headers = extract_header_pairs(&headers);

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

    let req_body = if body_bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&body_bytes).into_owned())
    };

    let (body_bytes, body_changed) = match backend.model_map.as_ref().filter(|mm| mm.has_any()) {
        Some(mm) => {
            let original_ptr = body_bytes.as_ptr();
            let rewritten = rewrite_model(body_bytes, mm);
            let changed = rewritten.as_ptr() != original_ptr;
            (rewritten, changed)
        }
        None => (body_bytes, false),
    };

    // Strip body-dependent headers only when the payload actually changed;
    // reqwest will recalculate Content-Length from the actual body.
    let forwarded_headers = if body_changed {
        let mut h = forwarded_headers;
        h.remove(reqwest::header::CONTENT_LENGTH);
        h.remove("content-md5");
        h.remove("digest");
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

            let resp_headers = extract_header_pairs(resp.headers());

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
                let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Option<String>>();
                let wrapped = SseBufferingStream {
                    inner: resp.bytes_stream(),
                    done_tx: Some(done_tx),
                    preview: Vec::new(),
                    carry: Vec::new(),
                    event_count: 0,
                };
                let body = Body::from_stream(wrapped);

                tokio::spawn(async move {
                    let sse_body = done_rx.await.unwrap_or(None);
                    let entry = make_log_entry(
                        &backend_name,
                        start,
                        CapturedRequest {
                            method: method_str,
                            path,
                            headers: req_headers,
                            body: req_body,
                        },
                        CapturedResponse {
                            status,
                            headers: resp_headers,
                            body: sse_body,
                        },
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
                let resp_body_str = if resp_body_bytes.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&resp_body_bytes).into_owned())
                };
                let entry = make_log_entry(
                    &backend_name,
                    start,
                    CapturedRequest {
                        method: method_str,
                        path,
                        headers: req_headers,
                        body: req_body,
                    },
                    CapturedResponse {
                        status,
                        headers: resp_headers,
                        body: resp_body_str,
                    },
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
                CapturedRequest {
                    method: method_str,
                    path,
                    headers: req_headers,
                    body: req_body,
                },
                CapturedResponse {
                    status: 502,
                    headers: HeaderPairs::default(),
                    body: None,
                },
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
