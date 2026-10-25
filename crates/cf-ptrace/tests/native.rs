//! These need a real Linux kernel; CI runs them on x86_64 and arm64.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

use cf_ptrace::*;

const INPUT: &[u8] = b"GET /\nGET /health\n";

fn build() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("flaky");
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/native/flaky.c");
    let ok = Command::new("cc")
        .args(["-O0", "-g", "-o"])
        .arg(&out)
        .arg(src)
        .status()
        .unwrap();
    assert!(ok.success(), "cc failed");
    (dir, out)
}

fn rec(bin: &Path) -> NativeRecording {
    let cfg = RecordConfig {
        program: bin.to_path_buf(),
        args: vec![],
        stdin: INPUT.to_vec(),
    };
    record(&cfg).expect("record").0
}

fn syscalls(r: &NativeRecording) -> Vec<(&str, &SyscallPolicy)> {
    r.events
        .iter()
        .filter_map(|e| match &e.kind {
            NativeEventKind::Syscall { name, policy, .. } => Some((name.as_str(), policy)),
            _ => None,
        })
        .collect()
}

fn crashed(r: &NativeRecording) -> bool {
    r.events
        .iter()
        .any(|e| matches!(e.kind, NativeEventKind::Signal { signo: 11, .. }))
}

#[test]
fn records_the_nondeterminism() {
    let (_d, bin) = build();
    let r = rec(&bin);
    let calls = syscalls(&r);
    assert!(
        !r.vdso_patched.is_empty(),
        "vdso not patched: clock_gettime would be invisible"
    );
    assert!(
        calls.contains(&("getrandom", &SyscallPolicy::Emulated)),
        "{calls:?}"
    );
    assert!(
        calls.contains(&("clock_gettime", &SyscallPolicy::Emulated)),
        "{calls:?}"
    );
    assert!(
        calls.contains(&("read", &SyscallPolicy::Emulated)),
        "stdin read should be emulated: {calls:?}"
    );
    assert!(calls
        .iter()
        .any(|(n, p)| *n == "mmap" && **p == SyscallPolicy::Passthrough));
    assert_eq!(r.global_history.len(), r.events.len());
    assert!(r.globals.iter().any(|g| g.name == "requests_served"));
    assert_eq!(r.at_random.len(), 16);
}

#[test]
fn replay_reproduces_output_and_crash() {
    let (_d, bin) = build();
    // a handful of recordings: different random seeds, some crash, some don't
    for _ in 0..6 {
        let r = rec(&bin);
        let report = replay(&r, None).expect("replay");
        assert_eq!(report.events_replayed as usize, r.events.len());
        assert_eq!(report.stdout, r.stdout, "replayed output differs");
        assert!(report.emulated > 0);
    }
}

#[test]
fn seek_to_the_moment_of_the_crash() {
    let (_d, bin) = build();
    let r = (0..64)
        .map(|_| rec(&bin))
        .find(crashed)
        .expect("50% crash rate, 64 tries");
    let sig = r
        .events
        .iter()
        .position(|e| matches!(e.kind, NativeEventKind::Signal { .. }))
        .unwrap() as u64;
    let report = replay(&r, Some(sig)).unwrap();
    let state = report.state.expect("stopped at the fault");
    assert!(
        state.symbol.as_deref().unwrap_or("").starts_with("main"),
        "{:?}",
        state.symbol
    );
    // globals at the crash match what the recording saw
    let served = state
        .globals
        .iter()
        .find(|g| g.name == "requests_served")
        .unwrap();
    let recorded = &r.global_history[sig as usize];
    let idx = r
        .globals
        .iter()
        .position(|g| g.name == "requests_served")
        .unwrap();
    assert_eq!(served.int, decode_int(&recorded[idx].bytes));
}

#[test]
fn seek_to_the_middle() {
    let (_d, bin) = build();
    let r = rec(&bin);
    let mid = (r.events.len() / 2) as u64;
    let report = replay(&r, Some(mid)).unwrap();
    let state = report.state.unwrap();
    assert_eq!(state.index, mid);
    assert_eq!(
        state.regs.pc, r.events[mid as usize].regs.pc,
        "pc differs at event {mid}"
    );
}
