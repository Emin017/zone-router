use crate::state::AppState;
use axum::extract::DefaultBodyLimit;
use axum::Router;
use std::sync::Arc;
use tokio::sync::RwLock;

const BODY_LIMIT: usize = 200 * 1024 * 1024; // 200MB

pub fn build_router(state: Arc<RwLock<AppState>>) -> Router {
    Router::new()
        .fallback(super::handler::proxy_handler)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

pub async fn start(
    state: Arc<RwLock<AppState>>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listen_addr = state.read().await.config.proxy.listen.clone();
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
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
