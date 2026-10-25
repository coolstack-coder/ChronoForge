//! Syscall numbers we care about, per architecture, plus the record/replay
//! policy for each.

#[cfg(target_arch = "x86_64")]
const TABLE: &[(u64, &str)] = &[
    (0, "read"),
    (1, "write"),
    (2, "open"),
    (3, "close"),
    (4, "stat"),
    (5, "fstat"),
    (6, "lstat"),
    (7, "poll"),
    (8, "lseek"),
    (9, "mmap"),
    (10, "mprotect"),
    (11, "munmap"),
    (12, "brk"),
    (13, "rt_sigaction"),
    (14, "rt_sigprocmask"),
    (16, "ioctl"),
    (17, "pread64"),
    (18, "pwrite64"),
    (19, "readv"),
    (20, "writev"),
    (21, "access"),
    (24, "sched_yield"),
    (28, "madvise"),
    (35, "nanosleep"),
    (39, "getpid"),
    (45, "recvfrom"),
    (59, "execve"),
    (60, "exit"),
    (62, "kill"),
    (63, "uname"),
    (72, "fcntl"),
    (79, "getcwd"),
    (89, "readlink"),
    (96, "gettimeofday"),
    (99, "sysinfo"),
    (102, "getuid"),
    (110, "getppid"),
    (158, "arch_prctl"),
    (186, "gettid"),
    (201, "time"),
    (202, "futex"),
    (218, "set_tid_address"),
    (228, "clock_gettime"),
    (229, "clock_getres"),
    (230, "clock_nanosleep"),
    (231, "exit_group"),
    (234, "tgkill"),
    (257, "openat"),
    (262, "newfstatat"),
    (273, "set_robust_list"),
    (302, "prlimit64"),
    (309, "getcpu"),
    (318, "getrandom"),
    (332, "statx"),
    (334, "rseq"),
];

#[cfg(target_arch = "aarch64")]
const TABLE: &[(u64, &str)] = &[
    (17, "getcwd"),
    (25, "fcntl"),
    (29, "ioctl"),
    (48, "faccessat"),
    (56, "openat"),
    (57, "close"),
    (62, "lseek"),
    (63, "read"),
    (64, "write"),
    (65, "readv"),
    (66, "writev"),
    (67, "pread64"),
    (68, "pwrite64"),
    (73, "ppoll"),
    (78, "readlinkat"),
    (79, "newfstatat"),
    (80, "fstat"),
    (93, "exit"),
    (94, "exit_group"),
    (96, "set_tid_address"),
    (98, "futex"),
    (99, "set_robust_list"),
    (101, "nanosleep"),
    (113, "clock_gettime"),
    (114, "clock_getres"),
    (115, "clock_nanosleep"),
    (124, "sched_yield"),
    (129, "kill"),
    (131, "tgkill"),
    (134, "rt_sigaction"),
    (135, "rt_sigprocmask"),
    (160, "uname"),
    (163, "getrlimit"),
    (167, "prctl"),
    (168, "getcpu"),
    (169, "gettimeofday"),
    (172, "getpid"),
    (173, "getppid"),
    (174, "getuid"),
    (178, "gettid"),
    (179, "sysinfo"),
    (207, "recvfrom"),
    (214, "brk"),
    (215, "munmap"),
    (221, "execve"),
    (222, "mmap"),
    (226, "mprotect"),
    (233, "madvise"),
    (261, "prlimit64"),
    (278, "getrandom"),
    (291, "statx"),
    (293, "rseq"),
];

pub fn name(nr: u64) -> String {
    TABLE
        .iter()
        .find(|(n, _)| *n == nr)
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| format!("syscall_{nr}"))
}

/// An out-buffer the kernel fills: (pointer arg index, length).
#[derive(Clone, Copy, Debug)]
pub enum OutLen {
    Fixed(u64),
    /// as many bytes as the syscall returned
    Ret,
}

pub struct Rule {
    pub emulate: bool,
    pub outputs: &'static [(usize, OutLen)],
}

const PASS: Rule = Rule {
    emulate: false,
    outputs: &[],
};

/// `nondet_fd` answers "is this fd a pipe/tty/socket/device?" — reading
/// those is input from the outside world, reading a regular file is not.
pub fn rule(name: &str, nondet_fd: impl Fn(u64) -> bool, args: &[u64; 6]) -> Rule {
    match name {
        "getrandom" => Rule {
            emulate: true,
            outputs: &[(0, OutLen::Ret)],
        },
        "clock_gettime" | "clock_getres" => Rule {
            emulate: true,
            outputs: &[(1, OutLen::Fixed(16))],
        },
        "gettimeofday" => Rule {
            emulate: true,
            outputs: &[(0, OutLen::Fixed(16))],
        },
        "time" => Rule {
            emulate: true,
            outputs: &[(0, OutLen::Fixed(8))],
        },
        "sysinfo" => Rule {
            emulate: true,
            outputs: &[(0, OutLen::Fixed(112))],
        },
        "getcpu" => Rule {
            emulate: true,
            outputs: &[(0, OutLen::Fixed(4)), (1, OutLen::Fixed(4))],
        },
        // replay doesn't need to actually wait
        "nanosleep" | "clock_nanosleep" | "getpid" | "getppid" | "gettid" => Rule {
            emulate: true,
            outputs: &[],
        },
        "recvfrom" => Rule {
            emulate: true,
            outputs: &[(1, OutLen::Ret)],
        },
        "read" | "pread64" if nondet_fd(args[0]) => Rule {
            emulate: true,
            outputs: &[(1, OutLen::Ret)],
        },
        _ => PASS,
    }
}
