//! Deterministic record & replay for Forge programs.
//!
//! The usual flow is:
//!
//! ```text
//! record()  --> Recording { nondet log, outcome }       (cheap, KBs)
//!                  |
//! Timeline::build(program, recording) --> full instrumented trace
//!                  |                      + checkpoints for seeking
//! timeline.seek(n) --> Machine at step n                 (reverse debugging)
//! ```

pub mod machine;
pub mod oracle;
pub mod snapshot;
pub mod timeline;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use cf_lang::Program;
use cf_trace::{Recording, Step, StepEvent, TraceMeta};

pub use machine::{Machine, Observer, Oracle, Quiet, ThreadState};
pub use oracle::{RecordOracle, ReplayOracle, SplitMix64};
pub use snapshot::Snapshot;
pub use timeline::{Timeline, DEFAULT_INTERVAL};

pub const ENGINE: &str = concat!("chronoforge-vm/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("replay diverged at step {step}: {detail}")]
    Divergence { step: Step, detail: String },
}

#[derive(Clone, Debug)]
pub struct RecordOptions {
    pub name: String,
    pub seed: u64,
    /// Chance of a preemption at each instruction.
    pub chaos: f64,
    pub inputs: Vec<String>,
    pub max_steps: Step,
}

impl Default for RecordOptions {
    fn default() -> Self {
        RecordOptions {
            name: "program".into(),
            seed: 0,
            chaos: 0.08,
            inputs: Vec::new(),
            max_steps: 2_000_000,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn run_recorder(
    program: Arc<Program>,
    opts: &RecordOptions,
    obs: &mut dyn Observer,
) -> Result<Recording, VmError> {
    let mut oracle = RecordOracle::new(opts.seed, opts.chaos, opts.inputs.clone());
    let mut m = Machine::new(program.clone());
    let outcome = m.run(&mut oracle, obs, opts.max_steps)?;
    Ok(Recording {
        meta: TraceMeta {
            name: opts.name.clone(),
            source: program.source.clone(),
            seed: opts.seed,
            chaos: opts.chaos,
            created_unix_ms: now_ms(),
            engine: ENGINE.into(),
        },
        nondet: oracle.log,
        outcome,
        total_steps: m.step,
        output: m.output,
    })
}

/// Record a run, capturing only nondeterminism. This is the fast path —
/// no step events are materialised.
pub fn record(program: Arc<Program>, opts: &RecordOptions) -> Result<Recording, VmError> {
    run_recorder(program, opts, &mut Quiet)
}

/// Record and keep the fully instrumented trace in one go (tests, small demos).
pub fn record_traced(
    program: Arc<Program>,
    opts: &RecordOptions,
) -> Result<(Recording, Vec<StepEvent>), VmError> {
    let mut steps = Vec::new();
    let rec = run_recorder(program, opts, &mut steps)?;
    Ok((rec, steps))
}

/// Replay a recording from scratch, streaming instrumented events.
pub fn replay(
    program: Arc<Program>,
    rec: &Recording,
    obs: &mut dyn Observer,
) -> Result<Machine, VmError> {
    let mut oracle = ReplayOracle::new(rec.nondet.clone().into());
    let mut m = Machine::new(program);
    while m.outcome.is_none() && m.step < rec.total_steps {
        m.step_once(&mut oracle, obs)?;
    }
    // a fault at the very last step still needs one call to settle deadlocks
    if m.outcome.is_none() && !matches!(rec.outcome, cf_trace::Outcome::StepLimit { .. }) {
        m.step_once(&mut oracle, obs)?;
    }
    verify(&m, rec)?;
    Ok(m)
}

/// Replay must land exactly where the recording did.
pub(crate) fn verify(m: &Machine, rec: &Recording) -> Result<(), VmError> {
    let outcome_ok = match (&m.outcome, &rec.outcome) {
        (Some(a), b) => a == b,
        (None, cf_trace::Outcome::StepLimit { .. }) => true,
        (None, _) => false,
    };
    if !outcome_ok || m.step != rec.total_steps || m.output != rec.output {
        return Err(VmError::Divergence {
            step: m.step,
            detail: format!(
                "replay ended with {:?} after {} steps, recording ended with {:?} after {}",
                m.outcome, m.step, rec.outcome, rec.total_steps
            ),
        });
    }
    Ok(())
}
