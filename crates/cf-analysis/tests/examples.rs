use std::sync::Arc;

use cf_analysis::*;
use cf_trace::{FaultKind, Recording, StepEvent};
use cf_vm::{record, replay, RecordOptions};

fn load(name: &str) -> Arc<cf_lang::Program> {
    let path = format!("{}/../../examples/forge/{name}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(path).unwrap();
    Arc::new(cf_lang::compile(&src).unwrap())
}

fn run(p: &Arc<cf_lang::Program>, seed: u64) -> (Recording, Vec<StepEvent>) {
    let rec = record(
        p.clone(),
        &RecordOptions {
            seed,
            chaos: 0.25,
            ..Default::default()
        },
    )
    .unwrap();
    let mut steps = Vec::new();
    replay(p.clone(), &rec, &mut steps).unwrap();
    (rec, steps)
}

fn find(p: &Arc<cf_lang::Program>, want_failure: bool) -> (Recording, Vec<StepEvent>) {
    (0..500)
        .map(|s| run(p, s))
        .find(|(r, _)| r.outcome.is_failure() == want_failure)
        .expect("no seed produced the wanted outcome")
}

#[test]
fn bank_lost_update_is_the_top_hypothesis() {
    let p = load("bank.forge");
    let (rec, steps) = find(&p, true);
    let report = analyze(&p, &rec, &steps);

    assert!(
        report.races.iter().any(|r| r.name == "balance"),
        "{:#?}",
        report.races
    );
    let top = &report.hypotheses[0];
    assert_eq!(top.kind, "atomicity-violation", "{top:#?}");
    assert!(top.title.contains("balance"));
    assert!(top.evidence.len() >= 3);
    // the evidence must point at real steps, in order
    let s: Vec<u64> = top.evidence.iter().map(|e| e.step).collect();
    assert!(s.windows(2).all(|w| w[0] < w[1]), "{s:?}");
}

#[test]
fn races_are_found_even_when_the_run_passes() {
    let p = load("bank.forge");
    let (rec, steps) = find(&p, false);
    let report = analyze(&p, &rec, &steps);
    // happens-before catches it even if the bad interleaving didn't happen
    let both_ran = steps.iter().any(|s| s.tid == 1) && steps.iter().any(|s| s.tid == 2);
    assert!(both_ran);
    assert!(report.races.iter().any(|r| r.name == "balance"));
    assert!(report.hypotheses.iter().any(|h| h.kind == "data-race"));
}

#[test]
fn locked_counter_is_clean() {
    let p = load("counter.forge");
    for seed in 0..20 {
        let (rec, steps) = run(&p, seed);
        assert!(!rec.outcome.is_failure());
        let report = analyze(&p, &rec, &steps);
        assert!(
            report.races.is_empty(),
            "false positive: {:#?}",
            report.races
        );
        assert!(report.locks.cycles.is_empty());
    }
}

#[test]
fn philosophers_deadlock_names_the_forks() {
    let p = load("philosophers.forge");
    let (rec, steps) = find(&p, true);
    assert!(matches!(
        rec.outcome.fault().unwrap().kind,
        FaultKind::Deadlock { .. }
    ));
    let report = analyze(&p, &rec, &steps);
    let cycle = &report.locks.cycles[0];
    assert!(
        cycle.mutexes.iter().all(|m| m.starts_with("forks[")),
        "{cycle:#?}"
    );
    assert_eq!(report.hypotheses[0].kind, "lock-order-inversion");
}

#[test]
fn checkout_points_at_the_index_and_the_random_basket() {
    let p = load("checkout.forge");
    let (rec, steps) = find(&p, true);
    assert_eq!(
        rec.outcome.fault().unwrap().kind,
        FaultKind::IndexOutOfBounds
    );
    let report = analyze(&p, &rec, &steps);
    let slice = report.slice.as_ref().unwrap();
    assert!(
        slice.lines.contains(&17),
        "slice should include the i + 1 line: {:?}",
        slice.lines
    );
    assert!(
        report
            .hypotheses
            .iter()
            .any(|h| h.kind == "nondeterministic-length"),
        "{:#?}",
        report.hypotheses
    );
    assert!(report.hypotheses.iter().any(|h| h.kind == "value-origin"));
}

#[test]
fn diff_blames_the_scheduler() {
    let p = load("bank.forge");
    let (good, good_steps) = find(&p, false);
    let (bad, bad_steps) = find(&p, true);
    let d = first_divergence(
        Side {
            program: &p,
            recording: &good,
            steps: &good_steps,
        },
        Side {
            program: &p,
            recording: &bad,
            steps: &bad_steps,
        },
    );
    assert_eq!(d.kind, "schedule", "{d:#?}");
    assert!(d.common_prefix > 0);
}
