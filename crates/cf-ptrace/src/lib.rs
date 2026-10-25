//! Native record & replay for single-threaded Linux programs, rr-style.
//!
//! Recording runs the program under ptrace and logs every syscall. The ones
//! that bring nondeterminism in (reads from pipes/ttys/sockets, getrandom,
//! clocks, pids, sleeps) get their results and out-buffers saved. Replay
//! runs the program again and, for those syscalls, skips the kernel and
//! injects the saved results instead. Everything else (mmap, brk, file
//! reads, writes) runs for real.
//!
//! Things that would otherwise leak nondeterminism and how we pin them:
//!  * ASLR             -> `personality(ADDR_NO_RANDOMIZE)` before exec
//!  * vDSO clock calls -> patched to real `syscall` instructions so we see them
//!  * AT_RANDOM bytes  -> recorded, and written back at exec time on replay
//!  * environment      -> fixed, minimal envp in both runs
//!
//! Out of scope (documented, not hidden): threads, signals delivered
//! asynchronously, rdtsc, and shared memory with other processes.

use std::path::PathBuf;

pub use cf_trace::native::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub mod syscalls;

#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error("native recording needs Linux (this is {0}); recordings made on Linux can still be viewed here")]
    Unsupported(&'static str),
    #[error("{0}: {1}")]
    Os(&'static str, std::io::Error),
    #[error("replay diverged at event {index}: {detail}")]
    Divergence { index: u64, detail: String },
    #[error("{0}")]
    Other(String),
}

#[derive(Clone, Debug)]
pub struct RecordConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GlobalValue {
    pub name: String,
    pub addr: u64,
    pub hex: String,
    pub int: Option<i64>,
}

/// Process state at one point of a replay.
#[derive(Clone, Debug, serde::Serialize)]
pub struct NativeState {
    pub index: u64,
    pub regs: RegSnapshot,
    pub symbol: Option<String>,
    pub globals: Vec<GlobalValue>,
    pub stack: Vec<u8>,
    pub maps: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ReplayReport {
    pub events_replayed: u64,
    pub emulated: u64,
    pub stdout: Vec<u8>,
    pub state: Option<NativeState>,
}

#[cfg(target_os = "linux")]
pub use linux::{record, replay};

#[cfg(not(target_os = "linux"))]
pub fn record(_cfg: &RecordConfig) -> Result<(NativeRecording, Vec<u8>), NativeError> {
    Err(NativeError::Unsupported(std::env::consts::OS))
}

#[cfg(not(target_os = "linux"))]
pub fn replay(_rec: &NativeRecording, _stop_at: Option<u64>) -> Result<ReplayReport, NativeError> {
    Err(NativeError::Unsupported(std::env::consts::OS))
}

/// Decode a global's bytes as a little-endian integer when the size fits.
pub fn decode_int(bytes: &[u8]) -> Option<i64> {
    match bytes.len() {
        1 => Some(bytes[0] as i8 as i64),
        2 => Some(i16::from_le_bytes(bytes.try_into().ok()?) as i64),
        4 => Some(i32::from_le_bytes(bytes.try_into().ok()?) as i64),
        8 => Some(i64::from_le_bytes(bytes.try_into().ok()?)),
        _ => None,
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
