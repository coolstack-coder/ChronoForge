//! "It passed yesterday." Compare two recordings step by step and report the
//! first point where they part ways. Both start from the same state, so
//! everything after the divergence is a consequence of it.

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
