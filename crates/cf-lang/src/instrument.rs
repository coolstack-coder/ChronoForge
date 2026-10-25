//! Compile-time instrumentation pass.
//!
//! Same idea as an LLVM sanitizer pass: walk the code once and decide which
//! instructions need a probe and what kind. The VM consults the plan, so a
//! `LoadLocal` never pays for a race check and a `LoadGlobal` always does.

use serde::Serialize;

use crate::bytecode::{Builtin, Function, Instr};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Probe(u8);

impl Probe {
    pub const NONE: Probe = Probe(0);
    /// Record reads/writes for the timeline and the data-flow graph.
    pub const ACCESS: Probe = Probe(1);
    /// Memory that may be shared between threads: feed the race detector.
    pub const SHARED: Probe = Probe(2);
    /// Happens-before edge (spawn/join/lock/unlock).
    pub const SYNC: Probe = Probe(4);
    /// Pulls a value from outside the program: must go in the recording.
    pub const NONDET: Probe = Probe(8);

    pub fn has(self, other: Probe) -> bool {
        self.0 & other.0 != 0
    }

    fn with(self, other: Probe) -> Probe {
        Probe(self.0 | other.0)
    }

    pub fn label(self) -> &'static str {
        if self.has(Probe::NONDET) {
            "nondet"
        } else if self.has(Probe::SYNC) {
            "sync"
        } else if self.has(Probe::SHARED) {
            "shared"
        } else if self.has(Probe::ACCESS) {
            "access"
        } else {
            ""
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Plan {
    probes: Vec<Vec<Probe>>,
    pub stats: PlanStats,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PlanStats {
    pub instructions: usize,
    pub access_probes: usize,
    pub shared_probes: usize,
    pub sync_probes: usize,
    pub nondet_probes: usize,
}

impl Plan {
    pub fn probe(&self, func: u32, pc: u32) -> Probe {
        self.probes
            .get(func as usize)
            .and_then(|f| f.get(pc as usize))
            .copied()
            .unwrap_or(Probe::NONE)
    }
}

pub fn instrument(funcs: &[Function]) -> Plan {
    let mut plan = Plan::default();
    for f in funcs {
        let probes: Vec<Probe> = f.code.iter().map(probe_for).collect();
        for p in &probes {
            plan.stats.instructions += 1;
            plan.stats.access_probes += p.has(Probe::ACCESS) as usize;
            plan.stats.shared_probes += p.has(Probe::SHARED) as usize;
            plan.stats.sync_probes += p.has(Probe::SYNC) as usize;
            plan.stats.nondet_probes += p.has(Probe::NONDET) as usize;
        }
        plan.probes.push(probes);
    }
    plan
}

fn probe_for(ins: &Instr) -> Probe {
    match ins {
        Instr::LoadLocal(_) | Instr::StoreLocal(_) => Probe::ACCESS,
        // heap cells are reachable from any thread holding the array ref
        Instr::LoadGlobal(_) | Instr::StoreGlobal(_) | Instr::Index | Instr::StoreIndex => {
            Probe::ACCESS.with(Probe::SHARED)
        }
        Instr::Builtin {
            b: Builtin::Push | Builtin::Len,
            ..
        } => Probe::ACCESS.with(Probe::SHARED),
        Instr::Spawn { .. } => Probe::SYNC,
        Instr::Builtin { b, .. }
            if matches!(b, Builtin::Lock | Builtin::Unlock | Builtin::Join) =>
        {
            Probe::SYNC
        }
        Instr::Builtin { b, .. } if b.is_nondeterministic() => Probe::NONDET,
        _ => Probe::NONE,
    }
}
