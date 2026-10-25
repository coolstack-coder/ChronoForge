//! A session = one recording, replayed once into a fully instrumented trace,
//! plus checkpoints for seeking and a cached analysis report.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use cf_analysis::{Names, Report};
use cf_lang::{PlanStats, Program};
use cf_trace::{Loc, NondetEvent, Outcome, Recording, Step, StepEvent, SyncOp, Tid};
use cf_vm::{RecordOptions, Snapshot, Timeline};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Record,
    Hunt,
    Import,
}

pub struct Session {
    pub id: String,
    pub origin: Origin,
    pub program: Arc<Program>,
    pub recording: Arc<Recording>,
    pub steps: Arc<Vec<StepEvent>>,
    pub timeline: Timeline,
    pub report: Report,
    pub timings: Timings,
    pub recording_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Timings {
    pub record_ms: f64,
    pub replay_ms: f64,
    pub analysis_ms: f64,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

#[derive(Debug)]
pub enum BuildError {
    Compile(cf_lang::CompileError),
    Engine(String),
}

impl Session {
    pub fn record(id: String, source: &str, opts: &RecordOptions) -> Result<Session, BuildError> {
        let program = Arc::new(cf_lang::compile(source).map_err(BuildError::Compile)?);
        let t = Instant::now();
        let rec =
            cf_vm::record(program.clone(), opts).map_err(|e| BuildError::Engine(e.to_string()))?;
        let record_ms = ms(t);
        let mut s = Session::replay(id, program, rec, Origin::Record)?;
        s.timings.record_ms = record_ms;
        Ok(s)
    }

    /// Build a session from an existing recording. This is the interesting
    /// path: nothing is re-recorded, the full trace comes from replay.
    pub fn replay(
        id: String,
        program: Arc<Program>,
        rec: Recording,
        origin: Origin,
    ) -> Result<Session, BuildError> {
        let recording = Arc::new(rec);
        let t = Instant::now();
        let mut steps = Vec::with_capacity(recording.total_steps as usize);
        let timeline = Timeline::build(
            program.clone(),
            recording.clone(),
            cf_vm::DEFAULT_INTERVAL,
            &mut steps,
        )
        .map_err(|e| BuildError::Engine(e.to_string()))?;
        let replay_ms = ms(t);

        let t = Instant::now();
        let report = cf_analysis::analyze(&program, &recording, &steps);
        let analysis_ms = ms(t);

        let recording_bytes = cf_trace::format::encode_recording(&recording, None)
            .map(|b| b.len())
            .unwrap_or(0);
        Ok(Session {
            id,
            origin,
            program,
            recording,
            steps: Arc::new(steps),
            timeline,
            report,
            timings: Timings {
                record_ms: 0.0,
                replay_ms,
                analysis_ms,
            },
            recording_bytes,
        })
    }

    pub fn from_source_and_recording(
        id: String,
        rec: Recording,
        origin: Origin,
    ) -> Result<Session, BuildError> {
        let program = Arc::new(cf_lang::compile(&rec.meta.source).map_err(BuildError::Compile)?);
        Session::replay(id, program, rec, origin)
    }

    pub fn names(&self) -> Names<'_> {
        Names::new(&self.program, &self.steps)
    }

    pub fn summary(&self) -> Summary {
        let mut threads: BTreeMap<Tid, ThreadInfo> = BTreeMap::new();
        let mut heat: BTreeMap<u32, u64> = BTreeMap::new();
        for s in self.steps.iter() {
            let t = threads.entry(s.tid).or_insert(ThreadInfo {
                tid: s.tid,
                name: String::new(),
                first_step: s.step,
                last_step: s.step,
                steps: 0,
            });
            t.last_step = s.step;
            t.steps += 1;
            *heat.entry(s.line).or_default() += 1;
        }
        // thread names come from the final machine state
        if let Ok(m) = self.timeline.seek(self.timeline.total_steps()) {
            for th in &m.threads {
                if let Some(t) = threads.get_mut(&th.tid) {
                    t.name = th.name.clone();
                }
            }
        }
        let nd = &self.recording.nondet;
        let count = |f: fn(&NondetEvent) -> bool| nd.iter().filter(|e| f(e)).count();
        Summary {
            id: self.id.clone(),
            name: self.recording.meta.name.clone(),
            origin: self.origin,
            source: self.program.source.clone(),
            seed: self.recording.meta.seed,
            chaos: self.recording.meta.chaos,
            total_steps: self.recording.total_steps,
            threads: threads.into_values().collect(),
            outcome: OutcomeView::from(&self.recording.outcome),
            recording: RecordingView {
                events: nd.len(),
                bytes: self.recording_bytes,
                schedule: count(|e| matches!(e, NondetEvent::Schedule { .. })),
                rand: count(|e| matches!(e, NondetEvent::Rand { .. })),
                clock: count(|e| matches!(e, NondetEvent::Clock { .. })),
                input: count(|e| matches!(e, NondetEvent::Input { .. })),
            },
            timings: self.timings,
            checkpoints: self.timeline.checkpoints(),
            plan: self.program.plan.stats.clone(),
            line_heat: heat.into_iter().collect(),
            output: self
                .recording
                .output
                .iter()
                .map(|o| (o.step, o.tid, o.text.clone()))
                .collect(),
            created_unix_ms: self.recording.meta.created_unix_ms,
            engine: self.recording.meta.engine.clone(),
        }
    }

    /// Columnar, compact trace for the timeline canvas.
    pub fn timeline_view(&self) -> TimelineView {
        let report = &self.report;
        let race_steps: std::collections::BTreeSet<Step> = report
            .races
            .iter()
            .flat_map(|r| [r.first.step, r.second.step])
            .collect();
        let slice_steps: std::collections::BTreeSet<Step> = report
            .slice
            .iter()
            .flat_map(|s| s.nodes.iter().map(|n| n.step))
            .collect();
        let switches: std::collections::BTreeSet<Step> = self
            .recording
            .nondet
            .iter()
            .filter_map(|e| match e {
                NondetEvent::Schedule { step, .. } => Some(*step),
                _ => None,
            })
            .collect();
        let fault_step = self.recording.outcome.fault().map(|f| f.step);
        let print_steps: std::collections::BTreeSet<Step> =
            self.recording.output.iter().map(|o| o.step).collect();

        let n = self.steps.len();
        let mut v = TimelineView {
            tid: Vec::with_capacity(n),
            line: Vec::with_capacity(n),
            op: Vec::with_capacity(n),
            flags: Vec::with_capacity(n),
            sync: Vec::new(),
            ops: OPS.iter().map(|s| s.to_string()).collect(),
        };
        for s in self.steps.iter() {
            let mut f = 0u32;
            if s.reads.iter().any(|r| r.loc.is_shared()) {
                f |= flag::SHARED_READ;
            }
            if s.writes.iter().any(|w| w.loc.is_shared()) {
                f |= flag::SHARED_WRITE;
            }
            if s.sync.is_some() {
                f |= flag::SYNC;
            }
            if s.nondet.is_some() {
                f |= flag::NONDET;
            }
            if switches.contains(&s.step) {
                f |= flag::SWITCH;
            }
            if slice_steps.contains(&s.step) {
                f |= flag::SLICE;
            }
            if race_steps.contains(&s.step) {
                f |= flag::RACE;
            }
            if fault_step == Some(s.step) {
                f |= flag::FAULT;
            }
            if print_steps.contains(&s.step) {
                f |= flag::OUTPUT;
            }
            v.tid.push(s.tid);
            v.line.push(s.line);
            v.op.push(
                OPS.iter()
                    .position(|o| *o == format!("{:?}", s.op))
                    .unwrap_or(0) as u8,
            );
            v.flags.push(f);
            if let Some(op) = s.sync {
                let (kind, target) = match op {
                    SyncOp::Spawn { child } => ("spawn", Some(child)),
                    SyncOp::Join { target } => ("join", Some(target)),
                    SyncOp::Acquire { mutex } => ("acquire", Some(mutex)),
                    SyncOp::Release { mutex } => ("release", Some(mutex)),
                    SyncOp::BlockLock { mutex, .. } => ("block", Some(mutex)),
                    SyncOp::BlockJoin { target } => ("block_join", Some(target)),
                    SyncOp::ThreadExit => ("exit", None),
                };
                v.sync.push(SyncMark {
                    step: s.step,
                    tid: s.tid,
                    kind,
                    target,
                });
            }
        }
        v
    }

    /// State right *after* `step` executed (so the UI's "current step" is the
    /// instruction you're looking at, with its effects visible).
    pub fn state_after(&self, step: Step) -> Result<StateView, String> {
        let position = (step + 1).min(self.timeline.total_steps());
        let t = Instant::now();
        let m = self.timeline.seek(position).map_err(|e| e.to_string())?;
        let seek_ms = ms(t);
        let names = self.names();
        let event = self
            .steps
            .get(step as usize)
            .map(|e| StepView::new(e, &names, &self.program));
        Ok(StateView {
            snapshot: m.snapshot(),
            event,
            seek_ms,
        })
    }

    pub fn history(&self, loc: Loc) -> Vec<HistoryPoint> {
        self.steps
            .iter()
            .flat_map(|s| {
                s.writes
                    .iter()
                    .filter(|w| w.loc == loc)
                    .map(move |w| HistoryPoint {
                        step: s.step,
                        tid: s.tid,
                        line: s.line,
                        old: w.old.to_string(),
                        new: w.new.to_string(),
                        int: match w.new {
                            cf_trace::Val::Int(i) => Some(i),
                            _ => None,
                        },
                    })
            })
            .collect()
    }
}

pub mod flag {
    pub const SHARED_READ: u32 = 1;
    pub const SHARED_WRITE: u32 = 2;
    pub const SYNC: u32 = 4;
    pub const NONDET: u32 = 8;
    pub const SWITCH: u32 = 16;
    pub const SLICE: u32 = 32;
    pub const RACE: u32 = 64;
    pub const FAULT: u32 = 128;
    pub const OUTPUT: u32 = 256;
}

const OPS: &[&str] = &[
    "Const",
    "Load",
    "Store",
    "Arith",
    "Compare",
    "Jump",
    "Branch",
    "Call",
    "Return",
    "Array",
    "Index",
    "StoreIndex",
    "Builtin",
    "Spawn",
    "Join",
    "Lock",
    "Unlock",
    "Yield",
    "Assert",
    "Print",
    "Nondet",
    "Halt",
    "Pop",
];

#[derive(Serialize)]
pub struct ThreadInfo {
    pub tid: Tid,
    pub name: String,
    pub first_step: Step,
    pub last_step: Step,
    pub steps: u64,
}

#[derive(Serialize)]
pub struct OutcomeView {
    pub status: &'static str,
    pub kind: Option<String>,
    pub message: Option<String>,
    pub step: Option<Step>,
    pub tid: Option<Tid>,
    pub line: Option<u32>,
}

impl From<&Outcome> for OutcomeView {
    fn from(o: &Outcome) -> Self {
        match o {
            Outcome::Completed => OutcomeView {
                status: "completed",
                kind: None,
                message: None,
                step: None,
                tid: None,
                line: None,
            },
            Outcome::StepLimit { limit } => OutcomeView {
                status: "step_limit",
                kind: None,
                message: Some(format!("stopped after {limit} steps")),
                step: None,
                tid: None,
                line: None,
            },
            Outcome::Fault(f) => OutcomeView {
                status: "fault",
                kind: Some(f.kind.label().to_string()),
                message: Some(f.message.clone()),
                step: Some(f.step),
                tid: Some(f.tid),
                line: Some(f.line),
            },
        }
    }
}

#[derive(Serialize)]
pub struct RecordingView {
    pub events: usize,
    pub bytes: usize,
    pub schedule: usize,
    pub rand: usize,
    pub clock: usize,
    pub input: usize,
}

#[derive(Serialize)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub origin: Origin,
    pub source: String,
    pub seed: u64,
    pub chaos: f64,
    pub total_steps: Step,
    pub threads: Vec<ThreadInfo>,
    pub outcome: OutcomeView,
    pub recording: RecordingView,
    pub timings: Timings,
    pub checkpoints: usize,
    pub plan: PlanStats,
    pub line_heat: Vec<(u32, u64)>,
    pub output: Vec<(Step, Tid, String)>,
    pub created_unix_ms: u64,
    pub engine: String,
}

