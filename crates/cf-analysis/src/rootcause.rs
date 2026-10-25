//! Turns slices, races and lock graphs into ranked, evidence-backed
//! hypotheses. Every claim points at concrete step numbers, so the UI can
//! jump to them and a human (or an LLM) can check the reasoning.

use std::collections::BTreeSet;

use cf_trace::{Fault, FaultKind, Loc, OpTag, Step, StepEvent, Tid};
use serde::Serialize;

use crate::hb::Race;
use crate::locks::LockGraph;
use crate::names::Names;
use crate::slice::{ancestors, describe, Slice};

#[derive(Clone, Debug, Serialize)]
pub struct Evidence {
    pub step: Step,
    pub tid: Tid,
    pub line: u32,
    pub role: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Hypothesis {
    pub kind: String,
    pub title: String,
    pub explanation: String,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,
    pub fix: Option<String>,
}

fn ev(steps: &[StepEvent], step: Step, role: &str, names: &Names) -> Evidence {
    let s = &steps[step as usize];
    Evidence {
        step,
        tid: s.tid,
        line: s.line,
        role: role.into(),
        detail: describe(s, names),
    }
}

/// The classic lost update: thread T read X, someone else wrote X, then T
/// wrote a value computed from its stale read.
struct StaleWrite {
    loc: Loc,
    read: Step,
    interloper: Step,
    write: Step,
}

fn stale_writes(steps: &[StepEvent], slice: &Slice) -> Vec<StaleWrite> {
    let mut out = Vec::new();
    for node in &slice.nodes {
        let w_ev = &steps[node.step as usize];
        for w in w_ev.writes.iter().filter(|w| w.loc.is_shared()) {
            // which of this write's inputs was a read of the same location?
            let reads: Vec<Step> = ancestors(steps, w_ev.step, 256)
                .into_iter()
                .map(|(s, _)| s)
                .filter(|&s| {
                    let e = &steps[s as usize];
                    e.tid == w_ev.tid && e.reads.iter().any(|r| r.loc == w.loc)
                })
                .collect();
            for r in reads {
                let between = ((r + 1)..w_ev.step).find(|&s| {
                    let e = &steps[s as usize];
                    e.tid != w_ev.tid && e.writes.iter().any(|x| x.loc == w.loc)
                });
                if let Some(interloper) = between {
                    out.push(StaleWrite {
                        loc: w.loc,
                        read: r,
                        interloper,
                        write: w_ev.step,
                    });
                }
            }
        }
    }
    out.sort_by_key(|s| (s.write, s.read));
    out.dedup_by_key(|s| (s.loc, s.write));
    out
}

pub fn hypotheses(
    steps: &[StepEvent],
    fault: Option<&Fault>,
    slice: Option<&Slice>,
    races: &[Race],
    locks: &LockGraph,
    names: &Names,
) -> Vec<Hypothesis> {
    let mut out = Vec::new();
    let Some(fault) = fault else {
        // Nothing failed, but latent bugs are still worth reporting.
        for r in races.iter().take(3) {
            out.push(race_hypothesis(r, steps, names, 0.55, false));
        }
        for c in &locks.cycles {
            out.push(lock_cycle_hypothesis(c, steps, names, 0.5, false));
        }
        return out;
    };

    if let FaultKind::Deadlock { .. } = fault.kind {
        for c in &locks.cycles {
            out.push(lock_cycle_hypothesis(c, steps, names, 0.95, true));
        }
        if out.is_empty() {
            out.push(Hypothesis {
                kind: "deadlock".into(),
                title: "Threads blocked forever".into(),
                explanation: fault.message.clone(),
                confidence: 0.6,
                evidence: blocked_evidence(steps, names),
                fix: Some("Make sure every lock() has a matching unlock() on every path, and that joined threads can finish.".into()),
            });
        }
        return out;
    }

    let Some(slice) = slice else { return out };
    let slice_locs: BTreeSet<Loc> = slice
        .nodes
        .iter()
        .flat_map(|n| {
            let e = &steps[n.step as usize];
            e.reads
                .iter()
                .map(|r| r.loc)
                .chain(e.writes.iter().map(|w| w.loc))
                .collect::<Vec<_>>()
        })
        .collect();

    // 1. atomicity violations on the path to the failure
    for sw in stale_writes(steps, slice) {
        let name = names.loc(sw.loc);
        let (r, i, w) = (
            &steps[sw.read as usize],
            &steps[sw.interloper as usize],
            &steps[sw.write as usize],
        );
        let raced = races.iter().any(|x| x.loc == sw.loc);
        out.push(Hypothesis {
            kind: "atomicity-violation".into(),
            title: format!("Lost update on `{name}`"),
            explanation: format!(
                "T{} read `{name}` at step {} (line {}), then T{} overwrote it at step {} (line {}). \
                 T{} kept using its stale copy and wrote `{name}` at step {} (line {}), silently discarding T{}'s update. \
                 The failure at step {} depends on that write.",
                r.tid, r.step, r.line, i.tid, i.step, i.line, w.tid, w.step, w.line, i.tid, fault.step
            ),
            confidence: if raced { 0.94 } else { 0.85 },
            evidence: vec![
                ev(steps, sw.read, "stale read", names),
                ev(steps, sw.interloper, "interleaved write", names),
                ev(steps, sw.write, "lost update", names),
                ev(steps, fault.step, "failure", names),
            ],
            fix: Some(format!(
                "Make the read-modify-write of `{name}` atomic: hold one mutex from the read on line {} through the write on line {}.",
                r.line, w.line
            )),
        });
    }

    // 2. races on locations the failure depends on
    for race in races.iter().filter(|r| slice_locs.contains(&r.loc)) {
        if out.iter().any(|h| {
            h.kind == "atomicity-violation" && h.title.contains(&format!("`{}`", race.name))
        }) {
            continue;
        }
        out.push(race_hypothesis(race, steps, names, 0.78, true));
    }

    // 3. the failure depends on something from the outside world
    for n in slice.nodes.iter().filter(|n| n.op == OpTag::Nondet).take(3) {
        out.push(Hypothesis {
            kind: "nondeterministic-input".into(),
            title: format!("Depends on {}", n.what.split(' ').next().unwrap_or("input")),
            explanation: format!(
                "The failing value is derived from a nondeterministic input at step {} (line {}): {}. \
                 Other runs see different values, which is why this may only fail sometimes.",
                n.step, n.line, n.what
            ),
            confidence: 0.45,
            evidence: vec![ev(steps, n.step, "nondeterministic input", names), ev(steps, fault.step, "failure", names)],
            fix: None,
        });
    }

    // 4. an array whose *length* was decided by the outside world
    if fault.kind == FaultKind::IndexOutOfBounds {
        if let Some(h) = length_hypothesis(steps, fault, names) {
            out.push(h);
        }
    }

    // 5. where did the bad operand come from?
    out.push(origin_hypothesis(steps, fault, slice, names));

    out.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap());
    out
}

