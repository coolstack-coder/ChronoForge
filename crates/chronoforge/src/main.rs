use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use cf_trace::{format, Outcome, Recording};
use cf_vm::{RecordOptions, Timeline};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "chronoforge",
    version,
    about = "Record once. Replay forever. Rewind anywhere."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a Forge program and record its nondeterminism
    Record {
        file: PathBuf,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        /// preemption probability per instruction
        #[arg(long, default_value_t = 0.08)]
        chaos: f64,
        /// values returned by input(), in order
        #[arg(long = "input")]
        inputs: Vec<String>,
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// also store the fully instrumented trace in the file
        #[arg(long)]
        full: bool,
    },
    /// Replay a recording and show the machine state at a step
    Replay {
        recording: PathBuf,
        /// show state right after this step (default: just before the failure)
        #[arg(long)]
        at: Option<u64>,
    },
    /// Record many chaos-scheduled runs to shake out interleaving bugs
    Hunt {
        file: PathBuf,
        #[arg(long, default_value_t = 300)]
        runs: u64,
        #[arg(long, default_value_t = 0.08)]
        chaos: f64,
        /// where to save the first failing and first passing recordings
        #[arg(long, default_value = "recordings")]
        save: PathBuf,
    },
    /// Races, deadlocks, slice and ranked root-cause hypotheses
    Analyze { recording: PathBuf },
    /// First divergence between two recordings
    Diff { a: PathBuf, b: PathBuf },
    /// Show compiled bytecode with instrumentation probes
    Disasm { file: PathBuf },
    /// Start the HTTP API (+ UI) and the gRPC service
    Serve {
        #[arg(long, default_value_t = 7420)]
        port: u16,
        #[arg(long, default_value_t = 7421)]
        grpc_port: u16,
        /// memory | file | rocksdb
        #[arg(long, default_value = "file")]
        store: String,
        #[arg(long, default_value = ".chronoforge")]
        data: PathBuf,
        #[arg(long, default_value = "ui/dist")]
        ui: PathBuf,
        #[arg(long, default_value = "examples/forge")]
        examples: PathBuf,
        #[arg(long, default_value = "examples/native/recordings")]
        native: PathBuf,
    },
    /// Native Linux programs via ptrace
    Native {
        #[command(subcommand)]
        cmd: NativeCmd,
    },
}

