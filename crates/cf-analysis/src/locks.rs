//! Lock-order graph ("Goodlock"). If one thread takes A then B and another
//! takes B then A, the program *can* deadlock — even if this run got lucky.

use std::collections::{BTreeMap, BTreeSet};

use cf_trace::{Step, StepEvent, SyncOp, Tid};
use serde::Serialize;

use crate::names::Names;

#[derive(Clone, Debug, Serialize)]
pub struct LockEdge {
    /// Holding `from`...
    pub from: u32,
    /// ...while acquiring `to`.
    pub to: u32,
    pub tid: Tid,
    pub step: Step,
    pub line: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct LockCycle {
    pub mutexes: Vec<String>,
    pub edges: Vec<LockEdge>,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct LockGraph {
    pub nodes: Vec<(u32, String)>,
    pub edges: Vec<LockEdge>,
    pub cycles: Vec<LockCycle>,
}

pub fn lock_graph(steps: &[StepEvent], names: &Names) -> LockGraph {
    let mut held: BTreeMap<Tid, Vec<u32>> = BTreeMap::new();
    // first witness of each (from, to, tid) is enough
    let mut edges: BTreeMap<(u32, u32, Tid), LockEdge> = BTreeMap::new();
    let mut nodes: BTreeSet<u32> = BTreeSet::new();

    for s in steps {
        match s.sync {
            Some(SyncOp::Acquire { mutex }) => {
                nodes.insert(mutex);
                let h = held.entry(s.tid).or_default();
                for &from in h.iter() {
                    edges.entry((from, mutex, s.tid)).or_insert(LockEdge {
                        from,
                        to: mutex,
                        tid: s.tid,
                        step: s.step,
                        line: s.line,
                    });
                }
                h.push(mutex);
            }
            // a blocked attempt is still an attempt: the edge exists even if
            // the acquire never completed (that's precisely the deadlock)
            Some(SyncOp::BlockLock { mutex, .. }) => {
                nodes.insert(mutex);
                if let Some(h) = held.get(&s.tid) {
                    for &from in h.iter() {
                        edges.entry((from, mutex, s.tid)).or_insert(LockEdge {
                            from,
                            to: mutex,
                            tid: s.tid,
                            step: s.step,
                            line: s.line,
                        });
                    }
                }
            }
            Some(SyncOp::Release { mutex }) => {
                if let Some(h) = held.get_mut(&s.tid) {
                    h.retain(|m| *m != mutex);
                }
            }
            _ => {}
        }
    }

    let edges: Vec<LockEdge> = edges.into_values().collect();
    let cycles = find_cycles(&edges, names);
    LockGraph {
        nodes: nodes.into_iter().map(|m| (m, names.mutex(m))).collect(),
        edges,
        cycles,
    }
}

/// Simple cycles in the lock graph whose edges come from at least two
/// different threads (a single thread can't deadlock with itself this way).
fn find_cycles(edges: &[LockEdge], names: &Names) -> Vec<LockCycle> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<Vec<u32>> = BTreeSet::new();

    fn dfs(
        edges: &[LockEdge],
        start: u32,
        path: &mut Vec<usize>,
        visited: &mut BTreeSet<u32>,
        found: &mut Vec<Vec<usize>>,
    ) {
        let at = path.last().map(|&e| edges[e].to).unwrap_or(start);
        for (i, e) in edges.iter().enumerate() {
            if e.from != at {
                continue;
            }
            if e.to == start {
                let mut cyc = path.clone();
                cyc.push(i);
                found.push(cyc);
            } else if e.to > start && !visited.contains(&e.to) && path.len() < 8 {
                // only explore nodes > start so each cycle is found once per rotation
                visited.insert(e.to);
                path.push(i);
                dfs(edges, start, path, visited, found);
                path.pop();
                visited.remove(&e.to);
            }
        }
    }

    let mut starts: Vec<u32> = edges.iter().map(|e| e.from).collect();
    starts.sort();
    starts.dedup();
    for start in starts {
        let mut found = Vec::new();
        dfs(
            edges,
            start,
            &mut Vec::new(),
            &mut BTreeSet::new(),
            &mut found,
        );
        for cyc in found {
            let threads: BTreeSet<Tid> = cyc.iter().map(|&i| edges[i].tid).collect();
            if threads.len() < 2 {
                continue;
            }
            let mut key: Vec<u32> = cyc.iter().map(|&i| edges[i].from).collect();
            key.sort();
            if !seen.insert(key) {
                continue;
            }
            let es: Vec<LockEdge> = cyc.iter().map(|&i| edges[i].clone()).collect();
            let description = es
                .iter()
                .map(|e| {
                    format!(
                        "T{} holds {} and wants {} (line {})",
                        e.tid,
                        names.mutex(e.from),
                        names.mutex(e.to),
                        e.line
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            out.push(LockCycle {
                mutexes: es.iter().map(|e| names.mutex(e.from)).collect(),
                edges: es,
                description,
            });
        }
    }
    out
}
