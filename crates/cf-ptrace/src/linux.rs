use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::{mem, ptr, thread};

use libc::{c_long, c_void, pid_t};
use object::{Object, ObjectKind, ObjectSection, ObjectSegment, ObjectSymbol, SymbolKind};

use crate::syscalls::{self, OutLen};
use crate::*;

const PTRACE_GET_SYSCALL_INFO: c_long = 0x420e;
const OP_ENTRY: u8 = 1;
const OP_EXIT: u8 = 2;
const AT_RANDOM: u64 = 25;
const ADDR_NO_RANDOMIZE: libc::c_ulong = 0x0040000;

/// Same environment for record and replay, so the initial stack layout
/// (and every pointer derived from it) is identical.
const FIXED_ENV: &[&str] = &[
    "PATH=/usr/local/bin:/usr/bin:/bin",
    "LANG=C",
    "HOME=/tmp",
    "CHRONOFORGE=1",
];

#[repr(C)]
#[derive(Default)]
struct SyscallInfo {
    op: u8,
    _pad: [u8; 3],
    arch: u32,
    ip: u64,
    sp: u64,
    // entry: nr, args[6]; exit: rval, is_error
    data: [u64; 7],
}

#[derive(Clone, Copy)]
pub(crate) enum Stub {
    Syscall(u32),
    Enosys,
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use super::*;

    pub const NAME: &str = "x86_64";
    pub type Regs = libc::user_regs_struct;

    pub const VDSO: &[(&str, Stub)] = &[
        ("__vdso_clock_gettime", Stub::Syscall(228)),
        ("__vdso_gettimeofday", Stub::Syscall(96)),
        ("__vdso_time", Stub::Syscall(201)),
        ("__vdso_getcpu", Stub::Syscall(309)),
        ("__vdso_clock_getres", Stub::Syscall(229)),
        // vgetrandom has a different ABI; make it fail so glibc falls back to the syscall
        ("__vdso_getrandom", Stub::Enosys),
    ];

    pub fn stub(s: Stub) -> Vec<u8> {
        match s {
            // mov eax, nr ; syscall ; ret
            Stub::Syscall(nr) => {
                let mut v = vec![0xb8];
                v.extend_from_slice(&nr.to_le_bytes());
                v.extend_from_slice(&[0x0f, 0x05, 0xc3]);
                v
            }
            // mov rax, -ENOSYS ; ret
            Stub::Enosys => vec![0x48, 0xc7, 0xc0, 0xda, 0xff, 0xff, 0xff, 0xc3],
        }
    }

    pub fn get(pid: pid_t) -> io::Result<Regs> {
        let mut r: Regs = unsafe { mem::zeroed() };
        check(unsafe {
            libc::ptrace(
                libc::PTRACE_GETREGS,
                pid,
                ptr::null_mut::<c_void>(),
                &mut r as *mut _ as *mut c_void,
            )
        })?;
        Ok(r)
    }

    pub fn set(pid: pid_t, r: &Regs) -> io::Result<()> {
        check(unsafe {
            libc::ptrace(
                libc::PTRACE_SETREGS,
                pid,
                ptr::null_mut::<c_void>(),
                r as *const _ as *mut c_void,
            )
        })
        .map(|_| ())
    }

    pub fn skip_syscall(pid: pid_t) -> io::Result<()> {
        let mut r = get(pid)?;
        r.orig_rax = u64::MAX;
        set(pid, &r)
    }

    pub fn set_return(pid: pid_t, v: i64) -> io::Result<()> {
        let mut r = get(pid)?;
        r.rax = v as u64;
        set(pid, &r)
    }

