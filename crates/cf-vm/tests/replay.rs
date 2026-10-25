use std::sync::Arc;

use cf_trace::{FaultKind, NondetEvent, Outcome};
use cf_vm::*;

fn program(src: &str) -> Arc<cf_lang::Program> {
    Arc::new(cf_lang::compile(src).expect("compiles"))
}

const BANK: &str = r#"
let balance = 100;
let paid = 0;
let m = mutex();
fn withdraw(amount) {
    let b = balance;
    if b >= amount {
        balance = b - amount;
        lock(m);
        paid = paid + amount;
        unlock(m);
    }
}
let a = spawn withdraw(80);
let b = spawn withdraw(50);
join(a);
join(b);
assert(balance + paid == 100, "money appeared: balance=" + str(balance) + " paid=" + str(paid));
"#;

fn opts(seed: u64) -> RecordOptions {
    RecordOptions {
        seed,
        chaos: 0.3,
        ..Default::default()
    }
}

#[test]
fn arithmetic_and_calls() {
    let p = program(
        "fn fib(n) { if n < 2 { return n; } return fib(n - 1) + fib(n - 2); }\nprint(fib(15));",
    );
    let rec = record(p, &RecordOptions::default()).unwrap();
    assert_eq!(rec.outcome, Outcome::Completed);
    assert_eq!(rec.output[0].text, "610");
}

#[test]
fn recursion_at_pc_zero() {
    // regression: the call is the very first instruction of the function
    let p = program("let n = 0;\nfn f() { n += 1; if n < 5 { f(); } }\nf();\nassert(n == 5);");
    assert_eq!(
        record(p, &RecordOptions::default()).unwrap().outcome,
        Outcome::Completed
    );
}

#[test]
fn replay_reproduces_every_seed_exactly() {
    let p = program(BANK);
    for seed in 0..60 {
        let (rec, steps) = record_traced(p.clone(), &opts(seed)).unwrap();
        let mut replayed = Vec::new();
        let m = replay(p.clone(), &rec, &mut replayed).unwrap();
        assert_eq!(m.outcome.as_ref(), Some(&rec.outcome), "seed {seed}");
        assert_eq!(
            replayed, steps,
            "seed {seed}: instrumented trace differs on replay"
        );
    }
}

#[test]
fn chaos_finds_the_lost_update() {
    let p = program(BANK);
    let outcomes: Vec<Outcome> = (0..200)
        .map(|s| record(p.clone(), &opts(s)).unwrap().outcome)
        .collect();
    let failures = outcomes.iter().filter(|o| o.is_failure()).count();
    assert!(failures > 0, "chaos scheduler never hit the race");
    assert!(failures < 200, "race should not fire every time");
    let f = outcomes.iter().find_map(|o| o.fault()).unwrap();
    assert_eq!(f.kind, FaultKind::AssertionFailed);
}

#[test]
fn clock_and_input_come_back_from_the_log() {
    let p = program("let t = now();\nlet s = input();\nlet r = rand(1000);\nprint(t, s, r);");
    let rec = record(
        p.clone(),
        &RecordOptions {
            inputs: vec!["hello".into()],
            ..Default::default()
        },
    )
    .unwrap();
    // time moves on; replay must still print the recorded clock value
    std::thread::sleep(std::time::Duration::from_millis(5));
    let m = replay(p, &rec, &mut Quiet).unwrap();
    assert_eq!(m.output, rec.output);
    assert!(rec
        .nondet
        .iter()
        .any(|e| matches!(e, NondetEvent::Clock { .. })));
}

#[test]
fn tampered_log_is_detected() {
    let p = program("let r = rand(10);\nprint(r);");
    let mut rec = record(p.clone(), &RecordOptions::default()).unwrap();
    rec.nondet.clear();
    assert!(matches!(
        replay(p, &rec, &mut Quiet),
        Err(VmError::Divergence { .. })
    ));
}

#[test]
fn deadlock_reports_the_cycle() {
    let src = r#"
let a = mutex();
let b = mutex();
fn left() { lock(a); yield(); lock(b); unlock(b); unlock(a); }
fn right() { lock(b); yield(); lock(a); unlock(a); unlock(b); }
let t1 = spawn left();
let t2 = spawn right();
join(t1);
join(t2);
"#;
    let p = program(src);
    let dl = (0..100)
        .map(|s| record(p.clone(), &opts(s)).unwrap())
        .find(|r| {
            matches!(
                r.outcome.fault().map(|f| &f.kind),
                Some(FaultKind::Deadlock { .. })
            )
        })
        .expect("some interleaving deadlocks");
    let Some(FaultKind::Deadlock { cycle }) = dl.outcome.fault().map(|f| f.kind.clone()) else {
        unreachable!()
    };
    let mut tids: Vec<u32> = cycle.iter().map(|c| c.0).collect();
    tids.sort();
    assert_eq!(tids, vec![1, 2]);
    // and the deadlock replays too
    replay(p, &dl, &mut Quiet).unwrap();
}

#[test]
fn seeking_backwards_matches_straight_replay() {
    let p = program(BANK);
    let rec = Arc::new(record(p.clone(), &opts(7)).unwrap());
    let tl = Timeline::build(p.clone(), rec.clone(), 16, &mut Quiet).unwrap();
    assert!(tl.checkpoints() > 1);
    // walk backwards from the end, comparing against a fresh replay each time
    for pos in (0..=rec.total_steps).rev().step_by(7) {
        let a = tl.seek(pos).unwrap().snapshot();
        let mut fresh = cf_vm::Machine::new(p.clone());
        let mut oracle = ReplayOracle::new(rec.nondet.clone().into());
        while fresh.step < pos {
            fresh.step_once(&mut oracle, &mut Quiet).unwrap();
        }
        let b = fresh.snapshot();
        assert_eq!(serde_json_like(&a), serde_json_like(&b), "position {pos}");
    }
}

fn serde_json_like(s: &Snapshot) -> String {
    format!(
        "{:?}",
        (
            &s.position,
            s.globals.iter().map(|g| &g.value).collect::<Vec<_>>(),
            s.threads.len()
        )
    )
}

#[test]
fn shadow_writers_point_at_the_store() {
    let p = program("let x = 1;\nx = x + 41;\nprint(x);");
    let (rec, steps) = record_traced(p.clone(), &RecordOptions::default()).unwrap();
    let tl = Timeline::build(p, Arc::new(rec), 4, &mut Quiet).unwrap();
    let snap = tl.seek(tl.total_steps()).unwrap().snapshot();
    let x = &snap.globals[0];
    assert_eq!(x.value, "42");
    let w = x.writer.unwrap() as usize;
    assert_eq!(steps[w].line, 2);
    assert_eq!(steps[w].writes[0].new, cf_trace::Val::Int(42));
}
