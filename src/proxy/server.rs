use crate::state::AppState;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tower::{MakeService, ServiceExt};

const BODY_LIMIT: usize = 200 * 1024 * 1024; // 200MB
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub fn build_router(state: Arc<RwLock<AppState>>) -> Router {
    Router::new()
        .fallback(super::handler::proxy_handler)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

struct TowerToHyperService<S>(S);

impl<S> hyper::service::Service<hyper::Request<hyper::body::Incoming>> for TowerToHyperService<S>
where
    S: tower::Service<axum::http::Request<axum::body::Body>> + Clone + Send + 'static,
    S::Response: axum::response::IntoResponse,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    S::Future: Send + 'static,
{
    type Response = axum::response::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: hyper::Request<hyper::body::Incoming>) -> Self::Future {
        let mut svc = self.0.clone();
        Box::pin(async move {
            let req = req.map(axum::body::Body::new);
            let resp = svc
                .ready()
                .await
                .map_err(Into::into)?
                .call(req)
                .await
                .map_err(Into::into)?;
            Ok(resp.into_response())
        })
    }
}

pub async fn start_with_listener(
    state: Arc<RwLock<AppState>>,
    listener: tokio::net::TcpListener,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = build_router(state);
    let mut make_svc = router.into_make_service();
    let mut conn_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    let shutdown = async {
        while !*shutdown_rx.borrow() {
            if shutdown_rx.changed().await.is_err() {
                break;
            }
        }
    };
    tokio::pin!(shutdown);

    loop {
        let stream = tokio::select! {
            biased;
            () = &mut shutdown => break,
            result = listener.accept() => match result {
                Ok((stream, _)) => stream,
                Err(e) => { eprintln!("accept error: {e}"); continue; }
            },
        };

        let svc = MakeService::<(), hyper::Request<hyper::body::Incoming>>::make_service(
            &mut make_svc,
            (),
        )
        .await
        .map_err(|e| format!("make_service error: {e:?}"))?;

        // Prune completed connection handles to avoid unbounded growth.
        conn_handles.retain(|h| !h.is_finished());

        conn_handles.push(tokio::spawn(async move {
            let io = TokioIo::new(stream);
            if let Err(e) = http1::Builder::new()
                .keep_alive(false)
                .serve_connection(io, TowerToHyperService(svc))
                .await
            {
                eprintln!("connection error: {e}");
            }
        }));
    }

    // Wait for all in-flight connections to complete (graceful drain).
    for handle in conn_handles {
        let _ = handle.await;
    }

    // Keep the function alive (and the listener bound) until force_shutdown
    // aborts this task, so the port remains reachable during the grace period.
    futures_util::future::pending::<()>().await;

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