    pub fn snapshot(r: &Regs) -> RegSnapshot {
        let regs = [
            ("rax", r.rax),
            ("rbx", r.rbx),
            ("rcx", r.rcx),
            ("rdx", r.rdx),
            ("rsi", r.rsi),
            ("rdi", r.rdi),
            ("rbp", r.rbp),
            ("rsp", r.rsp),
            ("r8", r.r8),
            ("r9", r.r9),
            ("r10", r.r10),
            ("r11", r.r11),
            ("r12", r.r12),
            ("r13", r.r13),
            ("r14", r.r14),
            ("r15", r.r15),
            ("rip", r.rip),
            ("eflags", r.eflags),
        ];
        RegSnapshot {
            pc: r.rip,
            sp: r.rsp,
            regs: regs.iter().map(|(n, v)| (n.to_string(), *v)).collect(),
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::*;

    pub const NAME: &str = "aarch64";
    const NT_PRSTATUS: usize = 1;
    const NT_ARM_SYSTEM_CALL: usize = 0x404;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Regs {
        pub regs: [u64; 31],
        pub sp: u64,
        pub pc: u64,
        pub pstate: u64,
    }

    pub const VDSO: &[(&str, Stub)] = &[
        ("__kernel_clock_gettime", Stub::Syscall(113)),
        ("__kernel_gettimeofday", Stub::Syscall(169)),
        ("__kernel_clock_getres", Stub::Syscall(114)),
        ("__kernel_getrandom", Stub::Enosys),
    ];

    pub fn stub(s: Stub) -> Vec<u8> {
        let words: Vec<u32> = match s {
            // mov x8, #nr ; svc #0 ; ret
            Stub::Syscall(nr) => vec![0xD280_0008 | (nr << 5), 0xD400_0001, 0xD65F_03C0],
            // movn x0, #37 (= -38, -ENOSYS) ; ret
            Stub::Enosys => vec![0x9280_0000 | (37 << 5), 0xD65F_03C0],
        };
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn regset(
        req: libc::c_uint,
        pid: pid_t,
        kind: usize,
        base: *mut c_void,
        len: usize,
    ) -> io::Result<()> {
        let mut iov = libc::iovec {
            iov_base: base,
            iov_len: len,
        };
        check(unsafe {
            libc::ptrace(
                req,
                pid,
                kind as *mut c_void,
                &mut iov as *mut _ as *mut c_void,
            )
        })
        .map(|_| ())
    }

    pub fn get(pid: pid_t) -> io::Result<Regs> {
        let mut r: Regs = unsafe { mem::zeroed() };
        regset(
            libc::PTRACE_GETREGSET,
            pid,
            NT_PRSTATUS,
            &mut r as *mut _ as *mut c_void,
            mem::size_of::<Regs>(),
        )?;
        Ok(r)
    }

    pub fn set(pid: pid_t, r: &Regs) -> io::Result<()> {
        let mut copy = *r;
        regset(
            libc::PTRACE_SETREGSET,
            pid,
            NT_PRSTATUS,
            &mut copy as *mut _ as *mut c_void,
            mem::size_of::<Regs>(),
        )
    }

    pub fn skip_syscall(pid: pid_t) -> io::Result<()> {
        let mut nr: i32 = -1;
        regset(
            libc::PTRACE_SETREGSET,
            pid,
            NT_ARM_SYSTEM_CALL,
            &mut nr as *mut _ as *mut c_void,
            4,
        )
    }

    pub fn set_return(pid: pid_t, v: i64) -> io::Result<()> {
        let mut r = get(pid)?;
        r.regs[0] = v as u64;
        set(pid, &r)
    }

    pub fn snapshot(r: &Regs) -> RegSnapshot {
        let mut regs: Vec<(String, u64)> = (0..31).map(|i| (format!("x{i}"), r.regs[i])).collect();
        regs.push(("sp".into(), r.sp));
        regs.push(("pc".into(), r.pc));
        regs.push(("pstate".into(), r.pstate));
        RegSnapshot {
            pc: r.pc,
            sp: r.sp,
            regs,
        }
    }
}

fn check(ret: c_long) -> io::Result<c_long> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn os(what: &'static str) -> impl FnOnce(io::Error) -> NativeError {
    move |e| NativeError::Os(what, e)
}

enum Stop {
    Syscall,
    Signal(i32),
    Exited(i32),
    Killed(i32),
}

struct Tracee {
    pid: pid_t,
    mem: File,
    alive: bool,
}

impl Drop for Tracee {
    fn drop(&mut self) {
        if self.alive {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
                let mut st = 0;
                libc::waitpid(self.pid, &mut st, libc::__WALL);
            }
        }
    }
}

impl Tracee {
    fn read(&self, addr: u64, len: u64) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len as usize];
        let n = self.mem.read_at(&mut buf, addr)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn write(&self, addr: u64, bytes: &[u8]) -> io::Result<()> {
        self.mem.write_all_at(bytes, addr)
    }

    fn read_cstr(&self, addr: u64) -> String {
        let bytes = self.read(addr, 256).unwrap_or_default();
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    fn resume(&self, sig: i32) -> io::Result<()> {
        check(unsafe {
            libc::ptrace(
                libc::PTRACE_SYSCALL,
                self.pid,
                ptr::null_mut::<c_void>(),
                sig as usize as *mut c_void,
            )
        })
        .map(|_| ())
    }

    fn wait(&mut self) -> io::Result<Stop> {
        let mut status = 0;
        if unsafe { libc::waitpid(self.pid, &mut status, libc::__WALL) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::WIFEXITED(status) {
            self.alive = false;
            return Ok(Stop::Exited(libc::WEXITSTATUS(status)));
        }
        if libc::WIFSIGNALED(status) {
            self.alive = false;
            return Ok(Stop::Killed(libc::WTERMSIG(status)));
        }
        let sig = libc::WSTOPSIG(status);
        Ok(if sig == libc::SIGTRAP | 0x80 {
            Stop::Syscall
        } else {
            Stop::Signal(sig)
        })
    }

    fn syscall_info(&self) -> io::Result<SyscallInfo> {
        let mut info = SyscallInfo::default();
        check(unsafe {
            libc::ptrace(
                PTRACE_GET_SYSCALL_INFO as _,
                self.pid,
                mem::size_of::<SyscallInfo>() as *mut c_void,
                &mut info as *mut _ as *mut c_void,
            )
        })?;
        Ok(info)
    }

    fn fault_addr(&self) -> Option<u64> {
        let mut si: libc::siginfo_t = unsafe { mem::zeroed() };
        let ok = unsafe {
            libc::ptrace(
                libc::PTRACE_GETSIGINFO,
                self.pid,
                ptr::null_mut::<c_void>(),
                &mut si as *mut _ as *mut c_void,
            )
        };
        (ok >= 0).then(|| unsafe { si.si_addr() } as u64)
    }

    fn maps(&self) -> Vec<Mapping> {
        std::fs::read_to_string(format!("/proc/{}/maps", self.pid))
            .unwrap_or_default()
            .lines()
            .filter_map(Mapping::parse)
            .collect()
    }

    fn fd_is_nondeterministic(&self, fd: u64) -> bool {
        match std::fs::read_link(format!("/proc/{}/fd/{fd}", self.pid)) {
            Ok(target) => {
                let t = target.to_string_lossy();
                t.starts_with("pipe:")
                    || t.starts_with("socket:")
                    || t.starts_with("/dev/")
                    || t.starts_with("anon_inode:")
            }
            Err(_) => false,
        }
    }
}

#[derive(Clone, Debug)]
struct Mapping {
    start: u64,
    end: u64,
    offset: u64,
    path: String,
    raw: String,
}

impl Mapping {
    fn parse(line: &str) -> Option<Mapping> {
        let mut parts = line.split_whitespace();
        let range = parts.next()?;
        let _perms = parts.next()?;
        let offset = u64::from_str_radix(parts.next()?, 16).ok()?;
        let _dev = parts.next()?;
        let _inode = parts.next()?;
        let path = parts.collect::<Vec<_>>().join(" ");
        let (s, e) = range.split_once('-')?;
        Some(Mapping {
            start: u64::from_str_radix(s, 16).ok()?,
            end: u64::from_str_radix(e, 16).ok()?,
            offset,
            path,
            raw: line.to_string(),
        })
    }
}

/// What we learn from the executable on disk.
struct Image {
    load_base: u64,
    globals: Vec<NativeGlobal>,
    funcs: Vec<(u64, u64, String)>,
}

impl Image {
    fn load(path: &Path, maps: &[Mapping]) -> Result<Image, NativeError> {
        let data = std::fs::read(path).map_err(os("reading executable"))?;
        let file = object::File::parse(&*data)
            .map_err(|e| NativeError::Other(format!("parsing ELF: {e}")))?;
        let path_str = path.to_string_lossy();
        let load_base = if file.kind() == ObjectKind::Dynamic {
            maps.iter()
                .filter(|m| m.path == path_str)
                .map(|m| m.start)
                .min()
                .unwrap_or(0)
        } else {
            0
        };
        let mut globals = Vec::new();
        let mut funcs = Vec::new();
        for sym in file.symbols() {
            let Ok(name) = sym.name() else { continue };
            if name.is_empty() || !sym.is_definition() {
                continue;
            }
            let addr = load_base + sym.address();
            match sym.kind() {
                SymbolKind::Text if sym.size() > 0 => {
                    funcs.push((addr, sym.size(), name.to_string()))
                }
                SymbolKind::Data
                    if sym.size() > 0 && !name.starts_with('_') && !name.contains('.') =>
                {
                    let section = sym
                        .section_index()
                        .and_then(|i| file.section_by_index(i).ok());
                    let in_data = section
                        .and_then(|s| s.name().ok().map(|n| n == ".data" || n == ".bss"))
                        .unwrap_or(false);
                    if in_data {
                        globals.push(NativeGlobal {
                            name: name.to_string(),
                            addr,
                            size: sym.size().min(256),
                        });
                    }
                }
                _ => {}
            }
        }
        globals.sort_by_key(|g| g.addr);
        funcs.sort();
        Ok(Image {
            load_base,
            globals,
            funcs,
        })
    }

    fn symbolize(&self, pc: u64, maps: &[Mapping]) -> Option<String> {
        if let Some((start, _, name)) = self
            .funcs
            .iter()
            .rev()
            .find(|(s, len, _)| pc >= *s && pc < s + len)
        {
            return Some(format!("{name}+{:#x}", pc - start));
        }
        let m = maps.iter().find(|m| pc >= m.start && pc < m.end)?;
        let file = m.path.rsplit('/').next().unwrap_or(&m.path);
        let file = if file.is_empty() { "anon" } else { file };
        Some(format!("{file}+{:#x}", pc - m.start + m.offset))
    }

    fn snapshot_globals(&self, t: &Tracee) -> Vec<MemBlob> {
        self.globals
            .iter()
            .map(|g| MemBlob {
                addr: g.addr,
                bytes: t.read(g.addr, g.size).unwrap_or_default(),
            })
            .collect()
    }
}

fn pipe() -> Result<(RawFd, RawFd), NativeError> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(NativeError::Os("pipe", io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

/// Read a pipe to EOF on a helper thread so a chatty tracee can't fill the
/// pipe and block while we're sitting in waitpid.
fn drain(fd: RawFd) -> thread::JoinHandle<Vec<u8>> {
    let mut f = unsafe { File::from_raw_fd(fd) };
    thread::spawn(move || {
        let mut out = Vec::new();
        let _ = f.read_to_end(&mut out);
        out
    })
}

fn spawn(
    program: &Path,
    args: &[String],
    stdin: RawFd,
    stdout: RawFd,
) -> Result<Tracee, NativeError> {
    // everything the child touches must exist before fork: no allocation after
    let path = CString::new(program.as_os_str().as_encoded_bytes())
        .map_err(|e| NativeError::Other(e.to_string()))?;
    let mut argv_c = vec![path.clone()];
    for a in args {
        argv_c.push(CString::new(a.as_bytes()).map_err(|e| NativeError::Other(e.to_string()))?);
    }
    let env_c: Vec<CString> = FIXED_ENV
        .iter()
        .map(|e| CString::new(*e).unwrap())
        .collect();
    let mut argv: Vec<*const libc::c_char> = argv_c.iter().map(|c| c.as_ptr()).collect();
    argv.push(ptr::null());
    let mut envp: Vec<*const libc::c_char> = env_c.iter().map(|c| c.as_ptr()).collect();
    envp.push(ptr::null());

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(NativeError::Os("fork", io::Error::last_os_error()));
    }
    if pid == 0 {
        unsafe {
            libc::dup2(stdin, 0);
            libc::dup2(stdout, 1);
            libc::ptrace(
                libc::PTRACE_TRACEME,
                0,
                ptr::null_mut::<c_void>(),
                ptr::null_mut::<c_void>(),
            );
            libc::personality(ADDR_NO_RANDOMIZE);
            libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
            libc::_exit(127);
        }
    }

    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, libc::__WALL) };
    if !libc::WIFSTOPPED(status) || libc::WSTOPSIG(status) != libc::SIGTRAP {
        return Err(NativeError::Other(format!(
            "could not exec {}",
            program.display()
        )));
    }
    let opts = libc::PTRACE_O_TRACESYSGOOD | libc::PTRACE_O_EXITKILL;
    check(unsafe {
        libc::ptrace(
            libc::PTRACE_SETOPTIONS,
            pid,
            ptr::null_mut::<c_void>(),
            opts as usize as *mut c_void,
        )
    })
    .map_err(os("PTRACE_SETOPTIONS"))?;
    let mem = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/proc/{pid}/mem"))
        .map_err(os("opening /proc/pid/mem"))?;
    Ok(Tracee {
        pid,
        mem,
        alive: true,
    })
}

/// rr's trick: the vDSO answers clock_gettime without entering the kernel,
/// so ptrace never sees it. Overwrite those functions with stubs that do a
/// real syscall, which we *can* see (and replay).
fn patch_vdso(t: &Tracee) -> Result<Vec<String>, NativeError> {
    let maps = t.maps();
    let Some(vdso) = maps.iter().find(|m| m.path == "[vdso]") else {
        return Ok(vec![]);
    };
    let image = t
        .read(vdso.start, vdso.end - vdso.start)
        .map_err(os("reading vdso"))?;
    let file = object::File::parse(&*image)
        .map_err(|e| NativeError::Other(format!("parsing vdso: {e}")))?;
    let base = file.segments().map(|s| s.address()).min().unwrap_or(0);
    let mut patched = Vec::new();
    let mut done = std::collections::BTreeSet::new();
    for sym in file.dynamic_symbols() {
        let Ok(name) = sym.name() else { continue };
        let Some((_, stub)) = arch::VDSO.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        let addr = vdso.start + sym.address() - base;
        if done.insert(addr) {
            t.write(addr, &arch::stub(*stub))
                .map_err(os("patching vdso"))?;
            patched.push(name.to_string());
        }
    }
    Ok(patched)
}

fn at_random_addr(pid: pid_t) -> Option<u64> {
    let auxv = std::fs::read(format!("/proc/{pid}/auxv")).ok()?;
    // auxv is a flat array of (key, value) u64 pairs
    auxv.as_chunks::<16>().0.iter().find_map(|c| {
        let k = u64::from_ne_bytes(c[..8].try_into().unwrap());
        (k == AT_RANDOM).then(|| u64::from_ne_bytes(c[8..].try_into().unwrap()))
    })
}

fn preview(bytes: &[u8]) -> String {
    let s: String = String::from_utf8_lossy(&bytes[..bytes.len().min(40)])
        .escape_debug()
        .collect();
    if bytes.len() > 40 {
        format!("\"{s}…\"")
    } else {
        format!("\"{s}\"")
    }
}

fn detail(
    name: &str,
    args: &[u64; 6],
    ret: i64,
    outputs: &[MemBlob],
    entry_preview: &str,
) -> String {
    let err = if ret < 0 {
        format!(" → -{}", -ret)
    } else {
        String::new()
    };
    match name {
        "read" | "pread64" | "recvfrom" => match outputs.first() {
            Some(o) => format!("fd={} {}", args[0], preview(&o.bytes)),
            None => format!("fd={} → {ret}", args[0]),
        },
        "write" | "pwrite64" => format!("fd={} {}", args[0], entry_preview),
        "openat" => format!(
            "{}{}",
            entry_preview,
            if ret >= 0 {
                format!(" → fd {ret}")
            } else {
                err
            }
        ),
        "clock_gettime" => match outputs.first().filter(|o| o.bytes.len() >= 16) {
            Some(o) => {
                let sec = i64::from_le_bytes(o.bytes[..8].try_into().unwrap());
                let nsec = i64::from_le_bytes(o.bytes[8..16].try_into().unwrap());
                format!("clock {} → {sec}.{nsec:09}s", args[0])
            }
            None => format!("clock {}{err}", args[0]),
        },
        "getrandom" => match outputs.first() {
            Some(o) => format!(
                "{} bytes: {}",
                o.bytes.len(),
                hex(&o.bytes[..o.bytes.len().min(8)])
            ),
            None => format!("→ {ret}"),
        },
        "mmap" | "brk" => format!("→ {:#x}", ret as u64),
        "exit_group" | "exit" => format!("code {}", args[0] as i32),
        _ => format!("→ {ret}"),
    }
}

fn is_fault(sig: i32) -> bool {
    matches!(
        sig,
        libc::SIGSEGV
            | libc::SIGBUS
            | libc::SIGFPE
            | libc::SIGILL
            | libc::SIGABRT
            | libc::SIGSYS
            | libc::SIGTRAP
    )
}

fn signame(sig: i32) -> String {
    match sig {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGTRAP => "SIGTRAP",
        _ => return format!("signal {sig}"),
    }
    .into()
}

struct Pending {
    nr: u64,
    args: [u64; 6],
    name: String,
    emulate: bool,
    outputs: &'static [(usize, OutLen)],
    entry_preview: String,
}

fn kernel() -> String {
    let mut u: libc::utsname = unsafe { mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return String::new();
    }
    let r = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) };
    r.to_string_lossy().into_owned()
}

