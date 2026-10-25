//! Happens-before race detection with vector clocks, FastTrack style.
//!
//! The nice property: it finds races that *didn't* cause a failure in this
//! particular run. Two unordered accesses are a race whether or not the
//! scheduler happened to interleave them badly this time.

use std::collections::{BTreeMap, HashMap};

use cf_trace::{Loc, Step, StepEvent, SyncOp, Tid, Val};
use serde::Serialize;

use crate::names::Names;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VectorClock(Vec<u32>);

impl VectorClock {
    pub fn get(&self, t: Tid) -> u32 {
        self.0.get(t as usize).copied().unwrap_or(0)
    }

    pub fn set(&mut self, t: Tid, v: u32) {
        if self.0.len() <= t as usize {
            self.0.resize(t as usize + 1, 0);
        }
        self.0[t as usize] = v;
    }

    pub fn tick(&mut self, t: Tid) {
        let v = self.get(t) + 1;
        self.set(t, v);
    }

    pub fn join(&mut self, other: &VectorClock) {
        for (t, v) in other.0.iter().enumerate() {
            if *v > self.get(t as Tid) {
                self.set(t as Tid, *v);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Access {
    pub step: Step,
    pub tid: Tid,
    pub line: u32,
    pub kind: AccessKind,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Race {
    pub loc: Loc,
    pub name: String,
    /// "write-write", "read-write" or "write-read" (earlier access first).
    pub kind: String,
    pub first: Access,
    pub second: Access,
    /// How many unordered pairs we saw on these two source lines.
    pub occurrences: u32,
}

#[derive(Clone, Copy)]
struct Epoch {
    tid: Tid,
    clock: u32,
    step: Step,
    line: u32,
}

#[derive(Default)]
struct Shadow {
    write: Option<(Epoch, Val)>,
    reads: BTreeMap<Tid, (Epoch, Val)>,
}

/// Per-thread vector clocks after each step, keyed by step. Other analyses
/// (the diff, the UI's "concurrent with" highlight) reuse this.
pub struct HbState {
    clocks: Vec<VectorClock>,
    locks: HashMap<u32, VectorClock>,
    exited: HashMap<Tid, VectorClock>,
}

impl HbState {
    fn new() -> Self {
        let mut main = VectorClock::default();
        main.set(0, 1);
        HbState {
            clocks: vec![main],
            locks: HashMap::new(),
            exited: HashMap::new(),
        }
    }

    fn clock(&mut self, t: Tid) -> &mut VectorClock {
        while self.clocks.len() <= t as usize {
            self.clocks.push(VectorClock::default());
        }
        &mut self.clocks[t as usize]
    }

    fn sync(&mut self, t: Tid, op: SyncOp) {
        match op {
            SyncOp::Spawn { child } => {
                let mut c = self.clock(t).clone();
                c.set(child, 1);
                *self.clock(child) = c;
                self.clock(t).tick(t);
            }
            SyncOp::Release { mutex } => {
                let c = self.clock(t).clone();
                self.locks.insert(mutex, c);
                self.clock(t).tick(t);
            }
            SyncOp::Acquire { mutex } => {
                if let Some(l) = self.locks.get(&mutex).cloned() {
                    self.clock(t).join(&l);
                }
            }
            SyncOp::ThreadExit => {
                let c = self.clock(t).clone();
                self.exited.insert(t, c);
            }
            SyncOp::Join { target } => {
                if let Some(c) = self.exited.get(&target).cloned() {
                    self.clock(t).join(&c);
                }
            }
            SyncOp::BlockLock { .. } | SyncOp::BlockJoin { .. } => {}
        }
    }
}

fn ordered(prev: &Epoch, now: &VectorClock) -> bool {
    prev.clock <= now.get(prev.tid)
}

pub fn detect_races(steps: &[StepEvent], names: &Names) -> Vec<Race> {
    let mut hb = HbState::new();
    let mut shadow: HashMap<Loc, Shadow> = HashMap::new();
    // dedupe by (loc, line, line) — a loop shouldn't produce 500 identical reports
    let mut found: BTreeMap<(Loc, u32, u32, &'static str), Race> = BTreeMap::new();

    let mut report = |loc: Loc,
                      kind: &'static str,
                      a: (Epoch, AccessKind, &Val),
                      b: (Epoch, AccessKind, &Val)| {
        let key = (loc, a.0.line, b.0.line, kind);
        found
            .entry(key)
            .and_modify(|r| r.occurrences += 1)
            .or_insert_with(|| Race {
                loc,
                name: names.loc(loc),
                kind: kind.to_string(),
                first: Access {
                    step: a.0.step,
                    tid: a.0.tid,
                    line: a.0.line,
                    kind: a.1,
                    value: a.2.to_string(),
                },
                second: Access {
                    step: b.0.step,
                    tid: b.0.tid,
                    line: b.0.line,
                    kind: b.1,
                    value: b.2.to_string(),
                },
                occurrences: 1,
            });
    };

    for s in steps {
        let t = s.tid;
        let now = hb.clock(t).clone();
        let me = Epoch {
            tid: t,
            clock: now.get(t),
            step: s.step,
            line: s.line,
        };

        for r in s.reads.iter().filter(|r| r.loc.is_shared()) {
            let sh = shadow.entry(r.loc).or_default();
            if let Some((w, wv)) = &sh.write {
                if w.tid != t && !ordered(w, &now) {
                    report(
                        r.loc,
                        "write-read",
                        (*w, AccessKind::Write, wv),
                        (me, AccessKind::Read, &r.val),
                    );
                }
            }
            sh.reads.insert(t, (me, r.val.clone()));
        }

        for w in s.writes.iter().filter(|w| w.loc.is_shared()) {
            let sh = shadow.entry(w.loc).or_default();
            if let Some((pw, pv)) = &sh.write {
                if pw.tid != t && !ordered(pw, &now) {
                    report(
                        w.loc,
                        "write-write",
                        (*pw, AccessKind::Write, pv),
                        (me, AccessKind::Write, &w.new),
                    );
                }
            }
            for (rt, (re, rv)) in &sh.reads {
                if *rt != t && !ordered(re, &now) {
                    report(
                        w.loc,
                        "read-write",
                        (*re, AccessKind::Read, rv),
                        (me, AccessKind::Write, &w.new),
                    );
                }
            }
            sh.write = Some((me, w.new.clone()));
            sh.reads.clear();
        }

        if let Some(op) = s.sync {
            hb.sync(t, op);
        }
    }

    let mut races: Vec<Race> = found.into_values().collect();
    races.sort_by_key(|r| (r.first.step, r.second.step));
    races
}

/// Vector clock of every step's thread at that step, for "which steps are
/// concurrent with this one" queries.
pub fn clocks_per_step(steps: &[StepEvent]) -> Vec<VectorClock> {
    let mut hb = HbState::new();
    let mut out = Vec::with_capacity(steps.len());
    for s in steps {
        out.push(hb.clock(s.tid).clone());
        if let Some(op) = s.sync {
            hb.sync(s.tid, op);
        }
    }
    out
}
