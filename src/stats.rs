use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};

const MAX_LOG_ENTRIES: usize = 1000;

#[derive(Debug, Clone)]
pub struct RequestLogEntry {
    pub timestamp: DateTime<Utc>,
    pub backend: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub latency_ms: u64,
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
            .record(entry.status, entry.latency_ms);
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

    #[test]
    fn stats_collector_caps_at_max() {
        let mut collector = StatsCollector::default();
        for i in 0..1100 {
            collector.record(RequestLogEntry {
                timestamp: Utc::now(),
                backend: "test".into(),
                method: "POST".into(),
                path: "/v1/messages".into(),
                status: 200,
                latency_ms: i,
            });
        }
        assert_eq!(collector.log.len(), MAX_LOG_ENTRIES);
        assert_eq!(collector.log.front().unwrap().latency_ms, 100);
    }

    #[test]
    fn stats_collector_tracks_per_backend() {
        let mut collector = StatsCollector::default();
        collector.record(RequestLogEntry {
            timestamp: Utc::now(),
            backend: "a".into(),
            method: "POST".into(),
            path: "/".into(),
            status: 200,
            latency_ms: 100,
        });
        collector.record(RequestLogEntry {
            timestamp: Utc::now(),
            backend: "b".into(),
            method: "POST".into(),
            path: "/".into(),
            status: 500,
            latency_ms: 50,
        });
        assert_eq!(collector.per_backend["a"].success_count, 1);
        assert_eq!(collector.per_backend["b"].error_count, 1);
    }
}