#[derive(Subcommand)]
enum NativeCmd {
    /// chronoforge native record -o run.json --stdin-text "GET /" -- ./server
    Record {
        #[arg(short, long, default_value = "native.json")]
        out: PathBuf,
        #[arg(long)]
        stdin: Option<PathBuf>,
        #[arg(long)]
        stdin_text: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Replay, optionally stopping at event N to inspect the process
    Replay {
        recording: PathBuf,
        #[arg(long)]
        at: Option<u64>,
    },
    /// List the recorded events
    Show { recording: PathBuf },
}

struct Paint(bool);

impl Paint {
    fn c(&self, code: &str, s: impl std::fmt::Display) -> String {
        if self.0 {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn dim(&self, s: impl std::fmt::Display) -> String {
        self.c("2", s)
    }
    fn bold(&self, s: impl std::fmt::Display) -> String {
        self.c("1", s)
    }
    fn red(&self, s: impl std::fmt::Display) -> String {
        self.c("38;5;203", s)
    }
    fn green(&self, s: impl std::fmt::Display) -> String {
        self.c("38;5;114", s)
    }
    fn amber(&self, s: impl std::fmt::Display) -> String {
        self.c("38;5;214", s)
    }
}

fn compile(path: &Path) -> Result<Arc<cf_lang::Program>> {
    let src =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let p = cf_lang::compile(&src).map_err(|e| anyhow::anyhow!("{}:{e}", path.display()))?;
    Ok(Arc::new(p))
}

fn load(path: &Path) -> Result<(Arc<cf_lang::Program>, Recording)> {
    let rec = format::TraceReader::open(path)
        .and_then(|mut r| r.recording())
        .with_context(|| format!("reading {}", path.display()))?;
    let p =
        cf_lang::compile(&rec.meta.source).map_err(|e| anyhow::anyhow!("embedded source: {e}"))?;
    Ok((Arc::new(p), rec))
}

fn outcome(p: &Paint, o: &Outcome) -> String {
    match o {
        Outcome::Completed => p.green("completed"),
        Outcome::StepLimit { limit } => p.amber(format!("stopped at step limit {limit}")),
        Outcome::Fault(f) => p.red(format!(
            "{} at line {} (T{}, step {}): {}",
            f.kind.label(),
            f.line,
            f.tid,
            f.step,
            f.message
        )),
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "program".into())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let p = Paint(std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none());
    match cli.cmd {
        Cmd::Record {
            file,
            seed,
            chaos,
            inputs,
            out,
            full,
        } => {
            let program = compile(&file)?;
            let opts = RecordOptions {
                name: stem(&file),
                seed,
                chaos,
                inputs,
                ..Default::default()
            };
            let t = std::time::Instant::now();
            let (rec, steps) = if full {
                let (r, s) = cf_vm::record_traced(program, &opts)?;
                (r, Some(s))
            } else {
                (cf_vm::record(program, &opts)?, None)
            };
            let took = t.elapsed();
            for line in &rec.output {
                println!("{} {}", p.dim(format!("T{}│", line.tid)), line.text);
            }
            let out =
                out.unwrap_or_else(|| PathBuf::from(format!("{}-seed{seed}.cfrec", stem(&file))));
            let bytes = format::write_recording(&out, &rec, steps.as_deref())?;
            println!();
            println!("{} {}", p.bold("outcome "), outcome(&p, &rec.outcome));
            println!(
                "{} {} steps in {:.1?} · {} nondeterministic events · {} bytes → {}",
                p.bold("recorded"),
                rec.total_steps,
                took,
                rec.nondet.len(),
                bytes,
                out.display()
            );
        }
        Cmd::Replay { recording, at } => {
            let (program, rec) = load(&recording)?;
            let rec = Arc::new(rec);
            let t = std::time::Instant::now();
            let tl = Timeline::build(
                program.clone(),
                rec.clone(),
                cf_vm::DEFAULT_INTERVAL,
                &mut cf_vm::Quiet,
            )?;
            println!(
                "{} {} steps in {:.1?}, outcome matches the recording ({})",
                p.green("replayed"),
                rec.total_steps,
                t.elapsed(),
                outcome(&p, &rec.outcome)
            );
            let step = at.unwrap_or_else(|| {
                rec.outcome
                    .fault()
                    .map(|f| f.step.saturating_sub(1))
                    .unwrap_or(rec.total_steps)
            });
            let m = tl.seek(step + 1)?;
            print_state(&p, &m, &program);
        }
        Cmd::Hunt {
            file,
            runs,
            chaos,
            save,
        } => {
            let program = compile(&file)?;
            let mut first_fail: Option<Recording> = None;
            let mut first_pass: Option<Recording> = None;
            let mut sigs: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
            let t = std::time::Instant::now();
            let mut strip = String::new();
            for seed in 0..runs {
                let rec = cf_vm::record(
                    program.clone(),
                    &RecordOptions {
                        name: stem(&file),
                        seed,
                        chaos,
                        max_steps: 500_000,
                        ..Default::default()
                    },
                )?;
                let sig = match rec.outcome.fault() {
                    Some(f) => format!("{} @ line {}", f.kind.label(), f.line),
                    None if rec.outcome.is_failure() => "step limit".into(),
                    None => "pass".into(),
                };
                let e = sigs.entry(sig).or_insert((0, seed));
                e.0 += 1;
                strip.push_str(&if rec.outcome.is_failure() {
                    p.red("█")
                } else {
                    p.green("▁")
                });
                if rec.outcome.is_failure() && first_fail.is_none() {
                    first_fail = Some(rec);
                } else if !rec.outcome.is_failure() && first_pass.is_none() {
                    first_pass = Some(rec);
                }
            }
            println!("{strip}");
            println!("{} {runs} runs in {:.1?}", p.bold("hunted"), t.elapsed());
            for (sig, (n, seed)) in &sigs {
                let pct = *n as f64 * 100.0 / runs as f64;
                let label = if sig == "pass" {
                    p.green(sig)
                } else {
                    p.red(sig)
                };
                println!("  {label:<40} {n:>5} runs ({pct:>5.1}%)  first: seed {seed}");
            }
            std::fs::create_dir_all(&save)?;
            for (kind, rec) in [("fail", first_fail), ("pass", first_pass)] {
                if let Some(r) = rec {
                    let path =
                        save.join(format!("{}-{kind}-seed{}.cfrec", stem(&file), r.meta.seed));
                    format::write_recording(&path, &r, None)?;
                    println!("  saved {kind} → {}", path.display());
                }
            }
        }
        Cmd::Analyze { recording } => {
            let (program, rec) = load(&recording)?;
            let mut steps = Vec::new();
            cf_vm::replay(program.clone(), &rec, &mut steps)?;
            let r = cf_analysis::analyze(&program, &rec, &steps);
            println!("{} {}", p.bold("outcome"), outcome(&p, &rec.outcome));
            println!(
                "{} {} steps · {} threads · {} shared accesses · {} sync events",
                p.dim("trace  "),
                r.stats.steps,
                r.stats.threads,
                r.stats.shared_accesses,
                r.stats.sync_events
            );
            if !r.races.is_empty() {
                println!("\n{}", p.bold("data races"));
                for race in &r.races {
                    println!(
                        "  {} {:<12} T{} line {} ({:?}) ⟷ T{} line {} ({:?})",
                        p.amber("⚡"),
                        race.name,
                        race.first.tid,
                        race.first.line,
                        race.first.kind,
                        race.second.tid,
                        race.second.line,
                        race.second.kind
                    );
                }
            }
            for c in &r.locks.cycles {
                println!("\n{} {}", p.bold("lock-order cycle"), c.description);
            }
            println!("\n{}", p.bold("hypotheses"));
            for (i, h) in r.hypotheses.iter().enumerate() {
                println!(
                    "  {}. {} {}",
                    i + 1,
                    p.bold(&h.title),
                    p.dim(format!("{:.0}%", h.confidence * 100.0))
                );
                println!("     {}", h.explanation);
                for e in &h.evidence {
                    println!(
                        "     {} step {:<6} T{} line {:<3} {}",
                        p.dim("·"),
                        e.step,
                        e.tid,
                        e.line,
                        p.dim(format!("{} — {}", e.role, e.detail))
                    );
                }
                if let Some(fix) = &h.fix {
                    println!("     {} {fix}", p.green("fix:"));
                }
            }
        }
        Cmd::Diff { a, b } => {
            let (pa, ra) = load(&a)?;
            let (pb, rb) = load(&b)?;
            let (mut sa, mut sb) = (Vec::new(), Vec::new());
            cf_vm::replay(pa.clone(), &ra, &mut sa)?;
            cf_vm::replay(pb.clone(), &rb, &mut sb)?;
            let d = cf_analysis::first_divergence(
                cf_analysis::Side {
                    program: &pa,
                    recording: &ra,
                    steps: &sa,
                },
                cf_analysis::Side {
                    program: &pb,
                    recording: &rb,
                    steps: &sb,
                },
            );
            println!(
                "{} A: {}   B: {}",
                p.bold("outcomes"),
                d.outcome_a,
                d.outcome_b
            );
            println!(
                "{} {} ({} identical steps)",
                p.bold("diverge "),
                p.amber(&d.kind),
                d.common_prefix
            );
            println!("{}", d.explanation);
        }
        Cmd::Disasm { file } => {
            let program = compile(&file)?;
            print!("{}", program.disassemble());
            let s = &program.plan.stats;
            println!(
                "\n{} instructions · {} access probes · {} shared · {} sync · {} nondet",
                s.instructions, s.access_probes, s.shared_probes, s.sync_probes, s.nondet_probes
            );
        }
        Cmd::Serve {
            port,
            grpc_port,
            store,
            data,
            ui,
            examples,
            native,
        } => {
            tracing_subscriber::fmt().with_env_filter("info").init();
            let store: Arc<dyn cf_trace::store::TraceStore> = match store.as_str() {
                "memory" => Arc::new(cf_trace::store::MemoryStore::default()),
                "file" => Arc::new(cf_trace::store::FileStore::open(data.join("sessions"))?),
                #[cfg(feature = "rocksdb")]
                "rocksdb" => Arc::new(cf_trace::store::RocksStore::open(data.join("rocksdb"))?),
                #[cfg(not(feature = "rocksdb"))]
                "rocksdb" => {
                    bail!("this build has no RocksDB support; rebuild with --features rocksdb")
                }
                other => bail!("unknown store `{other}` (memory | file | rocksdb)"),
            };
            let state = Arc::new(cf_server::AppState::new(store, examples, native));
            let cfg = cf_server::Config {
                http: ([127, 0, 0, 1], port).into(),
                grpc: Some(([127, 0, 0, 1], grpc_port).into()),
                ui_dir: Some(ui),
                state,
            };
            println!(
                "{}  http://127.0.0.1:{port}   (gRPC on :{grpc_port})",
                p.amber("◷ ChronoForge")
            );
            tokio::runtime::Runtime::new()?.block_on(cf_server::serve(cfg))?;
        }
        Cmd::Native { cmd } => native(&p, cmd)?,
    }
    Ok(())
}

fn print_state(p: &Paint, m: &cf_vm::Machine, program: &cf_lang::Program) {
    let snap = m.snapshot();
    let src: Vec<&str> = program.source.lines().collect();
    println!("\n{} position {}", p.bold("state at"), snap.position);
    for t in &snap.threads {
        let marker = if t.tid == snap.current {
            p.amber("▶")
        } else {
            " ".into()
        };
        let code = src
            .get(t.line.saturating_sub(1) as usize)
            .map(|s| s.trim())
            .unwrap_or("");
        println!(
            "{marker} T{} {:<14} {:<22} line {:<3} {}",
            t.tid,
            t.name,
            p.dim(&t.state),
            t.line,
            p.dim(code)
        );
        for f in t.frames.iter().take(1) {
            for v in &f.locals {
                println!("      {:<10} = {}", v.name, v.value);
            }
        }
    }
    println!("{}", p.bold("globals"));
    for g in &snap.globals {
        let w = g
            .writer
            .map(|w| format!("written at step {w}"))
            .unwrap_or_default();
        println!("  {:<12} = {:<16} {}", g.name, g.value, p.dim(w));
    }
}

fn native(p: &Paint, cmd: NativeCmd) -> Result<()> {
    match cmd {
        NativeCmd::Record {
            out,
            stdin,
            stdin_text,
            command,
        } => {
            let stdin = match (stdin, stdin_text) {
                (Some(f), _) => std::fs::read(f)?,
                (None, Some(t)) => t.replace("\\n", "\n").into_bytes(),
                _ => Vec::new(),
            };
            let cfg = cf_ptrace::RecordConfig {
                program: command[0].clone().into(),
                args: command[1..].to_vec(),
                stdin,
            };
            let (rec, stdout) = cf_ptrace::record(&cfg)?;
            print!("{}", String::from_utf8_lossy(&stdout));
            std::fs::write(&out, serde_json::to_vec_pretty(&rec)?)?;
            let emulated = rec
                .events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.kind,
                        cf_ptrace::NativeEventKind::Syscall {
                            policy: cf_ptrace::SyscallPolicy::Emulated,
                            ..
                        }
                    )
                })
                .count();
            println!(
                "{} {} events ({} emulated on replay), vdso patched: {} → {}",
                p.bold("recorded"),
                rec.events.len(),
                emulated,
                rec.vdso_patched.join(", "),
                out.display()
            );
        }
        NativeCmd::Replay { recording, at } => {
            let rec: cf_ptrace::NativeRecording =
                serde_json::from_slice(&std::fs::read(&recording)?)?;
            let report = cf_ptrace::replay(&rec, at)?;
            print!("{}", String::from_utf8_lossy(&report.stdout));
            println!(
                "{} {} events, {} syscalls emulated, output identical: {}",
                p.green("replayed"),
                report.events_replayed,
                report.emulated,
                report.stdout == rec.stdout || at.is_some()
            );
            if let Some(s) = report.state {
                println!(
                    "\n{} event {} in {}",
                    p.bold("stopped at"),
                    s.index,
                    s.symbol.unwrap_or_default()
                );
                println!("  pc {:#x}  sp {:#x}", s.regs.pc, s.regs.sp);
                for g in s.globals {
                    println!(
                        "  {:<18} = {:<20} {}",
                        g.name,
                        g.int.map(|i| i.to_string()).unwrap_or_default(),
                        p.dim(g.hex)
                    );
                }
            }
        }
        NativeCmd::Show { recording } => {
            let rec: cf_ptrace::NativeRecording =
                serde_json::from_slice(&std::fs::read(&recording)?)?;
            println!(
                "{} {} ({}, kernel {})",
                p.bold(&rec.program),
                rec.args.join(" "),
                rec.arch,
                rec.kernel
            );
            for e in &rec.events {
                let line = match &e.kind {
                    cf_ptrace::NativeEventKind::Syscall {
                        name,
                        detail,
                        policy,
                        ..
                    } => {
                        let tag = if *policy == cf_ptrace::SyscallPolicy::Emulated {
                            p.amber("emu ")
                        } else {
                            p.dim("pass")
                        };
                        format!("{tag} {name:<16} {detail}")
                    }
                    cf_ptrace::NativeEventKind::Signal {
                        name, fault_addr, ..
                    } => p.red(format!("SIGNAL {name} at {:#x}", fault_addr.unwrap_or(0))),
                    cf_ptrace::NativeEventKind::Exit { code } => p.bold(format!("exit {code}")),
                };
                println!(
                    "{:>4}  {line}  {}",
                    e.index,
                    p.dim(e.symbol.as_deref().unwrap_or(""))
                );
            }
        }
    }
    Ok(())
}
