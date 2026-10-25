//! Reverse execution, the way rr and WinDbg TTD do it: you can't run a CPU
//! backwards, so keep periodic checkpoints and *re-execute forward* from the
//! nearest one. With deterministic replay that's indistinguishable from
//! going back in time, and costs at most `interval` steps per seek.

use std::sync::Arc;

use cf_lang::Program;
use cf_trace::{Recording, Step};

use crate::machine::{Machine, Observer, Quiet};
use crate::oracle::ReplayOracle;
use crate::VmError;

pub const DEFAULT_INTERVAL: Step = 512;

struct Checkpoint {
    machine: Machine,
    cursor: usize,
}

pub struct Timeline {
    pub program: Arc<Program>,
    pub recording: Arc<Recording>,
    log: Arc<[cf_trace::NondetEvent]>,
    checkpoints: Vec<Checkpoint>,
    interval: Step,
}

impl Timeline {
    /// Replays the whole recording once, dropping a checkpoint every
    /// `interval` steps and streaming instrumented events into `obs`.
    pub fn build(
        program: Arc<Program>,
        recording: Arc<Recording>,
        interval: Step,
        obs: &mut dyn Observer,
    ) -> Result<Timeline, VmError> {
        let interval = interval.max(1);
        let log: Arc<[cf_trace::NondetEvent]> = recording.nondet.clone().into();
        let mut oracle = ReplayOracle::new(log.clone());
        let mut m = Machine::new(program.clone());
        let mut checkpoints = Vec::new();

        let step_limited = matches!(recording.outcome, cf_trace::Outcome::StepLimit { .. });
        while m.outcome.is_none() && m.step <= recording.total_steps {
            if m.step.is_multiple_of(interval) && checkpoints.len() as Step == m.step / interval {
                checkpoints.push(Checkpoint {
                    machine: m.clone(),
                    cursor: oracle.cursor,
                });
            }
            if m.step == recording.total_steps && step_limited {
                break;
            }
            // at the final position this call just settles Completed/Deadlock
            m.step_once(&mut oracle, obs)?;
        }
        crate::verify(&m, &recording)?;
        Ok(Timeline {
            program,
            recording,
            log,
            checkpoints,
            interval,
        })
    }

    pub fn total_steps(&self) -> Step {
        self.recording.total_steps
    }

    pub fn checkpoints(&self) -> usize {
        self.checkpoints.len()
    }

    /// Machine state after exactly `position` steps (clamped to the end).
    pub fn seek(&self, position: Step) -> Result<Machine, VmError> {
        let position = position.min(self.recording.total_steps);
        let idx = ((position / self.interval) as usize).min(self.checkpoints.len() - 1);
        let cp = &self.checkpoints[idx];
        let mut m = cp.machine.clone();
        let mut oracle = ReplayOracle::at(self.log.clone(), cp.cursor);
        while m.step < position && m.outcome.is_none() {
            m.step_once(&mut oracle, &mut Quiet)?;
        }
        Ok(m)
    }
}
