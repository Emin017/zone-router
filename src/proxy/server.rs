use crate::state::AppState;
use axum::extract::DefaultBodyLimit;
use axum::Router;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const BODY_LIMIT: usize = 200 * 1024 * 1024; // 200MB
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub fn build_router(state: Arc<RwLock<AppState>>) -> Router {
    Router::new()
        .fallback(super::handler::proxy_handler)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

pub async fn start_with_listener(
    state: Arc<RwLock<AppState>>,
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = build_router(state);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let mut rx = shutdown_rx;
            while !*rx.borrow() {
                let _ = rx.changed().await;
            }
        })
        .await?;
    Ok(())
}

/// Waits up to 5 seconds for the server to shut down gracefully, then
/// force-aborts it. Sets the shutdown flag on state before waiting.
pub async fn force_shutdown(
    state: Arc<RwLock<AppState>>,
    server_handle: &mut tokio::task::JoinHandle<()>,
) {
    state.write().await.shutdown = true;
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut *server_handle)
        .await
        .is_err()
    {
        server_handle.abort();
    }
}