pub fn record(cfg: &RecordConfig) -> Result<(NativeRecording, Vec<u8>), NativeError> {
    let program = std::fs::canonicalize(&cfg.program).map_err(os("resolving program path"))?;
    let (in_r, in_w) = pipe()?;
    let (out_r, out_w) = pipe()?;
    let stdin_data = cfg.stdin.clone();
    let feeder = thread::spawn(move || {
        let mut f = unsafe { File::from_raw_fd(in_w) };
        let _ = f.write_all(&stdin_data);
    });

    let mut t = spawn(&program, &cfg.args, in_r, out_w)?;
    unsafe {
        libc::close(in_r);
        libc::close(out_w);
    }
    let reader = drain(out_r);

    let at_random = at_random_addr(t.pid)
        .and_then(|a| t.read(a, 16).ok())
        .unwrap_or_default();
    let vdso_patched = patch_vdso(&t)?;
    let image = Image::load(&program, &t.maps())?;

    let mut events: Vec<NativeEvent> = Vec::new();
    let mut history: Vec<Vec<MemBlob>> = Vec::new();
    let mut pending: Option<Pending> = None;
    let mut sig = 0;

    loop {
        t.resume(sig).map_err(os("PTRACE_SYSCALL"))?;
        sig = 0;
        match t.wait().map_err(os("waitpid"))? {
            Stop::Syscall => {
                let info = t.syscall_info().map_err(os("PTRACE_GET_SYSCALL_INFO"))?;
                match info.op {
                    OP_ENTRY => {
                        let nr = info.data[0];
                        let args: [u64; 6] = info.data[1..7].try_into().unwrap();
                        let name = syscalls::name(nr);
                        let rule = syscalls::rule(&name, |fd| t.fd_is_nondeterministic(fd), &args);
                        let entry_preview = match name.as_str() {
                            "write" | "pwrite64" => {
                                preview(&t.read(args[1], args[2].min(64)).unwrap_or_default())
                            }
                            "openat" => format!("{:?}", t.read_cstr(args[1])),
                            _ => String::new(),
                        };
                        pending = Some(Pending {
                            nr,
                            args,
                            name,
                            emulate: rule.emulate,
                            outputs: rule.outputs,
                            entry_preview,
                        });
                    }
                    OP_EXIT => {
                        let Some(p) = pending.take() else { continue };
                        let ret = info.data[0] as i64;
                        let outputs = if p.emulate && ret >= 0 {
                            p.outputs
                                .iter()
                                .filter(|(arg, _)| p.args[*arg] != 0)
                                .map(|(arg, len)| {
                                    let n = match len {
                                        OutLen::Fixed(n) => *n,
                                        OutLen::Ret => ret as u64,
                                    };
                                    MemBlob {
                                        addr: p.args[*arg],
                                        bytes: t.read(p.args[*arg], n).unwrap_or_default(),
                                    }
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                        let regs = arch::get(t.pid)
                            .map(|r| arch::snapshot(&r))
                            .unwrap_or_default();
                        let maps = t.maps();
                        events.push(NativeEvent {
                            index: events.len() as u64,
                            symbol: image.symbolize(regs.pc, &maps),
                            regs,
                            kind: NativeEventKind::Syscall {
                                nr: p.nr,
                                detail: detail(&p.name, &p.args, ret, &outputs, &p.entry_preview),
                                name: p.name,
                                args: p.args,
                                ret,
                                policy: if p.emulate {
                                    SyscallPolicy::Emulated
                                } else {
                                    SyscallPolicy::Passthrough
                                },
                                outputs,
                            },
                        });
                        history.push(image.snapshot_globals(&t));
                    }
                    _ => {}
                }
            }
            Stop::Signal(s) => {
                if is_fault(s) {
                    let regs = arch::get(t.pid)
                        .map(|r| arch::snapshot(&r))
                        .unwrap_or_default();
                    let maps = t.maps();
                    events.push(NativeEvent {
                        index: events.len() as u64,
                        symbol: image.symbolize(regs.pc, &maps),
                        regs,
                        kind: NativeEventKind::Signal {
                            signo: s,
                            name: signame(s),
                            fault_addr: t.fault_addr(),
                        },
                    });
                    history.push(image.snapshot_globals(&t));
                }
                sig = s;
            }
            stop @ (Stop::Exited(_) | Stop::Killed(_)) => {
                // exit_group never returns, so its entry is still pending
                if let Some(p) = pending.take() {
                    events.push(NativeEvent {
                        index: events.len() as u64,
                        regs: RegSnapshot::default(),
                        symbol: None,
                        kind: NativeEventKind::Syscall {
                            nr: p.nr,
                            detail: format!("code {}", p.args[0] as i32),
                            name: p.name,
                            args: p.args,
                            ret: 0,
                            policy: SyscallPolicy::Passthrough,
                            outputs: vec![],
                        },
                    });
                    history.push(history.last().cloned().unwrap_or_default());
                }
                // shell convention: killed by signal N exits with 128 + N
                let code = match stop {
                    Stop::Killed(sig) => 128 + sig,
                    Stop::Exited(code) => code,
                    _ => unreachable!(),
                };
                events.push(NativeEvent {
                    index: events.len() as u64,
                    regs: RegSnapshot::default(),
                    symbol: None,
                    kind: NativeEventKind::Exit { code },
                });
                history.push(history.last().cloned().unwrap_or_default());
                break;
            }
        }
    }

    let _ = feeder.join();
    let stdout = reader.join().unwrap_or_default();
    let rec = NativeRecording {
        program: program.to_string_lossy().into_owned(),
        args: cfg.args.clone(),
        arch: arch::NAME.into(),
        kernel: kernel(),
        load_base: image.load_base,
        stdin: cfg.stdin.clone(),
        globals: image.globals.clone(),
        vdso_patched,
        at_random,
        env: FIXED_ENV.iter().map(|s| s.to_string()).collect(),
        stdout: stdout.clone(),
        events,
        global_history: history,
    };
    Ok((rec, stdout))
}

fn capture(t: &Tracee, image: &Image, index: u64) -> NativeState {
    let regs = arch::get(t.pid)
        .map(|r| arch::snapshot(&r))
        .unwrap_or_default();
    let maps = t.maps();
    let globals = image
        .globals
        .iter()
        .map(|g| {
            let bytes = t.read(g.addr, g.size).unwrap_or_default();
            GlobalValue {
                name: g.name.clone(),
                addr: g.addr,
                hex: hex(&bytes),
                int: decode_int(&bytes),
            }
        })
        .collect();
    NativeState {
        index,
        symbol: image.symbolize(regs.pc, &maps),
        stack: t.read(regs.sp, 256).unwrap_or_default(),
        maps: maps.iter().map(|m| m.raw.clone()).collect(),
        regs,
        globals,
    }
}

/// Re-execute a recording. With `stop_at = Some(n)` we halt right after
/// event `n` (or *at* the fault, for a signal event) and capture the state:
/// that's how "go back to just before the crash" works natively.
pub fn replay(rec: &NativeRecording, stop_at: Option<u64>) -> Result<ReplayReport, NativeError> {
    let program = PathBuf::from(&rec.program);
    // stdin must be the same *kind* of file as when recording: glibc probes
    // a char device like /dev/null with an extra isatty() ioctl, which would
    // be a divergence. Reads are emulated anyway, so an empty pipe will do.
    let (in_r, in_w) = pipe()?;
    unsafe { libc::close(in_w) };
    let (out_r, out_w) = pipe()?;
    let mut t = spawn(&program, &rec.args, in_r, out_w)?;
    unsafe {
        libc::close(in_r);
        libc::close(out_w);
    }
    let reader = drain(out_r);

    if let (Some(addr), 16) = (at_random_addr(t.pid), rec.at_random.len()) {
        t.write(addr, &rec.at_random)
            .map_err(os("restoring AT_RANDOM"))?;
    }
    patch_vdso(&t)?;
    let mut image = Image::load(&program, &t.maps())?;
    image.globals = rec.globals.clone();

    let mut cursor: usize = 0;
    let mut emulated = 0;
    let mut sig = 0;
    let mut in_flight: Option<usize> = None;
    let mut state = None;

    let diverge = |index: usize, detail: String| NativeError::Divergence {
        index: index as u64,
        detail,
    };

    loop {
        t.resume(sig).map_err(os("PTRACE_SYSCALL"))?;
        sig = 0;
        match t.wait().map_err(os("waitpid"))? {
            Stop::Syscall => {
                let info = t.syscall_info().map_err(os("PTRACE_GET_SYSCALL_INFO"))?;
                match info.op {
                    OP_ENTRY => {
                        let nr = info.data[0];
                        let Some(ev) = rec.events.get(cursor) else {
                            return Err(diverge(
                                cursor,
                                format!("program made extra syscall {}", syscalls::name(nr)),
                            ));
                        };
                        match &ev.kind {
                            NativeEventKind::Syscall {
                                nr: want, policy, ..
                            } if *want == nr => {
                                if *policy == SyscallPolicy::Emulated {
                                    arch::skip_syscall(t.pid).map_err(os("skipping syscall"))?;
                                }
                                in_flight = Some(cursor);
                            }
                            other => {
                                return Err(diverge(
                                    cursor,
                                    format!("got {}, recording has {other:?}", syscalls::name(nr)),
                                ))
                            }
                        }
                    }
                    OP_EXIT => {
                        let Some(i) = in_flight.take() else { continue };
                        if let NativeEventKind::Syscall {
                            policy: SyscallPolicy::Emulated,
                            ret,
                            outputs,
                            ..
                        } = &rec.events[i].kind
                        {
                            arch::set_return(t.pid, *ret).map_err(os("setting return value"))?;
                            for o in outputs {
                                t.write(o.addr, &o.bytes)
                                    .map_err(os("injecting syscall output"))?;
                            }
                            emulated += 1;
                        }
                        cursor = i + 1;
                        if stop_at == Some(i as u64) {
                            state = Some(capture(&t, &image, i as u64));
                            break;
                        }
                    }
                    _ => {}
                }
            }
            Stop::Signal(s) => {
                if is_fault(s) {
                    match rec.events.get(cursor).map(|e| &e.kind) {
                        Some(NativeEventKind::Signal { signo, .. }) if *signo == s => {}
                        other => {
                            return Err(diverge(
                                cursor,
                                format!("got {}, recording has {other:?}", signame(s)),
                            ))
                        }
                    }
                    if stop_at == Some(cursor as u64) {
                        state = Some(capture(&t, &image, cursor as u64));
                        break;
                    }
                    cursor += 1;
                }
                sig = s;
            }
            Stop::Exited(_) | Stop::Killed(_) => {
                // pending exit_group + the exit event itself
                cursor = rec.events.len();
                break;
            }
        }
    }

    drop(t); // kills the tracee if we stopped early, closing its stdout
    let stdout = reader.join().unwrap_or_default();
    Ok(ReplayReport {
        events_replayed: cursor as u64,
        emulated,
        stdout,
        state,
    })
}