fn race_hypothesis(
    r: &Race,
    steps: &[StepEvent],
    names: &Names,
    confidence: f32,
    on_path: bool,
) -> Hypothesis {
    let tail = if on_path {
        " The failing computation reads this location."
    } else {
        " It didn't break this run, but another interleaving could."
    };
    Hypothesis {
        kind: "data-race".into(),
        title: format!("Data race on `{}`", r.name),
        explanation: format!(
            "T{} {} `{}` at step {} (line {}) and T{} {} it at step {} (line {}) with no happens-before ordering \
             between them (no common lock, spawn or join).{tail}",
            r.first.tid,
            verb(&r.first.kind),
            r.name,
            r.first.step,
            r.first.line,
            r.second.tid,
            verb(&r.second.kind),
            r.second.step,
            r.second.line,
        ),
        confidence,
        evidence: vec![ev(steps, r.first.step, "access", names), ev(steps, r.second.step, "conflicting access", names)],
        fix: Some(format!("Guard every access to `{}` with the same mutex.", r.name)),
    }
}

fn verb(k: &crate::hb::AccessKind) -> &'static str {
    match k {
        crate::hb::AccessKind::Read => "read",
        crate::hb::AccessKind::Write => "wrote",
    }
}

fn lock_cycle_hypothesis(
    c: &crate::locks::LockCycle,
    steps: &[StepEvent],
    names: &Names,
    confidence: f32,
    happened: bool,
) -> Hypothesis {
    let order: Vec<String> = c
        .edges
        .iter()
        .map(|e| {
            format!(
                "T{} takes {} then {} (line {})",
                e.tid,
                names.mutex(e.from),
                names.mutex(e.to),
                e.line
            )
        })
        .collect();
    Hypothesis {
        kind: "lock-order-inversion".into(),
        title: format!("Lock-order inversion: {}", c.mutexes.join(" ⇄ ")),
        explanation: format!(
            "{}. Each thread holds one lock while waiting for the other's. {}",
            order.join("; "),
            if happened { "That is exactly the cycle this run got stuck in." } else { "This run escaped, another may not." }
        ),
        confidence,
        evidence: c.edges.iter().map(|e| ev(steps, e.step, "acquire while holding", names)).collect(),
        fix: Some(format!("Pick one global order (e.g. always {} before the others) and acquire in that order everywhere.", c.mutexes[0])),
    }
}