#[derive(Serialize)]
pub struct SyncMark {
    pub step: Step,
    pub tid: Tid,
    pub kind: &'static str,
    pub target: Option<u32>,
}

#[derive(Serialize)]
pub struct TimelineView {
    pub tid: Vec<Tid>,
    pub line: Vec<u32>,
    pub op: Vec<u8>,
    pub flags: Vec<u32>,
    pub sync: Vec<SyncMark>,
    pub ops: Vec<String>,
}

#[derive(Serialize)]
pub struct AccessView {
    pub loc: Loc,
    pub name: String,
    pub value: String,
    pub old: Option<String>,
    pub writer: Option<Step>,
    pub shared: bool,
}

#[derive(Serialize)]
pub struct StepView {
    pub step: Step,
    pub tid: Tid,
    pub line: u32,
    pub func: String,
    pub op: String,
    pub what: String,
    pub code: String,
    pub reads: Vec<AccessView>,
    pub writes: Vec<AccessView>,
    pub deps: Vec<Step>,
    pub ctrl: Option<Step>,
    pub sync: Option<String>,
    pub nondet: Option<String>,
}

impl StepView {
    pub fn new(e: &StepEvent, names: &Names, program: &Program) -> StepView {
        StepView {
            step: e.step,
            tid: e.tid,
            line: e.line,
            func: program.func(e.func).name.clone(),
            op: format!("{:?}", e.op),
            what: cf_analysis::slice::describe(e, names),
            code: names.source_line(e.line),
            reads: e
                .reads
                .iter()
                .map(|r| AccessView {
                    loc: r.loc,
                    name: names.loc(r.loc),
                    value: r.val.to_string(),
                    old: None,
                    writer: r.writer,
                    shared: r.loc.is_shared(),
                })
                .collect(),
            writes: e
                .writes
                .iter()
                .map(|w| AccessView {
                    loc: w.loc,
                    name: names.loc(w.loc),
                    value: w.new.to_string(),
                    old: Some(w.old.to_string()),
                    writer: Some(e.step),
                    shared: w.loc.is_shared(),
                })
                .collect(),
            deps: e.deps.clone(),
            ctrl: e.ctrl,
            sync: e.sync.map(|s| match s {
                SyncOp::Spawn { child } => format!("spawn T{child}"),
                SyncOp::Join { target } => format!("join T{target}"),
                SyncOp::Acquire { mutex } => format!("acquire {}", names.mutex(mutex)),
                SyncOp::Release { mutex } => format!("release {}", names.mutex(mutex)),
                SyncOp::BlockLock { mutex, owner } => {
                    format!("blocked on {} (held by T{owner})", names.mutex(mutex))
                }
                SyncOp::BlockJoin { target } => format!("waiting for T{target}"),
                SyncOp::ThreadExit => "thread exit".into(),
            }),
            nondet: e.nondet.map(|n| format!("{n:?}").to_lowercase()),
        }
    }
}

#[derive(Serialize)]
pub struct StateView {
    pub snapshot: Snapshot,
    pub event: Option<StepView>,
    pub seek_ms: f64,
}

#[derive(Serialize)]
pub struct HistoryPoint {
    pub step: Step,
    pub tid: Tid,
    pub line: u32,
    pub old: String,
    pub new: String,
    pub int: Option<i64>,
}
