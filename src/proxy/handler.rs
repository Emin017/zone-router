use crate::config::{AuthType, ModelMap};
use crate::state::AppState;
use crate::stats::{
    truncate_body_for_log, CapturedRequest, CapturedResponse, HeaderPairs, RequestLogEntry,
};
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
    mut request: CapturedRequest,
    req_original_size: usize,
    mut response: CapturedResponse,
    resp_original_size: usize,
) -> RequestLogEntry {
    request.body = truncate_body_for_log(request.body, req_original_size);
    response.body = truncate_body_for_log(response.body, resp_original_size);
    RequestLogEntry::new(
        Utc::now(),
        backend.to_owned(),
        start.elapsed().as_millis() as u64,
        request,
        response,
    )
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

fn extract_header_pairs(headers: &HeaderMap) -> HeaderPairs {
    HeaderPairs(
        headers
            .iter()
            .filter(|(name, _)| {
                let n = name.as_str().to_lowercase();
                !matches!(
                    n.as_str(),
                    "x-api-key" | "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
                )
            })
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
const MAX_CAPTURED_BODY_BYTES: usize = 256 * 1024; // 256 KB

fn capture_body(raw: &[u8]) -> (Option<String>, usize) {
    if raw.is_empty() {
        return (None, 0);
    }
    let original_size = raw.len();
    if raw.len() <= MAX_CAPTURED_BODY_BYTES {
        (
            Some(String::from_utf8_lossy(raw).into_owned()),
            original_size,
        )
    } else {
        let truncated = String::from_utf8_lossy(&raw[..MAX_CAPTURED_BODY_BYTES]);
        (Some(truncated.into_owned()), original_size)
    }
}

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
    /// Set when carry was flushed due to overflow, so the next delimiter
    /// still counts the oversized frame as one data event.
    carry_flushed_data: bool,
    /// Small tail buffer for cross-chunk delimiter detection after done_buffering.
    /// Retains at most 3 trailing bytes (\r\n\r is the longest partial delimiter).
    tail: Vec<u8>,
}

/// Returns true if the SSE frame contains a `data:` field line,
/// meaning it carries actual event payload rather than being a
/// comment-only or empty keepalive frame.
fn is_data_event(frame: &[u8]) -> bool {
    for line in frame.split(|&b| b == b'\n') {
        let trimmed = if line.first() == Some(&b'\r') {
            &line[1..]
        } else {
            line
        };
        if trimmed.starts_with(b"data:") || trimmed == b"data" {
            return true;
        }
    }
    false
}

impl<S> Stream for SseBufferingStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll = Pin::new(&mut self.inner).poll_next(cx);
        if let Poll::Ready(Some(Ok(ref chunk))) = poll {
            // Stop accumulating into carry once we've exceeded both the event
            // cap and byte cap — there's nothing left to buffer.
            let done_buffering =
                self.event_count >= MAX_SSE_EVENTS && self.preview.len() >= MAX_CAPTURED_BODY_BYTES;

            if done_buffering {
                // Still need to count data events for the truncation marker,
                // but don't grow carry unboundedly. Use self.tail to carry
                // partial delimiters across chunk boundaries.
                self.tail.extend_from_slice(chunk);
                while let Some(delim_end) = find_blank_line(&self.tail) {
                    let frame = &self.tail[..delim_end];
                    if is_data_event(frame) {
                        self.event_count += 1;
                    }
                    self.tail = self.tail[delim_end..].to_vec();
                }
                // Keep only the last 3 bytes for cross-chunk delimiter matching
                if self.tail.len() > 3 {
                    let start = self.tail.len() - 3;
                    self.tail = self.tail[start..].to_vec();
                }
            } else {
                self.carry.extend_from_slice(chunk);

                while let Some(delim_end) = find_blank_line(&self.carry) {
                    let event_bytes = self.carry[..delim_end].to_vec();
                    self.carry = self.carry[delim_end..].to_vec();

                    if self.carry_flushed_data || is_data_event(&event_bytes) {
                        self.event_count += 1;
                        self.carry_flushed_data = false;
                    } else {
                        continue;
                    }
                    if self.event_count <= MAX_SSE_EVENTS
                        && self.preview.len() < MAX_CAPTURED_BODY_BYTES
                    {
                        let remaining_cap = MAX_CAPTURED_BODY_BYTES - self.preview.len();
                        if event_bytes.len() <= remaining_cap {
                            self.preview.extend_from_slice(&event_bytes);
                        } else {
                            self.preview
                                .extend_from_slice(&event_bytes[..remaining_cap]);
                        }
                    }
                }

                // If carry itself has grown past the cap with no delimiter in sight,
                // flush what we can into preview and discard the excess bytes.
                // Record that we flushed data so the next delimiter still counts
                // this oversized frame as one event.
                if self.carry.len() > MAX_CAPTURED_BODY_BYTES {
                    let has_data = is_data_event(&self.carry);
                    if self.event_count < MAX_SSE_EVENTS
                        && self.preview.len() < MAX_CAPTURED_BODY_BYTES
                    {
                        let remaining_cap = MAX_CAPTURED_BODY_BYTES - self.preview.len();
                        let to_take = self.carry.len().min(remaining_cap);
                        let flush = self.carry[..to_take].to_vec();
                        self.preview.extend_from_slice(&flush);
                    }
                    self.carry.clear();
                    if has_data {
                        self.carry_flushed_data = true;
                    }
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
            if self.event_count <= MAX_SSE_EVENTS && self.preview.len() < MAX_CAPTURED_BODY_BYTES {
                let remaining_cap = MAX_CAPTURED_BODY_BYTES - self.preview.len();
                let to_append = self.carry.len().min(remaining_cap);
                self.preview.extend_from_slice(&self.carry[..to_append]);
            }
        }

        if let Some(tx) = self.done_tx.take() {
            let byte_capped = self.preview.len() >= MAX_CAPTURED_BODY_BYTES;
            let body = if self.event_count > MAX_SSE_EVENTS || byte_capped {
                let text = String::from_utf8_lossy(&self.preview);
                let reason = if self.event_count > MAX_SSE_EVENTS {
                    format!("{} events total", self.event_count)
                } else {
                    format!("{} bytes total", self.preview.len())
                };
                Some(format!("{}\n... (truncated, {reason})", text.trim_end(),))
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

    let (req_body, req_original_size) = capture_body(&body_bytes);

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

    // Capture request headers for logging after stripping stale body-dependent
    // headers, so the detail panel stays consistent with the rewritten body.
    let mut req_headers = extract_header_pairs(&headers);
    if body_changed {
        req_headers.0.retain(|(name, _)| {
            !matches!(
                name.as_str(),
                "content-length" | "content-md5" | "digest" | "content-digest"
            )
        });
    }

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
                    carry_flushed_data: false,
                    tail: Vec::new(),
                };
                let body = Body::from_stream(wrapped);

                tokio::spawn(async move {
                    let sse_body = done_rx.await.unwrap_or(None);
                    // SSE bodies are already capped and carry their own truncation
                    // marker from SseBufferingStream::drop. Pass the preview length
                    // as original_size so truncate_body_for_log preserves the
                    // existing marker text rather than replacing it with a byte count.
                    let sse_size = sse_body.as_ref().map_or(0, |s| s.len());
                    let entry = make_log_entry(
                        &backend_name,
                        start,
                        CapturedRequest {
                            method: method_str,
                            path,
                            headers: req_headers,
                            body: req_body,
                        },
                        req_original_size,
                        CapturedResponse {
                            status,
                            headers: resp_headers,
                            body: sse_body,
                        },
                        sse_size,
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
                let (resp_body_str, resp_original_size) = capture_body(&resp_body_bytes);
                let entry = make_log_entry(
                    &backend_name,
                    start,
                    CapturedRequest {
                        method: method_str,
                        path,
                        headers: req_headers,
                        body: req_body,
                    },
                    req_original_size,
                    CapturedResponse {
                        status,
                        headers: resp_headers,
                        body: resp_body_str,
                    },
                    resp_original_size,
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
                req_original_size,
                CapturedResponse {
                    status: 502,
                    headers: HeaderPairs::default(),
                    body: None,
                },
                0,
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
