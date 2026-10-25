//! Backward dynamic slicing: starting from the failing instruction, follow
//! the recorded data dependencies back to everything that influenced it.
//! The VM tags every operand with the step that produced it, so this is a
//! plain graph walk — no static analysis guesswork.

use std::collections::{BTreeSet, VecDeque};

use cf_trace::{NondetKind, OpTag, Step, StepEvent, Tid};
use serde::Serialize;

use crate::names::Names;

#[derive(Clone, Debug, Serialize)]
pub struct SliceNode {
    pub step: Step,
    pub tid: Tid,
    pub line: u32,
    pub op: OpTag,
    /// Hops away from the slicing criterion.
    pub depth: u32,
    pub what: String,
    pub deps: Vec<Step>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Slice {
    pub criterion: Step,
    pub nodes: Vec<SliceNode>,
    pub lines: Vec<u32>,
    pub truncated: bool,
}

impl Slice {
    pub fn contains(&self, step: Step) -> bool {
        self.nodes.binary_search_by_key(&step, |n| n.step).is_ok()
    }
}

pub fn describe(s: &StepEvent, names: &Names) -> String {
    if let Some(k) = s.nondet {
        let f = match k {
            NondetKind::Rand => "rand()",
            NondetKind::Clock => "now()",
            NondetKind::Input => "input()",
        };
        let v = s.result.as_ref().map(|v| v.to_string()).unwrap_or_default();
        return format!("{f} → {v}");
    }
    if let Some(op) = s.sync {
        use cf_trace::SyncOp::*;
        let what = match op {
            Acquire { mutex } => format!("lock({}) acquired", names.mutex(mutex)),
            Release { mutex } => format!("unlock({})", names.mutex(mutex)),
            BlockLock { mutex, owner } => {
                format!("lock({}) blocked — held by T{owner}", names.mutex(mutex))
            }
            BlockJoin { target } => format!("join(T{target}) waiting"),
            Join { target } => format!("join(T{target})"),
            Spawn { child } => format!("spawn → T{child}"),
            ThreadExit => "thread exits".into(),
        };
        return what;
    }
    if let Some(w) = s.writes.first() {
        let name = names.loc(w.loc);
        return if w.old == w.new || matches!(s.op, OpTag::Call | OpTag::Spawn | OpTag::Array) {
            format!("{name} = {}", w.new)
        } else {
            format!("{name}: {} → {}", w.old, w.new)
        };
    }
    if let Some(r) = s.reads.first() {
        return format!("read {} = {}", names.loc(r.loc), r.val);
    }
    match (&s.op, &s.result) {
        (OpTag::Call, _) => format!("call {}", names.func(s.func)),
        (OpTag::Assert, None) => format!("assert(…) failed: {}", names.source_line(s.line)),
        (OpTag::Assert, Some(_)) => "assert(…) held".into(),
        (OpTag::Index | OpTag::StoreIndex, None) => {
            format!("index out of bounds: {}", names.source_line(s.line))
        }
        (op, Some(v)) => format!("{op:?} → {v}"),
        (op, None) => format!("{op:?}"),
    }
}

/// All steps that `from` transitively depends on (including itself),
/// nearest first. `limit` caps the walk for pathological traces.
pub fn ancestors(steps: &[StepEvent], from: Step, limit: usize) -> Vec<(Step, u32)> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut queue = VecDeque::from([(from, 0u32)]);
    while let Some((s, depth)) = queue.pop_front() {
        if !seen.insert(s) {
            continue;
        }
        out.push((s, depth));
        if out.len() >= limit {
            break;
        }
        if let Some(ev) = steps.get(s as usize) {
            for &d in &ev.deps {
                if d < s {
                    queue.push_back((d, depth + 1));
                }
            }
        }
    }
    out
}

pub fn backward_slice(steps: &[StepEvent], criterion: Step, names: &Names, limit: usize) -> Slice {
    let found = ancestors(steps, criterion, limit + 1);
    let truncated = found.len() > limit;
    let mut nodes: Vec<SliceNode> = found
        .into_iter()
        .take(limit)
        .filter_map(|(s, depth)| steps.get(s as usize).map(|ev| (ev, depth)))
        .map(|(ev, depth)| SliceNode {
            step: ev.step,
            tid: ev.tid,
            line: ev.line,
            op: ev.op,
            depth,
            what: describe(ev, names),
            deps: ev.deps.clone(),
        })
        .collect();
    nodes.sort_by_key(|n| n.step);
    let lines: BTreeSet<u32> = nodes.iter().map(|n| n.line).collect();
    Slice {
        criterion,
        nodes,
        lines: lines.into_iter().collect(),
        truncated,
    }
}
