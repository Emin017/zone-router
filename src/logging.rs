use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

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

pub struct TuiLogLayer {
    tx: mpsc::UnboundedSender<LogEntry>,
    counter: AtomicU64,
}

impl TuiLogLayer {
    pub fn new(tx: mpsc::UnboundedSender<LogEntry>) -> Self {
        Self {
            tx,
            counter: AtomicU64::new(0),
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
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
            message: visitor.message,
        };

        // Silently drop if the receiver is gone (TUI exited before proxy).
        let _ = self.tx.send(entry);
    }
}

/// Initialise the global tracing subscriber with a file layer and a TUI channel layer.
///
/// Returns the log receiver for the TUI and a guard that must be held for the
/// lifetime of the program (dropping it flushes and closes the file writer).
pub fn init_tracing() -> (
    mpsc::UnboundedReceiver<LogEntry>,
    tracing_appender::non_blocking::WorkerGuard,
) {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    let (tx, rx) = mpsc::unbounded_channel();

    // When RUST_LOG is set, both layers use it; otherwise each uses its own default.
    let rust_log = std::env::var("RUST_LOG").ok();
    let make_filter = |default: &str| -> EnvFilter {
        rust_log
            .as_deref()
            .and_then(|v| EnvFilter::try_new(v).ok())
            .unwrap_or_else(|| EnvFilter::new(default))
    };

    // File layer — daily rolling under XDG state dir.
    let log_dir = dirs::state_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("zone-router/logs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "zone-router.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_filter = make_filter("zone_router=debug");
    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(file_filter);

    // TUI layer — sends LogEntry structs through the channel.
    let tui_filter = make_filter("zone_router=info");
    let tui_layer = TuiLogLayer::new(tx).with_filter(tui_filter);

    tracing_subscriber::registry()
        .with(file_layer)
        .with(tui_layer)
        .init();

    (rx, guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    #[test]
    fn tui_log_layer_sends_entries() {
        let (tx, mut rx) = mpsc::unbounded_channel();
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
        let (tx, mut rx) = mpsc::unbounded_channel();
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
        let (tx, rx) = mpsc::unbounded_channel();
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
        let (tx, mut rx) = mpsc::unbounded_channel();
        let filter = std::env::var("RUST_LOG")
            .ok()
            .and_then(|v| EnvFilter::try_new(v).ok())
            .unwrap_or_else(|| EnvFilter::new("zone_router=info"));
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
        let (tx, mut rx) = mpsc::unbounded_channel();
        let filter = std::env::var("RUST_LOG")
            .ok()
            .and_then(|v| EnvFilter::try_new(v).ok())
            .unwrap_or_else(|| EnvFilter::new("zone_router=info"));
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
}
