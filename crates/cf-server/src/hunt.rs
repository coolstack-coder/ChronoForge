//! Chaos hunting: record the same program under many scheduler seeds and
//! keep every recording, so any failing (or passing) run can be opened and
//! replayed later without re-running anything.

use std::sync::Arc;
use std::time::Instant;

use cf_lang::Program;
use cf_trace::{NondetEvent, Recording};
use cf_vm::RecordOptions;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct HuntRun {
    pub seed: u64,
    pub failed: bool,
    pub kind: Option<String>,
    pub message: Option<String>,
    pub line: Option<u32>,
    pub steps: u64,
    pub switches: usize,
    pub signature: String,
}

pub struct Hunt {
    pub id: String,
    pub name: String,
    pub program: Arc<Program>,
    pub runs: Vec<HuntRun>,
    pub recordings: Vec<Recording>,
    pub elapsed_ms: f64,
}

#[derive(Serialize)]
pub struct HuntView<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub runs: &'a [HuntRun],
    pub failures: usize,
    pub signatures: Vec<(String, usize)>,
    pub elapsed_ms: f64,
}

impl Hunt {
    pub fn view(&self) -> HuntView<'_> {
        let mut sigs: std::collections::BTreeMap<String, usize> = Default::default();
        for r in &self.runs {
            *sigs.entry(r.signature.clone()).or_default() += 1;
        }
        let mut signatures: Vec<(String, usize)> = sigs.into_iter().collect();
        signatures.sort_by_key(|s| std::cmp::Reverse(s.1));
        HuntView {
            id: &self.id,
            name: &self.name,
            runs: &self.runs,
            failures: self.runs.iter().filter(|r| r.failed).count(),
            signatures,
            elapsed_ms: self.elapsed_ms,
        }
    }
}

pub fn run(
    id: String,
    name: String,
    program: Arc<Program>,
    seeds: u64,
    chaos: f64,
) -> Result<Hunt, String> {
    let t = Instant::now();
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16) as u64;
    let mut results: Vec<(u64, Result<Recording, String>)> = Vec::with_capacity(seeds as usize);

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let program = program.clone();
                let name = name.clone();
                scope.spawn(move || {
                    (w..seeds)
                        .step_by(workers as usize)
                        .map(|seed| {
                            let opts = RecordOptions {
                                name: name.clone(),
                                seed,
                                chaos,
                                max_steps: 500_000,
                                ..Default::default()
                            };
                            (
                                seed,
                                cf_vm::record(program.clone(), &opts).map_err(|e| e.to_string()),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            results.extend(h.join().expect("hunt worker panicked"));
        }
    });
    results.sort_by_key(|(s, _)| *s);

    let mut runs = Vec::new();
    let mut recordings = Vec::new();
    for (seed, rec) in results {
        let rec = rec?;
        let fault = rec.outcome.fault();
        runs.push(HuntRun {
            seed,
            failed: rec.outcome.is_failure(),
            kind: fault.map(|f| f.kind.label().to_string()),
            message: fault.map(|f| f.message.clone()),
            line: fault.map(|f| f.line),
            steps: rec.total_steps,
            switches: rec
                .nondet
                .iter()
                .filter(|e| matches!(e, NondetEvent::Schedule { .. }))
                .count(),
            signature: match fault {
                Some(f) => format!("{} @ line {}", f.kind.label(), f.line),
                None if rec.outcome.is_failure() => "step limit".into(),
                None => "pass".into(),
            },
        });
        recordings.push(rec);
    }
    Ok(Hunt {
        id,
        name,
        program,
        runs,
        recordings,
        elapsed_ms: t.elapsed().as_secs_f64() * 1000.0,
    })
}
