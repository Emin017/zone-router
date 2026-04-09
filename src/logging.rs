use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: u64,
    pub timestamp: chrono::DateTime<chrono::Local>,
    pub level: Level,
    pub target: String,
    pub message: String,
}

impl fmt::Display for LogEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}] {} {}",
            self.timestamp.format("%H:%M:%S"),
            self.level,
            self.target,
            self.message,
        )
    }
}

// Maximum number of log entries buffered in the TUI channel.
// When the channel is full the sender drops new events (lossy but bounded).
const TUI_CHANNEL_CAP: usize = 500;

pub struct TuiLogLayer {
    tx: mpsc::Sender<LogEntry>,
    counter: AtomicU64,
}

impl TuiLogLayer {
    pub fn new(tx: mpsc::Sender<LogEntry>) -> Self {
        Self {
            tx,
            counter: AtomicU64::new(0),
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push(format!("{}={}", field.name(), value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push(format!("{}={}", field.name(), value));
    }
}

impl MessageVisitor {
    fn into_full_message(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else {
            format!("{} {}", self.message, self.fields.join(" "))
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for TuiLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let entry = LogEntry {
            id: self.counter.fetch_add(1, Ordering::Relaxed),
            timestamp: chrono::Local::now(),
            level: *event.metadata().level(),
            target: event.metadata().target().to_owned(),
            message: visitor.into_full_message(),
        };

        // Silently drop if the channel is full or the receiver is gone.
        let _ = self.tx.try_send(entry);
    }
}

/// Emit the startup info log with the bound listen address.
pub fn log_server_started(addr: &str) {
    tracing::info!(addr = %addr, "server started");
}

/// Probe whether `log_dir` can be created and written to.
/// Returns `true` only if both `create_dir_all` and a temp file write succeed.
fn is_writable_dir(log_dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(log_dir).is_err() {
        return false;
    }
    let probe = log_dir.join(".zone-router-write-probe");
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Initialise the global tracing subscriber with a file layer and a TUI channel layer.
///
/// Returns the log receiver for the TUI, a guard that must be held for the
/// lifetime of the program, and the log directory path if file logging is active.
pub fn init_tracing() -> (
    mpsc::Receiver<LogEntry>,
    tracing_appender::non_blocking::WorkerGuard,
    Option<std::path::PathBuf>,
) {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{EnvFilter, fmt};

    let (tx, rx) = mpsc::channel(TUI_CHANNEL_CAP);

    // When RUST_LOG is set, both layers use it; otherwise each uses its own default.
    let rust_log = std::env::var("RUST_LOG").ok();
    let make_filter = |default: &str| -> EnvFilter {
        rust_log
            .as_deref()
            .and_then(|v| EnvFilter::try_new(v).ok())
            .unwrap_or_else(|| EnvFilter::new(default))
    };

    // File layer — daily rolling under XDG state dir.
    // Gracefully skip the file layer if the log directory cannot be created
    // or is not writable (e.g. containers, restricted service accounts).
    let writable_log_dir = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .map(|base| base.join("zone-router/logs"))
        .filter(|dir| is_writable_dir(dir));

    let (file_layer, guard, log_dir_out) = if let Some(log_dir) = writable_log_dir {
        let file_appender = tracing_appender::rolling::daily(&log_dir, "zone-router.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let file_filter = make_filter("zone_router=debug");
        let layer = fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_filter(file_filter);
        (Some(layer), guard, Some(log_dir))
    } else {
        // Log dir unavailable — TUI layer still works, file layer skipped.
        // A sink guard keeps the return type uniform.
        let (_sink_nb, sink_guard) = tracing_appender::non_blocking(std::io::sink());
        (None, sink_guard, None)
    };

    // TUI layer — sends LogEntry structs through the channel.
    let tui_filter = make_filter("zone_router=info");
    let tui_layer = TuiLogLayer::new(tx).with_filter(tui_filter);

    tracing_subscriber::registry()
        .with(file_layer)
        .with(tui_layer)
        .init();

    (rx, guard, log_dir_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::prelude::*;

    #[test]
    fn tui_log_layer_sends_entries() {
        let (tx, mut rx) = mpsc::channel(100);
        let layer = TuiLogLayer::new(tx);

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "zone_router::test", "hello from test");
        });

        let entry = rx.try_recv().expect("should receive a log entry");
        assert_eq!(entry.id, 0);
        assert_eq!(entry.level, Level::INFO);
        assert_eq!(entry.target, "zone_router::test");
        assert_eq!(entry.message, "hello from test");
    }

    #[test]
    fn monotonic_id_increments() {
        let (tx, mut rx) = mpsc::channel(100);
        let layer = TuiLogLayer::new(tx);

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "zone_router::test", "first");
            tracing::warn!(target: "zone_router::test", "second");
            tracing::error!(target: "zone_router::test", "third");
        });

        let e1 = rx.try_recv().unwrap();
        let e2 = rx.try_recv().unwrap();
        let e3 = rx.try_recv().unwrap();
        assert_eq!(e1.id, 0);
        assert_eq!(e2.id, 1);
        assert_eq!(e3.id, 2);
        assert_eq!(e2.level, Level::WARN);
        assert_eq!(e3.level, Level::ERROR);
    }

    #[test]
    fn dropped_receiver_does_not_panic() {
        let (tx, rx) = mpsc::channel(100);
        let layer = TuiLogLayer::new(tx);
        drop(rx);

        let subscriber = tracing_subscriber::registry().with(layer);
        // This must not panic even though the receiver is gone.
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "zone_router::test", "should not panic");
        });
    }

    #[test]
    fn default_tui_filter_suppresses_debug() {
        let (tx, mut rx) = mpsc::channel(100);
        // Construct the default filter directly — no ambient RUST_LOG involved.
        let filter = EnvFilter::new("zone_router=info");
        let layer = TuiLogLayer::new(tx).with_filter(filter);

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "zone_router::test", "debug msg");
            tracing::info!(target: "zone_router::test", "info msg");
        });

        // Only the info event should arrive; the debug event is filtered out.
        let entry = rx.try_recv().expect("should receive the info entry");
        assert_eq!(entry.level, Level::INFO);
        assert_eq!(entry.message, "info msg");
        assert!(rx.try_recv().is_err(), "no more entries expected");
    }

    #[test]
    fn non_zone_router_target_suppressed_by_default() {
        let (tx, mut rx) = mpsc::channel(100);
        // Construct the default filter directly — no ambient RUST_LOG involved.
        let filter = EnvFilter::new("zone_router=info");
        let layer = TuiLogLayer::new(tx).with_filter(filter);

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "hyper::server", "hyper event");
            tracing::info!(target: "zone_router::test", "zone_router event");
        });

        // Only the zone_router event should arrive; hyper is excluded by default.
        let entry = rx.try_recv().expect("should receive zone_router entry");
        assert_eq!(entry.target, "zone_router::test");
        assert!(rx.try_recv().is_err(), "no more entries expected");
    }

    #[test]
    fn override_filter_includes_external_targets() {
        let (tx, mut rx) = mpsc::channel(100);
        // Simulate RUST_LOG=info — override includes all targets at info level.
        let filter = EnvFilter::new("info");
        let layer = TuiLogLayer::new(tx).with_filter(filter);

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "hyper::server", "hyper event");
            tracing::info!(target: "zone_router::test", "zone_router event");
        });

        // Both events should arrive when override is active.
        let e1 = rx.try_recv().expect("should receive first entry");
        let e2 = rx.try_recv().expect("should receive second entry");
        assert_eq!(e1.target, "hyper::server");
        assert_eq!(e2.target, "zone_router::test");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn file_layer_creates_log_file_and_flushes_on_guard_drop() {
        use std::fs;

        let dir = tempfile::tempdir().expect("temp dir");
        let file_appender = tracing_appender::rolling::daily(dir.path(), "test-zone-router.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = {
            use tracing_subscriber::fmt;
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(EnvFilter::new("zone_router=debug"))
        };

        let subscriber = tracing_subscriber::registry().with(file_layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "zone_router::test", "file test message");
            tracing::debug!(target: "zone_router::test", "debug also written");
        });

        // Drop the guard to flush the non-blocking writer.
        drop(guard);

        // Verify a log file was created in the temp directory.
        let entries: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(!entries.is_empty(), "at least one log file should exist");

        // Verify file content contains our messages.
        let content = fs::read_to_string(entries[0].path()).expect("read log file");
        assert!(
            content.contains("file test message"),
            "info message in file"
        );
        assert!(
            content.contains("debug also written"),
            "debug message in file"
        );
    }

    #[test]
    fn file_default_filter_keeps_debug_excludes_trace() {
        use std::fs;

        let dir = tempfile::tempdir().expect("temp dir");
        let file_appender = tracing_appender::rolling::daily(dir.path(), "test-filter.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = {
            use tracing_subscriber::fmt;
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(EnvFilter::new("zone_router=debug"))
        };

        let subscriber = tracing_subscriber::registry().with(file_layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "zone_router::test", "trace msg");
            tracing::debug!(target: "zone_router::test", "debug msg");
        });

        drop(guard);

        let log_path = fs::read_dir(dir.path())
            .expect("read dir")
            .find_map(|e| e.ok())
            .expect("log file")
            .path();
        let content = fs::read_to_string(&log_path).expect("read");

        assert!(
            content.contains("debug msg"),
            "DEBUG should be kept at zone_router=debug"
        );
        assert!(
            !content.contains("trace msg"),
            "TRACE should be excluded at zone_router=debug"
        );
    }

    #[test]
    fn file_default_filter_excludes_non_zone_router() {
        use std::fs;

        let dir = tempfile::tempdir().expect("temp dir");
        let file_appender = tracing_appender::rolling::daily(dir.path(), "test-target.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = {
            use tracing_subscriber::fmt;
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(EnvFilter::new("zone_router=debug"))
        };

        let subscriber = tracing_subscriber::registry().with(file_layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "hyper::server", "hyper log");
            tracing::info!(target: "zone_router::test", "zone_router log");
        });

        drop(guard);

        let log_path = fs::read_dir(dir.path())
            .expect("read dir")
            .find_map(|e| e.ok())
            .expect("log file")
            .path();
        let content = fs::read_to_string(&log_path).expect("read");

        assert!(
            content.contains("zone_router log"),
            "zone_router target should be included"
        );
        assert!(
            !content.contains("hyper log"),
            "hyper target should be excluded by default"
        );
    }

    #[test]
    fn file_override_filter_includes_external_targets() {
        use std::fs;

        let dir = tempfile::tempdir().expect("temp dir");
        let file_appender = tracing_appender::rolling::daily(dir.path(), "test-override.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        // Simulate RUST_LOG=info — override includes all targets.
        let file_layer = {
            use tracing_subscriber::fmt;
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(EnvFilter::new("info"))
        };

        let subscriber = tracing_subscriber::registry().with(file_layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "hyper::server", "hyper in file");
            tracing::info!(target: "zone_router::test", "zone_router in file");
        });

        drop(guard);

        let log_path = fs::read_dir(dir.path())
            .expect("read dir")
            .find_map(|e| e.ok())
            .expect("log file")
            .path();
        let content = fs::read_to_string(&log_path).expect("read");

        assert!(
            content.contains("hyper in file"),
            "override filter should include external targets"
        );
        assert!(
            content.contains("zone_router in file"),
            "override filter should include zone_router"
        );
    }

    #[test]
    fn daily_rolling_filename_pattern() {
        use std::fs;

        let dir = tempfile::tempdir().expect("temp dir");
        let prefix = "test-rolling.log";
        let file_appender = tracing_appender::rolling::daily(dir.path(), prefix);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = {
            use tracing_subscriber::fmt;
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(EnvFilter::new("zone_router=debug"))
        };

        let subscriber = tracing_subscriber::registry().with(file_layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "zone_router::test", "filename test");
        });

        drop(guard);

        let log_path = fs::read_dir(dir.path())
            .expect("read dir")
            .find_map(|e| e.ok())
            .expect("log file")
            .path();

        let filename = log_path.file_name().unwrap().to_string_lossy();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert!(
            filename.starts_with(prefix) && filename.contains(&today),
            "daily rolling filename should be '{prefix}.{today}', got '{filename}'"
        );
    }

    /// This test calls the real `init_tracing()` twice, which panics on the
    /// second `.init()` call. It is isolated in a subprocess by the
    /// `logging_subprocess` integration test so the global subscriber doesn't
    /// poison the rest of the suite.
    #[test]
    #[should_panic(expected = "a global default trace dispatcher has already been set")]
    fn double_init_tracing_panics() {
        let _ = crate::logging::init_tracing();
        // Second call: .init() calls set_global_default which returns Err,
        // which .init() unwraps, causing a panic.
        let _ = crate::logging::init_tracing();
    }
}
