use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cf_server::{app, AppState};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn state() -> Arc<AppState> {
    Arc::new(AppState::new(
        Arc::new(cf_trace::store::MemoryStore::default()),
        root().join("examples/forge"),
        root().join("examples/native"),
    ))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let req = req
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn bank() -> String {
    std::fs::read_to_string(root().join("examples/forge/bank.forge")).unwrap()
}

#[tokio::test]
async fn hunt_open_seek_and_diff() {
    let app = app(state(), None);

    let (st, ex) = call(&app, "GET", "/api/examples", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ex[0]["name"], "bank");

    let (st, hunt) = call(
        &app,
        "POST",
        "/api/hunt",
        Some(json!({"name": "bank", "source": bank(), "runs": 120, "chaos": 0.2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hunt}");
    let runs = hunt["runs"].as_array().unwrap();
    let fail = runs
        .iter()
        .find(|r| r["failed"] == true)
        .expect("hunt found a failure");
    let pass = runs
        .iter()
        .find(|r| r["failed"] == false)
        .expect("hunt found a pass");
    let hid = hunt["id"].as_str().unwrap();

    let open = |seed: &Value| format!("/api/hunt/{hid}/open/{seed}");
    let (_, bad) = call(&app, "POST", &open(&fail["seed"]), None).await;
    let (_, good) = call(&app, "POST", &open(&pass["seed"]), None).await;
    assert_eq!(bad["outcome"]["status"], "fault", "{bad}");
    assert_eq!(bad["origin"], "hunt");
    let (bid, gid) = (bad["id"].as_str().unwrap(), good["id"].as_str().unwrap());

    let (_, tl) = call(&app, "GET", &format!("/api/sessions/{bid}/timeline"), None).await;
    let n = tl["tid"].as_array().unwrap().len() as u64;
    assert_eq!(n, bad["total_steps"].as_u64().unwrap());

    // reverse debugging: state just before the failing step, then at step 0
    let fault_step = bad["outcome"]["step"].as_u64().unwrap();
    let (st, before) = call(
        &app,
        "GET",
        &format!("/api/sessions/{bid}/state?step={}", fault_step - 1),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(before["snapshot"]["globals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["name"] == "balance"));
    let (_, start) = call(
        &app,
        "GET",
        &format!("/api/sessions/{bid}/state?step=0"),
        None,
    )
    .await;
    assert_eq!(start["snapshot"]["position"], 1);

    let (_, analysis) = call(&app, "GET", &format!("/api/sessions/{bid}/analysis"), None).await;
    assert_eq!(analysis["hypotheses"][0]["kind"], "atomicity-violation");

    let (_, hist) = call(
        &app,
        "GET",
        &format!("/api/sessions/{bid}/history?loc=g:0"),
        None,
    )
    .await;
    assert!(hist["points"].as_array().unwrap().len() >= 2);
    assert_eq!(hist["name"], "balance");

    let (_, diff) = call(
        &app,
        "GET",
        &format!("/api/sessions/{gid}/diff/{bid}"),
        None,
    )
    .await;
    assert_eq!(diff["kind"], "schedule", "{diff}");

    let (_, ex) = call(
        &app,
        "POST",
        &format!("/api/sessions/{bid}/explain"),
        Some(json!({"llm": false})),
    )
    .await;
    assert_eq!(ex["mode"], "offline");
    assert!(ex["narrative"].as_str().unwrap().contains("Lost update"));
}

#[tokio::test]
async fn compile_errors_are_located() {
    let app = app(state(), None);
    let (st, v) = call(
        &app,
        "POST",
        "/api/sessions",
        Some(json!({"source": "let x = 1;\nprint(y);"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["line"], 2);
}

#[tokio::test]
async fn export_then_import_roundtrips() {
    let state = state();
    let app = app(state.clone(), None);
    let (_, s) = call(
        &app,
        "POST",
        "/api/sessions",
        Some(json!({"source": bank(), "seed": 3, "chaos": 0.2})),
    )
    .await;
    let id = s["id"].as_str().unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/sessions/{id}/export"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert!(
        bytes.len() < 4096,
        "recordings should be tiny, got {} bytes",
        bytes.len()
    );
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/sessions/import")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["total_steps"], s["total_steps"]);
    assert_eq!(body["outcome"]["status"], s["outcome"]["status"]);
}

#[tokio::test]
async fn grpc_record_seek_analyze_stream() {
    use cf_server::grpc::{pb, ChronoForgeClient, ChronoForgeServer, Service};

    let state = state();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let svc = ChronoForgeServer::new(Service { state });
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_incoming(
                tonic::transport::server::TcpIncoming::from_listener(listener, true, None).unwrap(),
            )
            .await
            .unwrap();
    });

    let mut client = ChronoForgeClient::connect(format!("http://{addr}"))
        .await
        .unwrap();
    let s = client
        .record(pb::RecordRequest {
            name: "bank".into(),
            source: bank(),
            seed: 1,
            chaos: 0.2,
            inputs: vec![],
        })
        .await
        .unwrap()
        .into_inner();
    assert!(s.total_steps > 10);

    let st = client
        .seek(pb::SeekRequest {
            session_id: s.id.clone(),
            step: 5,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(st.position, 6);

    let report = client
        .analyze(pb::SessionRef {
            session_id: s.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(report.races.iter().any(|r| r.name == "balance"));

    let mut stream = client
        .stream_steps(pb::StepRange {
            session_id: s.id.clone(),
            from: 0,
            to: 10,
        })
        .await
        .unwrap()
        .into_inner();
    let mut n = 0;
    while let Some(step) = stream.message().await.unwrap() {
        assert_eq!(step.step, n);
        n += 1;
    }
    assert_eq!(n, 10);

    let mut hunt = client
        .hunt(pb::HuntRequest {
            name: "bank".into(),
            source: bank(),
            runs: 40,
            chaos: 0.25,
        })
        .await
        .unwrap()
        .into_inner();
    let mut runs = 0;
    while hunt.message().await.unwrap().is_some() {
        runs += 1;
    }
    assert_eq!(runs, 40);
}
