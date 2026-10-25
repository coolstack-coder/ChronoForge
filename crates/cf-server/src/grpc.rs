use std::pin::Pin;
use std::sync::Arc;

use tokio_stream_lite::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::session::Session;
use crate::AppState;

pub mod pb {
    tonic::include_proto!("chronoforge.v1");
}

use pb::chrono_forge_server::ChronoForge;

pub use pb::chrono_forge_client::ChronoForgeClient;
pub use pb::chrono_forge_server::ChronoForgeServer;

pub struct Service {
    pub state: Arc<AppState>,
}

fn summary(s: &Session) -> pb::SessionSummary {
    let fault = s.recording.outcome.fault();
    pb::SessionSummary {
        id: s.id.clone(),
        name: s.recording.meta.name.clone(),
        total_steps: s.recording.total_steps,
        threads: s.steps.iter().map(|x| x.tid + 1).max().unwrap_or(1),
        outcome: match &s.recording.outcome {
            cf_trace::Outcome::Completed => "completed",
            cf_trace::Outcome::Fault(_) => "fault",
            cf_trace::Outcome::StepLimit { .. } => "step_limit",
        }
        .into(),
        fault_kind: fault
            .map(|f| f.kind.label().to_string())
            .unwrap_or_default(),
        fault_message: fault.map(|f| f.message.clone()).unwrap_or_default(),
        fault_line: fault.map(|f| f.line).unwrap_or(0),
        fault_step: fault.map(|f| f.step).unwrap_or(0),
        recording_events: s.recording.nondet.len() as u64,
        recording_bytes: s.recording_bytes as u64,
    }
}

fn var(v: &cf_vm::snapshot::VarSnap) -> pb::Variable {
    pb::Variable {
        name: v.name.clone(),
        value: v.value.clone(),
        kind: v.kind.into(),
        loc: v.loc.to_string(),
        writer: v.writer,
    }
}

fn session(state: &AppState, id: &str) -> Result<Arc<Session>, Status> {
    state
        .sessions
        .read()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| Status::not_found(format!("no session {id}")))
}

