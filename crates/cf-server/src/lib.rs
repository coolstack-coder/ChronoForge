//! The ChronoForge backend: JSON/HTTP for the UI, gRPC for tools.

pub mod api;
pub mod explain;
// tonic::Status is big; that's tonic's design, not ours
#[allow(clippy::result_large_err)]
pub mod grpc;
pub mod hunt;
pub mod session;

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use axum::Router;
use cf_trace::store::TraceStore;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::session::Session;

/// Keep this many sessions in memory; older ones live on in the store.
const MAX_SESSIONS: usize = 48;

pub struct AppState {
    pub sessions: RwLock<BTreeMap<String, Arc<Session>>>,
    order: RwLock<VecDeque<String>>,
    pub hunts: RwLock<BTreeMap<String, Arc<hunt::Hunt>>>,
    pub store: Arc<dyn TraceStore>,
    pub examples_dir: PathBuf,
    pub native_dir: PathBuf,
    counter: AtomicU64,
}

impl AppState {
    pub fn new(store: Arc<dyn TraceStore>, examples_dir: PathBuf, native_dir: PathBuf) -> Self {
        AppState {
            sessions: RwLock::default(),
            order: RwLock::default(),
            hunts: RwLock::default(),
            store,
            examples_dir,
            native_dir,
            counter: AtomicU64::new(1),
        }
    }

    pub fn new_id(&self, prefix: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut rng = cf_vm::SplitMix64::new(nanos ^ n);
        format!("{prefix}-{:06x}", rng.next_u64() & 0xff_ffff)
    }

    pub fn get(&self, id: &str) -> Result<Arc<Session>, api::ApiError> {
        self.sessions
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| api_not_found(id))
    }

    /// Store a session and return its summary.
    pub fn insert(&self, s: Session) -> Value {
        if let Err(e) = self.store.put(&s.id, &s.recording, &s.steps) {
            tracing::warn!("could not persist session {}: {e}", s.id);
        }
        let summary =
            serde_json::to_value(s.summary()).unwrap_or_else(|e| json!({"error": e.to_string()}));
        let id = s.id.clone();
        self.sessions
            .write()
            .unwrap()
            .insert(id.clone(), Arc::new(s));
        let mut order = self.order.write().unwrap();
        order.push_back(id);
        while order.len() > MAX_SESSIONS {
            if let Some(old) = order.pop_front() {
                self.sessions.write().unwrap().remove(&old);
            }
        }
        summary
    }
}

fn api_not_found(id: &str) -> api::ApiError {
    api::ApiError::not_found(&format!("session {id}"))
}

impl api::ApiError {
    pub fn not_found(what: &str) -> Self {
        api::ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            json!({"error": format!("{what} not found")}),
        )
    }
}

pub struct Config {
    pub http: SocketAddr,
    pub grpc: Option<SocketAddr>,
    pub ui_dir: Option<PathBuf>,
    pub state: Arc<AppState>,
}

pub fn app(state: Arc<AppState>, ui_dir: Option<PathBuf>) -> Router {
    let mut router = api::router(state).layer(CorsLayer::permissive());
    if let Some(dir) = ui_dir.filter(|d| d.join("index.html").exists()) {
        let index = dir.join("index.html");
        router =
            router.fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(index)));
    }
    router
}

pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    let app = app(cfg.state.clone(), cfg.ui_dir);
    let listener = tokio::net::TcpListener::bind(cfg.http).await?;
    tracing::info!("http listening on http://{}", listener.local_addr()?);

    let grpc = cfg.grpc.map(|addr| {
        let svc = grpc::ChronoForgeServer::new(grpc::Service {
            state: cfg.state.clone(),
        });
        tracing::info!("grpc listening on {addr}");
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(svc)
                .serve(addr)
                .await
        })
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    if let Some(g) = grpc {
        g.abort();
    }
    Ok(())
}
