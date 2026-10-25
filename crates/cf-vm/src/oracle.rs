use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use cf_trace::{NondetEvent, Step, Tid};

use crate::machine::{Machine, Oracle};
use crate::VmError;

/// SplitMix64. Tiny, fast, and identical on every platform, which is all a
/// chaos scheduler needs.
#[derive(Clone, Debug)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        SplitMix64(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
}

/// Records a run. Scheduling is "chaos mode" (like rr's): at every
/// instruction there's a `chaos` chance of preempting the running thread,
/// which shakes out interleavings a normal scheduler would hide.
pub struct RecordOracle {
    rng: SplitMix64,
    chaos: f64,
    inputs: VecDeque<String>,
    pub log: Vec<NondetEvent>,
}

impl RecordOracle {
    pub fn new(seed: u64, chaos: f64, inputs: Vec<String>) -> Self {
        RecordOracle {
            rng: SplitMix64::new(seed),
            chaos,
            inputs: inputs.into(),
            log: Vec::new(),
        }
    }
}

impl Oracle for RecordOracle {
    fn schedule(&mut self, m: &Machine, runnable: &[Tid]) -> Result<Tid, VmError> {
        let cur = m.current;
        let cur_ok = runnable.contains(&cur);
        let others: Vec<Tid> = runnable.iter().copied().filter(|t| *t != cur).collect();
        let next = if !cur_ok {
            others[self.rng.below(others.len() as u64) as usize]
        } else if !others.is_empty() && (m.yielded || self.rng.chance(self.chaos)) {
            others[self.rng.below(others.len() as u64) as usize]
        } else {
            cur
        };
        if next != cur {
            self.log.push(NondetEvent::Schedule {
                step: m.step,
                tid: next,
            });
        }
        Ok(next)
    }

    fn rand(&mut self, step: Step, bound: i64) -> Result<i64, VmError> {
        let value = self.rng.below(bound as u64) as i64;
        self.log.push(NondetEvent::Rand { step, value });
        Ok(value)
    }

    fn clock(&mut self, step: Step) -> Result<i64, VmError> {
        // genuinely nondeterministic: the wall clock
        let value = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.log.push(NondetEvent::Clock { step, value });
        Ok(value)
    }

    fn input(&mut self, step: Step) -> Result<String, VmError> {
        let value = self.inputs.pop_front().unwrap_or_default();
        self.log.push(NondetEvent::Input {
            step,
            value: value.clone(),
        });
        Ok(value)
    }
}

/// Feeds a recorded log back in, and refuses to improvise: any mismatch
/// between what the program asks for and what the log holds is a divergence.
#[derive(Clone)]
pub struct ReplayOracle {
    log: Arc<[NondetEvent]>,
    pub cursor: usize,
}

impl ReplayOracle {
    pub fn new(log: Arc<[NondetEvent]>) -> Self {
        ReplayOracle { log, cursor: 0 }
    }

    pub fn at(log: Arc<[NondetEvent]>, cursor: usize) -> Self {
        ReplayOracle { log, cursor }
    }

    fn take(&mut self, step: Step, what: &str) -> Result<&NondetEvent, VmError> {
        match self.log.get(self.cursor) {
            Some(ev) if ev.step() == step => {
                self.cursor += 1;
                Ok(&self.log[self.cursor - 1])
            }
            other => Err(VmError::Divergence {
                step,
                detail: format!("program asked for {what}, recording has {other:?}"),
            }),
        }
    }
}

impl Oracle for ReplayOracle {
    fn schedule(&mut self, m: &Machine, _runnable: &[Tid]) -> Result<Tid, VmError> {
        if let Some(NondetEvent::Schedule { step, tid }) = self.log.get(self.cursor) {
            if *step == m.step {
                self.cursor += 1;
                return Ok(*tid);
            }
        }
        Ok(m.current)
    }

    fn rand(&mut self, step: Step, _bound: i64) -> Result<i64, VmError> {
        match self.take(step, "rand()")? {
            NondetEvent::Rand { value, .. } => Ok(*value),
            other => Err(VmError::Divergence {
                step,
                detail: format!("expected rand(), recording has {other:?}"),
            }),
        }
    }

    fn clock(&mut self, step: Step) -> Result<i64, VmError> {
        match self.take(step, "now()")? {
            NondetEvent::Clock { value, .. } => Ok(*value),
            other => Err(VmError::Divergence {
                step,
                detail: format!("expected now(), recording has {other:?}"),
            }),
        }
    }

    fn input(&mut self, step: Step) -> Result<String, VmError> {
        match self.take(step, "input()")? {
            NondetEvent::Input { value, .. } => Ok(value.clone()),
            other => Err(VmError::Divergence {
                step,
                detail: format!("expected input(), recording has {other:?}"),
            }),
        }
    }
}
