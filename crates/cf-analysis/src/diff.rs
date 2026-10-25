//! "It passed yesterday." Compare two recordings step by step and report the
//! first point where they part ways. Both start from the same state, so
//! everything after the divergence is a consequence of it.

use std::collections::BTreeMap;

use cf_lang::Program;
use cf_trace::{NondetEvent, Recording, Step, StepEvent, Tid};
use serde::Serialize;

use crate::names::Names;
use crate::slice::describe;

#[derive(Clone, Debug, Serialize)]
pub struct StepBrief {
    pub step: Step,
    pub tid: Tid,
    pub line: u32,
    pub code: String,
    pub what: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Divergence {
    /// schedule | value | control | length | identical
    pub kind: String,
    pub index: Step,
    pub common_prefix: Step,
    pub a: Option<StepBrief>,
    pub b: Option<StepBrief>,
    pub nondet_a: Option<NondetEvent>,
    pub nondet_b: Option<NondetEvent>,
    pub explanation: String,
    pub outcome_a: String,
    pub outcome_b: String,
    /// The first shared location whose sequence of written values differs.
    /// Schedules often diverge harmlessly early on; this is where the runs
    /// actually start computing different things.
    pub data: Option<DataDivergence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WriteBrief {
    pub step: Step,
    pub tid: Tid,
    pub line: u32,
    pub value: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DataDivergence {
    pub name: String,
    /// Which write to `name` (0-based) differs.
    pub write: usize,
    pub a: Option<WriteBrief>,
    pub b: Option<WriteBrief>,
    pub explanation: String,
}

fn shared_writes(steps: &[StepEvent], names: &Names) -> BTreeMap<String, Vec<WriteBrief>> {
    let mut out: BTreeMap<String, Vec<WriteBrief>> = BTreeMap::new();
    for s in steps {
        for w in s.writes.iter().filter(|w| w.loc.is_shared()) {
            out.entry(names.loc(w.loc)).or_default().push(WriteBrief {
                step: s.step,
                tid: s.tid,
                line: s.line,
                value: w.new.to_string(),
            });
        }
    }
    out
}

fn data_divergence(
    a: &[StepEvent],
    na: &Names,
    b: &[StepEvent],
    nb: &Names,
) -> Option<DataDivergence> {
    let (wa, wb) = (shared_writes(a, na), shared_writes(b, nb));
    let mut best: Option<(Step, DataDivergence)> = None;
    for name in wa.keys().chain(wb.keys()) {
        let (xs, ys) = (
            wa.get(name).map(Vec::as_slice).unwrap_or(&[]),
            wb.get(name).map(Vec::as_slice).unwrap_or(&[]),
        );
        let Some(k) = (0..xs.len().max(ys.len()))
            .find(|&k| xs.get(k).map(|x| &x.value) != ys.get(k).map(|y| &y.value))
        else {
            continue;
        };
        let (x, y) = (xs.get(k).cloned(), ys.get(k).cloned());
        let when = x
            .iter()
            .chain(y.iter())
            .map(|w| w.step)
            .min()
            .unwrap_or(Step::MAX);
        if best.as_ref().is_some_and(|(s, _)| *s <= when) {
            continue;
        }
        let show = |w: &Option<WriteBrief>| match w {
            Some(w) => format!(
                "T{} wrote {} at step {} (line {})",
                w.tid, w.value, w.step, w.line
            ),
            None => "never wrote it".into(),
        };
        let explanation = format!(
            "Write #{} to `{name}` is where the data parts ways: in the passing run {}, in the failing run {}.",
            k + 1,
            show(&x),
            show(&y)
        );
        best = Some((
            when,
            DataDivergence {
                name: name.clone(),
                write: k,
                a: x,
                b: y,
                explanation,
            },
        ));
    }
    best.map(|(_, d)| d)
}

fn brief(s: &StepEvent, names: &Names) -> StepBrief {
    StepBrief {
        step: s.step,
        tid: s.tid,
        line: s.line,
        code: names.source_line(s.line),
        what: describe(s, names),
    }
}

fn outcome(r: &Recording) -> String {
    match r.outcome.fault() {
        Some(f) => format!("{} at step {} (line {})", f.kind.label(), f.step, f.line),
        None if r.outcome.is_failure() => "step limit".into(),
        None => "completed".into(),
    }
}

pub struct Side<'a> {
    pub program: &'a Program,
    pub recording: &'a Recording,
    pub steps: &'a [StepEvent],
}

pub fn first_divergence(a: Side, b: Side) -> Divergence {
    let na = Names::new(a.program, a.steps);
    let nb = Names::new(b.program, b.steps);
    let same_code = a.program.source == b.program.source;

    // with different code, pcs mean nothing: compare by source text instead
    let same_place = |x: &StepEvent, y: &StepEvent| {
        if same_code {
            x.func == y.func && x.pc == y.pc
        } else {
            x.op == y.op && na.source_line(x.line) == nb.source_line(y.line)
        }
    };

    let n = a.steps.len().min(b.steps.len());
    let mut i = 0;
    while i < n {
        let (x, y) = (&a.steps[i], &b.steps[i]);
        if x.tid != y.tid || !same_place(x, y) || x.writes != y.writes || x.result != y.result {
            break;
        }
        i += 1;
    }

    let nondet_at = |r: &Recording, step: Step| r.nondet.iter().find(|e| e.step() == step).cloned();
    let mut d = Divergence {
        kind: "identical".into(),
        index: i as Step,
        common_prefix: i as Step,
        a: a.steps.get(i).map(|s| brief(s, &na)),
        b: b.steps.get(i).map(|s| brief(s, &nb)),
        nondet_a: nondet_at(a.recording, i as Step),
        nondet_b: nondet_at(b.recording, i as Step),
        explanation: String::new(),
        outcome_a: outcome(a.recording),
        outcome_b: outcome(b.recording),
        data: data_divergence(a.steps, &na, b.steps, &nb),
    };

    if i == n {
        if a.steps.len() == b.steps.len() {
            d.explanation = "The two runs executed identically.".into();
        } else {
            d.kind = "length".into();
            d.explanation = format!(
                "Both runs agree for {i} steps, then one stops: A ran {} steps, B ran {}.",
                a.steps.len(),
                b.steps.len()
            );
        }
        return d;
    }

    let (x, y) = (&a.steps[i], &b.steps[i]);
    if x.tid != y.tid {
        d.kind = "schedule".into();
        d.explanation = format!(
            "After {i} identical steps the scheduler made a different choice: run A kept going with T{} at line {} \
             (`{}`), run B switched to T{} at line {} (`{}`). No input differed — only the thread interleaving.",
            x.tid,
            x.line,
            na.source_line(x.line),
            y.tid,
            y.line,
            nb.source_line(y.line)
        );
    } else if !same_place(x, y) {
        d.kind = if same_code { "control" } else { "code" }.into();
        d.explanation = format!(
            "T{} took a different path at step {i}: line {} in A versus line {} in B.{}",
            x.tid,
            x.line,
            y.line,
            if same_code {
                " An earlier value must have steered a branch differently."
            } else {
                " The programs differ here."
            }
        );
    } else {
        d.kind = "value".into();
        d.explanation = format!(
            "Same instruction (T{}, line {}), different data: A computed `{}`, B computed `{}`.",
            x.tid,
            x.line,
            describe(x, &na),
            describe(y, &nb)
        );
    }
    d
}