fn blocked_evidence(steps: &[StepEvent], names: &Names) -> Vec<Evidence> {
    let mut last: std::collections::BTreeMap<Tid, Step> = Default::default();
    for s in steps {
        if matches!(
            s.sync,
            Some(cf_trace::SyncOp::BlockLock { .. } | cf_trace::SyncOp::BlockJoin { .. })
        ) {
            last.insert(s.tid, s.step);
        }
    }
    last.values()
        .map(|&s| ev(steps, s, "blocked here", names))
        .collect()
}

/// Data slicing alone can't see this one: the index is fine, the array is
/// just shorter (or longer) than usual because a loop ran a different
/// number of times. Follow the step that last grew the array to the branch
/// that controlled it, then that branch's inputs.
fn length_hypothesis(steps: &[StepEvent], fault: &Fault, names: &Names) -> Option<Hypothesis> {
    let fault_ev = steps.get(fault.step as usize)?;
    let grow = fault_ev
        .deps
        .iter()
        .copied()
        .filter(|&d| {
            steps[d as usize]
                .writes
                .iter()
                .any(|w| matches!(w.loc, Loc::Heap { .. }))
        })
        .max()?;

    // skip over call steps: a function's body is controlled by its call site
    let mut branch = steps[grow as usize].ctrl;
    while let Some(b) = branch {
        if matches!(steps[b as usize].op, OpTag::Call | OpTag::Spawn) {
            branch = steps[b as usize].ctrl;
        } else {
            break;
        }
    }
    let branch = branch?;
    let input = ancestors(steps, branch, 512)
        .into_iter()
        .map(|(s, _)| s)
        .filter(|&s| steps[s as usize].op == OpTag::Nondet)
        .min()?;

    let (g, b, i) = (
        &steps[grow as usize],
        &steps[branch as usize],
        &steps[input as usize],
    );
    Some(Hypothesis {
        kind: "nondeterministic-length".into(),
        title: format!("Array length decided by {}", describe(i, names).split(' ').next().unwrap_or("input")),
        explanation: format!(
            "The array was last grown at step {} (line {}: `{}`), under the condition at line {} (`{}`). \
             That condition depends on {} at step {} (line {}), so the array's length — and whether this access \
             runs off the end — changes from run to run. ({})",
            g.step,
            g.line,
            names.source_line(g.line),
            b.line,
            names.source_line(b.line),
            describe(i, names),
            i.step,
            i.line,
            fault.message
        ),
        confidence: 0.72,
        evidence: vec![
            ev(steps, input, "nondeterministic input", names),
            ev(steps, branch, "controlling branch", names),
            ev(steps, grow, "array grown", names),
            ev(steps, fault.step, "failure", names),
        ],
        fix: Some(format!(
            "The code at line {} assumes a length the input doesn't guarantee — bounds-check the index (or handle odd/even sizes) before indexing.",
            fault.line
        )),
    })
}

fn origin_hypothesis(
    steps: &[StepEvent],
    fault: &Fault,
    slice: &Slice,
    names: &Names,
) -> Hypothesis {
    // closest stores on the slice are the "named" values the failure was computed from
    let mut stores: Vec<&crate::slice::SliceNode> = slice
        .nodes
        .iter()
        .filter(|n| n.step != fault.step && !steps[n.step as usize].writes.is_empty())
        .collect();
    stores.sort_by_key(|n| (n.depth, std::cmp::Reverse(n.step)));
    stores.truncate(4);

    let mut evidence: Vec<Evidence> = stores
        .iter()
        .map(|n| ev(steps, n.step, "value origin", names))
        .collect();
    evidence.push(ev(steps, fault.step, "failure", names));

    let chain: Vec<String> = stores
        .iter()
        .map(|n| {
            format!(
                "`{}` (step {}, line {}: {})",
                n.what,
                n.step,
                n.line,
                names.source_line(n.line)
            )
        })
        .collect();
    let what = match fault.kind {
        FaultKind::IndexOutOfBounds => "The out-of-range index",
        FaultKind::DivisionByZero => "The zero divisor",
        FaultKind::AssertionFailed => "The failing condition",
        _ => "The faulting value",
    };
    Hypothesis {
        kind: "value-origin".into(),
        title: format!("{} at line {}", fault.kind.label(), fault.line),
        explanation: if chain.is_empty() {
            format!("{}: {}", fault.kind.label(), fault.message)
        } else {
            format!(
                "{what} was computed from {}. ({})",
                chain.join(", "),
                fault.message
            )
        },
        confidence: 0.35,
        evidence,
        fix: None,
    }
}
