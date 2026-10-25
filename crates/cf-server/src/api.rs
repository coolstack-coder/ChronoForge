use std::collections::BTreeMap;
use std::path::Path as FsPath;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cf_trace::Loc;
use cf_vm::RecordOptions;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::session::{BuildError, Origin, Session};
use crate::{hunt, AppState};

pub struct ApiError(StatusCode, Value);

impl ApiError {
    pub fn new(status: StatusCode, body: Value) -> Self {
        ApiError(status, body)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<BuildError> for ApiError {
    fn from(e: BuildError) -> Self {
        match e {
            BuildError::Compile(c) => ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"error": c.to_string(), "line": c.line, "col": c.col, "message": c.message}),
            ),
            BuildError::Engine(m) => {
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": m}))
            }
        }
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError::not_found(what)
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, json!({"error": msg.into()}))
}

type R<T> = Result<Json<T>, ApiError>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/examples", get(examples))
        .route("/api/compile", post(compile))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/import", post(import_session))
        .route("/api/sessions/{id}", get(session_summary))
        .route("/api/sessions/{id}/timeline", get(timeline))
        .route("/api/sessions/{id}/state", get(state_at))
        .route("/api/sessions/{id}/analysis", get(analysis))
        .route("/api/sessions/{id}/history", get(history))
        .route("/api/sessions/{id}/diff/{other}", get(diff))
        .route("/api/sessions/{id}/explain", post(explain))
        .route("/api/sessions/{id}/export", get(export))
        .route("/api/hunt", post(create_hunt))
        .route("/api/hunt/{id}", get(get_hunt))
        .route("/api/hunt/{id}/open/{seed}", post(open_hunt_run))
        .route("/api/native", get(native_list))
        .route("/api/native/{id}", get(native_get))
        .route("/api/native/{id}/replay", post(native_replay))
        .with_state(state)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("blocking task panicked")
}

async fn health(State(s): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "engine": cf_vm::ENGINE,
        "store": s.store.backend(),
        "native": cfg!(target_os = "linux"),
        "os": std::env::consts::OS,
        "sessions": s.sessions.read().unwrap().len(),
        "ai": std::env::var("ANTHROPIC_API_KEY").map(|k| !k.is_empty()).unwrap_or(false),
    }))
}

/// The first comment block of an example doubles as its description.
fn describe_example(src: &str) -> (String, String) {
    let comments: Vec<&str> = src
        .lines()
        .take_while(|l| l.trim_start().starts_with("//"))
        .map(|l| l.trim_start_matches('/').trim())
        .collect();
    let title = comments.first().copied().unwrap_or("").to_string();
    let rest = comments.get(1..).unwrap_or(&[]).join(" ");
    (title, rest)
}

