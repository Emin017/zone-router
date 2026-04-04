use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_LOG_ENTRIES: usize = 500;
const MAX_BODY_PREVIEW: usize = 4096;

static NEXT_ENTRY_ID: AtomicU64 = AtomicU64::new(1);

/// Ordered collection of HTTP header name-value pairs.
#[derive(Debug, Clone, Default)]
pub struct HeaderPairs(pub Vec<(String, String)>);

/// Captured HTTP request data.
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub headers: HeaderPairs,
    pub body: Option<String>,
}

/// Captured HTTP response data.
#[derive(Debug, Clone)]
pub struct CapturedResponse {
    pub status: u16,
    pub headers: HeaderPairs,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RequestLogEntry {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub backend: String,
    pub latency_ms: u64,
    pub request: CapturedRequest,
    pub response: CapturedResponse,
}

impl RequestLogEntry {
    pub fn new(
        timestamp: DateTime<Utc>,
        backend: String,
        latency_ms: u64,
        request: CapturedRequest,
        response: CapturedResponse,
    ) -> Self {
        Self {
            id: NEXT_ENTRY_ID.fetch_add(1, Ordering::Relaxed),
            timestamp,
            backend,
            latency_ms,
            request,
            response,
        }
    }
}

/// Truncate a body string to `MAX_BODY_PREVIEW` bytes for storage in log entries.
/// Preserves existing truncation markers from upstream capture so the original
/// byte count is not replaced with the intermediate string length.
pub fn truncate_body_for_log(body: Option<String>) -> Option<String> {
    body.map(|s| {
        if s.len() <= MAX_BODY_PREVIEW {
            s
        } else if s.contains("... (truncated,") || s.contains("...(truncated,") {
            // Already has a truncation marker from capture_body — keep the
            // original total but trim the preview portion to fit.
            let marker_pos = s
                .rfind("... (truncated,")
                .or_else(|| s.rfind("...(truncated,"))
                .unwrap();
            let marker = &s[marker_pos..];
            let budget = MAX_BODY_PREVIEW.saturating_sub(marker.len());
            let mut end = budget.min(marker_pos);
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}{}", &s[..end], marker)
        } else {
            let mut end = MAX_BODY_PREVIEW;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...(truncated, {} bytes total)", &s[..end], s.len())
        }
    })
}

#[derive(Debug, Clone, Default)]
pub struct BackendStats {
    pub total_requests: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub total_latency_ms: u64,
}

impl BackendStats {
    pub fn avg_latency_ms(&self) -> f64 {
        if self.total_requests > 0 {
            self.total_latency_ms as f64 / self.total_requests as f64
        } else {
            0.0
        }
    }

    pub fn record(&mut self, status: u16, latency_ms: u64) {
        self.total_requests += 1;
        self.total_latency_ms += latency_ms;
        if (200..400).contains(&status) {
            self.success_count += 1;
        } else {
            self.error_count += 1;
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatsCollector {
    pub log: VecDeque<RequestLogEntry>,
    pub per_backend: HashMap<String, BackendStats>,
}

impl Default for StatsCollector {
    fn default() -> Self {
        Self {
            log: VecDeque::with_capacity(MAX_LOG_ENTRIES),
            per_backend: HashMap::new(),
        }
    }
}

impl StatsCollector {
    pub fn record(&mut self, entry: RequestLogEntry) {
        self.per_backend
            .entry(entry.backend.clone())
            .or_default()
            .record(entry.response.status, entry.latency_ms);
        if self.log.len() >= MAX_LOG_ENTRIES {
            self.log.pop_front();
        }
        self.log.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_stats_avg_latency() {
        let mut stats = BackendStats::default();
        stats.record(200, 100);
        stats.record(200, 300);
        assert_eq!(stats.avg_latency_ms(), 200.0);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn backend_stats_error_counting() {
        let mut stats = BackendStats::default();
        stats.record(200, 100);
        stats.record(500, 50);
        stats.record(401, 30);
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.error_count, 2);
    }

    #[test]
    fn backend_stats_empty_avg() {
        assert_eq!(BackendStats::default().avg_latency_ms(), 0.0);
    }

    fn test_entry(
        backend: &str,
        method: &str,
        path: &str,
        status: u16,
        latency_ms: u64,
    ) -> RequestLogEntry {
        RequestLogEntry::new(
            Utc::now(),
            backend.into(),
            latency_ms,
            CapturedRequest {
                method: method.into(),
                path: path.into(),
                headers: HeaderPairs::default(),
                body: None,
            },
            CapturedResponse {
                status,
                headers: HeaderPairs::default(),
                body: None,
            },
        )
    }

    #[test]
    fn stats_collector_caps_at_max() {
        let mut collector = StatsCollector::default();
        for i in 0..700 {
            collector.record(test_entry("test", "POST", "/v1/messages", 200, i));
        }
        assert_eq!(collector.log.len(), MAX_LOG_ENTRIES);
        assert_eq!(collector.log.front().unwrap().latency_ms, 200);
    }

    #[test]
    fn stats_collector_tracks_per_backend() {
        let mut collector = StatsCollector::default();
        collector.record(test_entry("a", "POST", "/", 200, 100));
        collector.record(test_entry("b", "POST", "/", 500, 50));
        assert_eq!(collector.per_backend["a"].success_count, 1);
        assert_eq!(collector.per_backend["b"].error_count, 1);
    }
}
