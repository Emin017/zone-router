use crate::state::AppState;
use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tower::{Service, ServiceExt};

const BODY_LIMIT: usize = 200 * 1024 * 1024; // 200MB
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub fn build_router(state: Arc<RwLock<AppState>>) -> Router {
    Router::new()
        .fallback(super::handler::proxy_handler)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Resolves when the watch channel signals `true` (shutdown requested).
/// If the sender is dropped, parks forever so callers don't spin.
async fn wait_for_shutdown(rx: &mut tokio::sync::watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            futures_util::future::pending::<()>().await;
        }
    }
}

struct TowerToHyperService<S>(S);

impl<S> hyper::service::Service<hyper::Request<hyper::body::Incoming>> for TowerToHyperService<S>
where
    S: tower::Service<axum::http::Request<axum::body::Body>> + Clone + Send + 'static,
    S::Response: IntoResponse,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    S::Future: Send + 'static,
{
    type Response = axum::response::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: hyper::Request<hyper::body::Incoming>) -> Self::Future {
        let mut svc = self.0.clone();
        Box::pin(async move {
            let resp = svc
                .ready()
                .await
                .map_err(Into::into)?
                .call(req.map(axum::body::Body::new))
                .await
                .map_err(Into::into)?;
            Ok(resp.into_response())
        })
    }
}

pub async fn start_with_listener(
    state: Arc<RwLock<AppState>>,
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut make_svc = build_router(state).into_make_service();
    // JoinSet aborts all tasks on drop, so if force_shutdown aborts this
    // task, all child connection tasks are also terminated.
    let mut conns = tokio::task::JoinSet::new();

    let mut accept_rx = shutdown_rx.clone();
    tokio::pin!(let shutdown = wait_for_shutdown(&mut accept_rx););

    loop {
        let stream = tokio::select! {
            biased;
            () = &mut shutdown => break,
            result = listener.accept() => match result {
                Ok((stream, _)) => stream,
                Err(e) => {
                    eprintln!("accept error: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            },
        };

        let svc = Service::<()>::call(&mut make_svc, ())
            .await
            .map_err(|e| format!("make_service error: {e:?}"))?;

        // Reap completed connection tasks to avoid unbounded memory growth.
        while conns.try_join_next().is_some() {}

        let mut conn_rx = shutdown_rx.clone();
        conns.spawn(async move {
            let builder = auto::Builder::new(TokioExecutor::new());
            let mut conn =
                Box::pin(builder.serve_connection(TokioIo::new(stream), TowerToHyperService(svc)));

            tokio::select! {
                result = &mut conn => {
                    if let Err(e) = result {
                        eprintln!("connection error: {e}");
                    }
                }
                () = wait_for_shutdown(&mut conn_rx) => {
                    conn.as_mut().graceful_shutdown();
                    if let Err(e) = conn.await {
                        eprintln!("connection error: {e}");
                    }
                }
            }
        });
    }

    // Close the listener so new TCP connects are refused immediately,
    // then wait for in-flight connections to drain.
    drop(listener);
    while conns.join_next().await.is_some() {}

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