async fn examples(State(s): State<Arc<AppState>>) -> R<Vec<Value>> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&s.examples_dir)
        .map_err(|e| bad(format!("examples dir: {e}")))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "forge"))
        .collect();
    // a deliberate order for the UI: the headline bug first
    let rank = |p: &FsPath| {
        let n = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        ["bank", "philosophers", "checkout", "token", "counter"]
            .iter()
            .position(|x| *x == n)
            .unwrap_or(99)
    };
    entries.sort_by_key(|p| (rank(p), p.clone()));
    for path in entries {
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let (title, description) = describe_example(&source);
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        out.push(json!({"name": name, "file": format!("{name}.forge"), "title": title, "description": description, "source": source}));
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct CompileReq {
    source: String,
}

async fn compile(Json(req): Json<CompileReq>) -> R<Value> {
    match cf_lang::compile(&req.source) {
        Ok(p) => Ok(Json(
            json!({"ok": true, "plan": p.plan.stats, "globals": p.globals, "disasm": p.disassemble()}),
        )),
        Err(e) => Ok(Json(json!({"ok": false, "error": e}))),
    }
}

#[derive(Deserialize)]
struct CreateReq {
    name: Option<String>,
    source: String,
    seed: Option<u64>,
    chaos: Option<f64>,
    #[serde(default)]
    inputs: Vec<String>,
}

async fn create_session(State(s): State<Arc<AppState>>, Json(req): Json<CreateReq>) -> R<Value> {
    let id = s.new_id("s");
    let opts = RecordOptions {
        name: req.name.unwrap_or_else(|| "scratch".into()),
        seed: req.seed.unwrap_or(0),
        chaos: req.chaos.unwrap_or(0.08).clamp(0.0, 1.0),
        inputs: req.inputs,
        max_steps: 1_000_000,
    };
    let src = req.source;
    let session = blocking(move || Session::record(id, &src, &opts)).await?;
    Ok(Json(s.insert(session)))
}

async fn import_session(State(s): State<Arc<AppState>>, body: Bytes) -> R<Value> {
    let rec = cf_trace::format::decode_recording(&body).map_err(|e| bad(e.to_string()))?;
    let id = s.new_id("s");
    let session =
        blocking(move || Session::from_source_and_recording(id, rec, Origin::Import)).await?;
    Ok(Json(s.insert(session)))
}

async fn list_sessions(State(s): State<Arc<AppState>>) -> Json<Vec<Value>> {
    let g = s.sessions.read().unwrap();
    let mut list: Vec<Value> = g
        .values()
        .map(|x| {
            json!({
                "id": x.id, "name": x.recording.meta.name, "seed": x.recording.meta.seed,
                "steps": x.recording.total_steps, "failed": x.recording.outcome.is_failure(),
                "created_unix_ms": x.recording.meta.created_unix_ms, "origin": x.origin,
            })
        })
        .collect();
    list.sort_by_key(|v| std::cmp::Reverse(v["created_unix_ms"].as_u64()));
    Json(list)
}

async fn session_summary(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> R<Value> {
    let session = s.get(&id)?;
    Ok(Json(serde_json::to_value(session.summary()).unwrap()))
}

async fn timeline(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> R<Value> {
    let session = s.get(&id)?;
    let v = blocking(move || serde_json::to_value(session.timeline_view()).unwrap()).await;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct StateQ {
    step: u64,
}

async fn state_at(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<StateQ>,
) -> R<Value> {
    let session = s.get(&id)?;
    let v = blocking(move || {
        session
            .state_after(q.step)
            .map(|v| serde_json::to_value(v).unwrap())
    })
    .await;
    v.map(Json)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})))
}

async fn analysis(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> R<Value> {
    let session = s.get(&id)?;
    Ok(Json(serde_json::to_value(&session.report).unwrap()))
}

#[derive(Deserialize)]
struct HistoryQ {
    loc: String,
}

async fn history(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<HistoryQ>,
) -> R<Value> {
    let session = s.get(&id)?;
    let loc: Loc = q.loc.parse().map_err(bad)?;
    Ok(Json(
        json!({"loc": loc, "name": session.names().loc(loc), "points": session.history(loc)}),
    ))
}

async fn diff(State(s): State<Arc<AppState>>, Path((a, b)): Path<(String, String)>) -> R<Value> {
    let (a, b) = (s.get(&a)?, s.get(&b)?);
    let v = blocking(move || {
        let d = cf_analysis::first_divergence(
            cf_analysis::Side {
                program: &a.program,
                recording: &a.recording,
                steps: &a.steps,
            },
            cf_analysis::Side {
                program: &b.program,
                recording: &b.recording,
                steps: &b.steps,
            },
        );
        serde_json::to_value(d).unwrap()
    })
    .await;
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
struct ExplainReq {
    #[serde(default)]
    llm: bool,
}

async fn explain(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<ExplainReq>>,
) -> R<Value> {
    let session = s.get(&id)?;
    let llm = body.map(|b| b.0.llm).unwrap_or(false);
    let v = blocking(move || serde_json::to_value(crate::explain::explain(&session, llm)).unwrap())
        .await;
    Ok(Json(v))
}

async fn export(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = s.get(&id)?;
    let bytes = cf_trace::format::encode_recording(&session.recording, None)
        .map_err(|e| bad(e.to_string()))?;
    let name = format!("{}-{}.cfrec", session.recording.meta.name, id);
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
struct HuntReq {
    name: Option<String>,
    source: String,
    runs: Option<u64>,
    chaos: Option<f64>,
}

async fn create_hunt(State(s): State<Arc<AppState>>, Json(req): Json<HuntReq>) -> R<Value> {
    let program = Arc::new(cf_lang::compile(&req.source).map_err(BuildError::Compile)?);
    let id = s.new_id("h");
    let runs = req.runs.unwrap_or(200).clamp(1, 5000);
    let chaos = req.chaos.unwrap_or(0.08).clamp(0.0, 1.0);
    let name = req.name.unwrap_or_else(|| "scratch".into());
    let h = blocking(move || hunt::run(id, name, program, runs, chaos))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e})))?;
    let view = serde_json::to_value(h.view()).unwrap();
    s.hunts.write().unwrap().insert(h.id.clone(), Arc::new(h));
    Ok(Json(view))
}