type Stream<T> = Pin<Box<dyn tokio_stream_lite::Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl ChronoForge for Service {
    async fn record(
        &self,
        req: Request<pb::RecordRequest>,
    ) -> Result<Response<pb::SessionSummary>, Status> {
        let r = req.into_inner();
        let id = self.state.new_id("s");
        let opts = cf_vm::RecordOptions {
            name: if r.name.is_empty() {
                "grpc".into()
            } else {
                r.name
            },
            seed: r.seed,
            chaos: if r.chaos > 0.0 {
                r.chaos.min(1.0)
            } else {
                0.08
            },
            inputs: r.inputs,
            max_steps: 1_000_000,
        };
        let source = r.source;
        let s = tokio::task::spawn_blocking(move || Session::record(id, &source, &opts))
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .map_err(|e| match e {
                crate::session::BuildError::Compile(c) => Status::invalid_argument(c.to_string()),
                crate::session::BuildError::Engine(m) => Status::internal(m),
            })?;
        let out = summary(&s);
        self.state.insert(s);
        Ok(Response::new(out))
    }

    type HuntStream = Stream<pb::HuntRun>;

    async fn hunt(
        &self,
        req: Request<pb::HuntRequest>,
    ) -> Result<Response<Self::HuntStream>, Status> {
        let r = req.into_inner();
        let program = Arc::new(
            cf_lang::compile(&r.source).map_err(|e| Status::invalid_argument(e.to_string()))?,
        );
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let runs = r.runs.clamp(1, 5000) as u64;
        let chaos = if r.chaos > 0.0 {
            r.chaos.min(1.0)
        } else {
            0.08
        };
        tokio::task::spawn_blocking(move || {
            for seed in 0..runs {
                let opts = cf_vm::RecordOptions {
                    seed,
                    chaos,
                    max_steps: 500_000,
                    ..Default::default()
                };
                let msg = cf_vm::record(program.clone(), &opts)
                    .map(|rec| pb::HuntRun {
                        seed,
                        failed: rec.outcome.is_failure(),
                        signature: rec
                            .outcome
                            .fault()
                            .map(|f| format!("{} @ line {}", f.kind.label(), f.line))
                            .unwrap_or_else(|| "pass".into()),
                        steps: rec.total_steps,
                    })
                    .map_err(|e| Status::internal(e.to_string()));
                if tx.blocking_send(msg).is_err() {
                    break; // client went away
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn seek(
        &self,
        req: Request<pb::SeekRequest>,
    ) -> Result<Response<pb::MachineState>, Status> {
        let r = req.into_inner();
        let s = session(&self.state, &r.session_id)?;
        let view = tokio::task::spawn_blocking(move || s.state_after(r.step))
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .map_err(Status::internal)?;
        let snap = view.snapshot;
        Ok(Response::new(pb::MachineState {
            position: snap.position,
            current: snap.current,
            threads: snap
                .threads
                .iter()
                .map(|t| pb::Thread {
                    tid: t.tid,
                    name: t.name.clone(),
                    state: t.state.clone(),
                    frames: t
                        .frames
                        .iter()
                        .map(|f| pb::Frame {
                            func: f.func.clone(),
                            line: f.line,
                            locals: f.locals.iter().map(var).collect(),
                        })
                        .collect(),
                })
                .collect(),
            globals: snap.globals.iter().map(var).collect(),
            output: snap.output.iter().map(|o| o.text.clone()).collect(),
        }))
    }

    type StreamStepsStream = Stream<pb::Step>;

    async fn stream_steps(
        &self,
        req: Request<pb::StepRange>,
    ) -> Result<Response<Self::StreamStepsStream>, Status> {
        let r = req.into_inner();
        let s = session(&self.state, &r.session_id)?;
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        tokio::task::spawn_blocking(move || {
            let names = s.names();
            let to = if r.to == 0 {
                s.steps.len() as u64
            } else {
                r.to.min(s.steps.len() as u64)
            };
            for e in &s.steps[(r.from.min(to) as usize)..(to as usize)] {
                let acc = |loc: cf_trace::Loc, v: &cf_trace::Val| pb::Access {
                    loc: loc.to_string(),
                    name: names.loc(loc),
                    value: v.to_string(),
                };
                let msg = pb::Step {
                    step: e.step,
                    tid: e.tid,
                    line: e.line,
                    op: format!("{:?}", e.op),
                    what: cf_analysis::slice::describe(e, &names),
                    reads: e.reads.iter().map(|x| acc(x.loc, &x.val)).collect(),
                    writes: e.writes.iter().map(|x| acc(x.loc, &x.new)).collect(),
                    deps: e.deps.clone(),
                };
                if tx.blocking_send(Ok(msg)).is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn analyze(
        &self,
        req: Request<pb::SessionRef>,
    ) -> Result<Response<pb::AnalysisReport>, Status> {
        let s = session(&self.state, &req.into_inner().session_id)?;
        let r = &s.report;
        Ok(Response::new(pb::AnalysisReport {
            hypotheses: r
                .hypotheses
                .iter()
                .map(|h| pb::Hypothesis {
                    kind: h.kind.clone(),
                    title: h.title.clone(),
                    explanation: h.explanation.clone(),
                    confidence: h.confidence,
                    evidence_steps: h.evidence.iter().map(|e| e.step).collect(),
                    fix: h.fix.clone().unwrap_or_default(),
                })
                .collect(),
            races: r
                .races
                .iter()
                .map(|x| pb::Race {
                    name: x.name.clone(),
                    kind: x.kind.clone(),
                    first_step: x.first.step,
                    second_step: x.second.step,
                    first_line: x.first.line,
                    second_line: x.second.line,
                })
                .collect(),
            lock_cycles: r
                .locks
                .cycles
                .iter()
                .map(|c| c.description.clone())
                .collect(),
        }))
    }
}

/// Just enough of tokio-stream to turn an mpsc receiver into a Stream,
/// without pulling in another crate for twenty lines.
mod tokio_stream_lite {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub use tonic::codegen::tokio_stream::Stream;

    pub struct ReceiverStream<T>(tokio::sync::mpsc::Receiver<T>);

    impl<T> ReceiverStream<T> {
        pub fn new(rx: tokio::sync::mpsc::Receiver<T>) -> Self {
            ReceiverStream(rx)
        }
    }

    impl<T> Stream for ReceiverStream<T> {
        type Item = T;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
            self.0.poll_recv(cx)
        }
    }
}
