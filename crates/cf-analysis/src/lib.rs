//! Offline analyses over a replayed trace.

pub mod diff;
pub mod hb;
pub mod locks;
pub mod names;
pub mod rootcause;
pub mod slice;

use cf_lang::Program;
use cf_trace::{Fault, Recording, StepEvent};
use serde::Serialize;

pub use diff::{first_divergence, Divergence, Side};
pub use hb::{detect_races, Race};
pub use locks::{lock_graph, LockGraph};
pub use names::Names;
pub use rootcause::{Evidence, Hypothesis};
pub use slice::{backward_slice, Slice};

pub const SLICE_LIMIT: usize = 400;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub steps: u64,
    pub threads: u32,
    pub shared_accesses: u64,
    pub sync_events: u64,
    pub nondet_events: u64,
    pub recording_events: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub fault: Option<Fault>,
    pub stats: Stats,
    pub races: Vec<Race>,
    pub locks: LockGraph,
    pub slice: Option<Slice>,
    pub hypotheses: Vec<Hypothesis>,
}

pub fn analyze(program: &Program, rec: &Recording, steps: &[StepEvent]) -> Report {
    let names = Names::new(program, steps);
    let races = detect_races(steps, &names);
    let locks = lock_graph(steps, &names);
    let fault = rec.outcome.fault().cloned();
    let slice = fault
        .as_ref()
        .filter(|f| (f.step as usize) < steps.len())
        .map(|f| backward_slice(steps, f.step, &names, SLICE_LIMIT));
    let hypotheses = rootcause::hypotheses(
        steps,
        fault.as_ref(),
        slice.as_ref(),
        &races,
        &locks,
        &names,
    );

    let stats = Stats {
        steps: steps.len() as u64,
        threads: steps.iter().map(|s| s.tid + 1).max().unwrap_or(1),
        shared_accesses: steps
            .iter()
            .map(|s| {
                s.reads.iter().filter(|r| r.loc.is_shared()).count()
                    + s.writes.iter().filter(|w| w.loc.is_shared()).count()
            })
            .sum::<usize>() as u64,
        sync_events: steps.iter().filter(|s| s.sync.is_some()).count() as u64,
        nondet_events: steps.iter().filter(|s| s.nondet.is_some()).count() as u64,
        recording_events: rec.nondet.len() as u64,
    };

    Report {
        fault,
        stats,
        races,
        locks,
        slice,
        hypotheses,
    }
}
