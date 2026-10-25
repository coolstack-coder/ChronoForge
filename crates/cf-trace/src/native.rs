//! Types for native (ptrace) recordings. They live here rather than in
//! cf-ptrace so a macOS server can still open and browse a recording that
//! was captured on a Linux box.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemBlob {
    pub addr: u64,
    pub bytes: Vec<u8>,
}

/// General purpose registers, kept as (name, value) so x86_64 and aarch64
/// share one shape.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegSnapshot {
    pub pc: u64,
    pub sp: u64,
    pub regs: Vec<(String, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyscallPolicy {
    /// Executed for real during replay (mmap, brk, write to a tty...).
    Passthrough,
    /// Skipped during replay; result and out-buffers injected from the log.
    Emulated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeEventKind {
    Syscall {
        nr: u64,
        name: String,
        args: [u64; 6],
        ret: i64,
        policy: SyscallPolicy,
        /// Memory the kernel wrote into the tracee (read buffers, timespecs).
        outputs: Vec<MemBlob>,
        /// Short human summary, e.g. `fd=0 "hello\n"` for the timeline.
        detail: String,
    },
    Signal {
        signo: i32,
        name: String,
        fault_addr: Option<u64>,
    },
    Exit {
        code: i32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeEvent {
    pub index: u64,
    pub kind: NativeEventKind,
    pub regs: RegSnapshot,
    /// Symbol containing the pc, when we could resolve one.
    pub symbol: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeGlobal {
    pub name: String,
    pub addr: u64,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRecording {
    pub program: String,
    pub args: Vec<String>,
    pub arch: String,
    pub kernel: String,
    pub load_base: u64,
    pub stdin: Vec<u8>,
    /// Data/BSS symbols from the ELF, so we can show "variables" by name.
    pub globals: Vec<NativeGlobal>,
    pub vdso_patched: Vec<String>,
    /// The 16 random bytes the kernel hands every process (stack canary,
    /// pointer guard). Written back at exec time on replay.
    #[serde(default)]
    pub at_random: Vec<u8>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub stdout: Vec<u8>,
    pub events: Vec<NativeEvent>,
    /// Snapshots of each global's bytes at every event, filled by replay.
    #[serde(default)]
    pub global_history: Vec<Vec<MemBlob>>,
}