async fn get_hunt(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> R<Value> {
    let h = s
        .hunts
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| not_found("hunt"))?;
    Ok(Json(serde_json::to_value(h.view()).unwrap()))
}

async fn open_hunt_run(
    State(s): State<Arc<AppState>>,
    Path((id, seed)): Path<(String, u64)>,
) -> R<Value> {
    let h = s
        .hunts
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| not_found("hunt"))?;
    let idx = h
        .runs
        .iter()
        .position(|r| r.seed == seed)
        .ok_or_else(|| not_found("seed"))?;
    let rec = h.recordings[idx].clone();
    let program = h.program.clone();
    let sid = s.new_id("s");
    // replay the stored recording — never re-record, or now()-dependent runs would change
    let session = blocking(move || Session::replay(sid, program, rec, Origin::Hunt)).await?;
    Ok(Json(s.insert(session)))
}

fn native_path(s: &AppState, id: &str) -> Result<std::path::PathBuf, ApiError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(not_found("native recording"));
    }
    Ok(s.native_dir.join(format!("{id}.json")))
}

fn load_native(s: &AppState, id: &str) -> Result<cf_ptrace::NativeRecording, ApiError> {
    let raw = std::fs::read(native_path(s, id)?).map_err(|_| not_found("native recording"))?;
    serde_json::from_slice(&raw).map_err(|e| bad(format!("bad native recording: {e}")))
}

async fn native_list(State(s): State<Arc<AppState>>) -> Json<Vec<Value>> {
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&s.native_dir) {
        let mut paths: Vec<_> = dir.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        paths.sort();
        for p in paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
        {
            let id = p.file_stem().unwrap().to_string_lossy().to_string();
            let Ok(rec) = load_native(&s, &id) else {
                continue;
            };
            let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
            let mut emulated = 0;
            let mut signal = None;
            for e in &rec.events {
                match &e.kind {
                    cf_ptrace::NativeEventKind::Syscall { name, policy, .. } => {
                        *by_name.entry(name.clone()).or_default() += 1;
                        emulated += (*policy == cf_ptrace::SyscallPolicy::Emulated) as usize;
                    }
                    cf_ptrace::NativeEventKind::Signal { name, .. } => signal = Some(name.clone()),
                    _ => {}
                }
            }
            out.push(json!({
                "id": id, "program": rec.program, "arch": rec.arch, "kernel": rec.kernel,
                "events": rec.events.len(), "emulated": emulated, "signal": signal,
                "syscalls": by_name, "vdso_patched": rec.vdso_patched,
            }));
        }
    }
    Json(out)
}

async fn native_get(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> R<Value> {
    Ok(Json(serde_json::to_value(load_native(&s, &id)?).unwrap()))
}

#[derive(Deserialize)]
struct NativeReplayReq {
    stop_at: Option<u64>,
}

async fn native_replay(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<NativeReplayReq>,
) -> R<Value> {
    let rec = load_native(&s, &id)?;
    let v = blocking(move || cf_ptrace::replay(&rec, req.stop_at)).await;
    match v {
        Ok(report) => Ok(Json(serde_json::to_value(report).unwrap())),
        Err(e) => Err(ApiError(
            StatusCode::NOT_IMPLEMENTED,
            json!({"error": e.to_string()}),
        )),
    }
}
