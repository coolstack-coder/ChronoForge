//! The interpreter.
//!
//! Two rules keep it deterministic:
//!  1. every source of nondeterminism (who runs next, rand, clock, input)
//!     goes through the [`Oracle`], and
//!  2. nothing else in here looks at the outside world or iterates a hash map.
//!
//! Break either and replay silently diverges, so the replay oracle checks
//! every decision against the log and errors out loudly instead.

use std::sync::Arc;

use cf_lang::bytecode::{BinOp, Builtin, Instr, UnOp};
use cf_lang::{Probe, Program};
use cf_trace::*;

use crate::VmError;

pub const MAX_FRAMES: usize = 256;

/// Where nondeterminism comes from: the chaos recorder or a replay log.
pub trait Oracle {
    fn schedule(&mut self, m: &Machine, runnable: &[Tid]) -> Result<Tid, VmError>;
    fn rand(&mut self, step: Step, bound: i64) -> Result<i64, VmError>;
    fn clock(&mut self, step: Step) -> Result<i64, VmError>;
    fn input(&mut self, step: Step) -> Result<String, VmError>;
}

/// Receives instrumented step events. Seeking uses a quiet observer so that
/// re-execution doesn't allocate events nobody reads.
pub trait Observer {
    fn enabled(&self) -> bool;
    fn on_step(&mut self, ev: StepEvent);
}

pub struct Quiet;

impl Observer for Quiet {
    fn enabled(&self) -> bool {
        false
    }
    fn on_step(&mut self, _: StepEvent) {}
}

impl Observer for Vec<StepEvent> {
    fn enabled(&self) -> bool {
        true
    }
    fn on_step(&mut self, ev: StepEvent) {
        self.push(ev);
    }
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub func: u32,
    pub pc: u32,
    pub id: u64,
    pub locals: Vec<Val>,
    /// Shadow memory: the step that last wrote each local.
    pub writers: Vec<Option<Step>>,
    /// Most recent branch executed in this frame (or the call that created
    /// it) — cheap dynamic control dependence.
    pub last_branch: Option<Step>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum ThreadState {
    Runnable,
    BlockedLock(u32),
    BlockedJoin(Tid),
    Done,
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub val: Val,
    /// Step that computed this operand — the edge of the data-flow graph.
    pub producer: Option<Step>,
}

#[derive(Clone, Debug)]
pub struct Thread {
    pub tid: Tid,
    pub name: String,
    pub frames: Vec<Frame>,
    pub stack: Vec<Slot>,
    pub state: ThreadState,
    pub result: Val,
    pub spawned_at: Step,
    pub finished_at: Option<Step>,
}

#[derive(Clone, Debug)]
pub struct MutexState {
    pub owner: Option<Tid>,
    pub created_at: Step,
}

#[derive(Clone, Debug)]
pub struct Machine {
    pub program: Arc<Program>,
    pub globals: Vec<Val>,
    pub global_writers: Vec<Option<Step>>,
    pub heap: Vec<Vec<Val>>,
    pub heap_writers: Vec<Vec<Option<Step>>>,
    pub threads: Vec<Thread>,
    pub mutexes: Vec<MutexState>,
    pub current: Tid,
    /// Number of steps executed so far (= index of the next step).
    pub step: Step,
    /// Set by `yield()`, read by the recorder to prefer another thread.
    pub yielded: bool,
    pub output: Vec<OutputLine>,
    pub outcome: Option<Outcome>,
    next_frame: u64,
}

/// Collects instrumentation for one step; a no-op when tracing is off.
struct Rec {
    on: bool,
    reads: Vec<Read>,
    writes: Vec<cf_trace::Write>,
    deps: Vec<Step>,
    sync: Option<SyncOp>,
    nondet: Option<NondetKind>,
    result: Option<Val>,
    probe: Probe,
}

impl Rec {
    fn read(&mut self, loc: Loc, val: &Val, writer: Option<Step>) {
        if let Some(w) = writer {
            self.dep(Some(w));
        }
        if self.on && self.probe.has(Probe::ACCESS) {
            self.reads.push(Read {
                loc,
                val: val.clone(),
                writer,
            });
        }
    }

    fn write(&mut self, loc: Loc, old: &Val, new: &Val) {
        if self.on && self.probe.has(Probe::ACCESS) {
            self.writes.push(cf_trace::Write {
                loc,
                old: old.clone(),
                new: new.clone(),
            });
        }
    }

    fn dep(&mut self, step: Option<Step>) {
        if let (true, Some(s)) = (self.on, step) {
            if !self.deps.contains(&s) {
                self.deps.push(s);
            }
        }
    }

    fn result(&mut self, v: &Val) {
        if self.on {
            self.result = Some(v.clone());
        }
    }
}

enum Flow {
    Next,
    Jump(u32),
    /// Call/Return already set up the right frame and pc.
    Moved,
    /// Don't advance the pc: the instruction will be retried (blocked lock/join).
    Stay,
    Fault(FaultKind, String),
}

impl Machine {
    pub fn new(program: Arc<Program>) -> Self {
        let main = program.func(program.main);
        let frame = Frame {
            func: program.main,
            pc: 0,
            id: 0,
            locals: vec![Val::Nil; main.nlocals as usize],
            writers: vec![None; main.nlocals as usize],
            last_branch: None,
        };
        let nglobals = program.globals.len();
        Machine {
            globals: vec![Val::Nil; nglobals],
            global_writers: vec![None; nglobals],
            heap: Vec::new(),
            heap_writers: Vec::new(),
            threads: vec![Thread {
                tid: 0,
                name: "main".into(),
                frames: vec![frame],
                stack: Vec::new(),
                state: ThreadState::Runnable,
                result: Val::Nil,
                spawned_at: 0,
                finished_at: None,
            }],
            mutexes: Vec::new(),
            current: 0,
            step: 0,
            yielded: false,
            output: Vec::new(),
            outcome: None,
            next_frame: 1,
            program,
        }
    }

    pub fn runnable(&self) -> Vec<Tid> {
        self.threads
            .iter()
            .filter(|t| t.state == ThreadState::Runnable)
            .map(|t| t.tid)
            .collect()
    }

    /// Line the given thread is about to execute.
    pub fn line_of(&self, tid: Tid) -> u32 {
        let t = &self.threads[tid as usize];
        match t.frames.last() {
            Some(f) => {
                let func = self.program.func(f.func);
                let pc = (f.pc as usize).min(func.lines.len().saturating_sub(1));
                func.lines.get(pc).copied().unwrap_or(func.line)
            }
            None => 0,
        }
    }

    pub fn run(
        &mut self,
        oracle: &mut dyn Oracle,
        obs: &mut dyn Observer,
        max_steps: Step,
    ) -> Result<Outcome, VmError> {
        loop {
            if let Some(o) = &self.outcome {
                return Ok(o.clone());
            }
            if self.step >= max_steps {
                let o = Outcome::StepLimit { limit: max_steps };
                self.outcome = Some(o.clone());
                return Ok(o);
            }
            self.step_once(oracle, obs)?;
        }
    }

    /// Execute exactly one instruction (or settle the outcome if nothing can run).
    pub fn step_once(
        &mut self,
        oracle: &mut dyn Oracle,
        obs: &mut dyn Observer,
    ) -> Result<(), VmError> {
        if self.outcome.is_some() {
            return Ok(());
        }
        let runnable = self.runnable();
        if runnable.is_empty() {
            self.outcome = Some(self.settle());
            return Ok(());
        }

        let tid = oracle.schedule(self, &runnable)?;
        if self.threads.get(tid as usize).map(|t| t.state) != Some(ThreadState::Runnable) {
            return Err(VmError::Divergence {
                step: self.step,
                detail: format!("scheduler picked T{tid}, which cannot run"),
            });
        }
        self.current = tid;
        self.yielded = false;

        let (func, pc, ctrl) = {
            let f = self.threads[tid as usize]
                .frames
                .last()
                .expect("runnable thread without a frame");
            (f.func, f.pc, f.last_branch)
        };
        let program = Arc::clone(&self.program);
        let function = program.func(func);
        let ins = &function.code[pc as usize];
        let line = function.lines[pc as usize];
        let step = self.step;
        let mut rec = Rec {
            on: obs.enabled(),
            reads: Vec::new(),
            writes: Vec::new(),
            deps: Vec::new(),
            sync: None,
            nondet: None,
            result: None,
            probe: program.plan.probe(func, pc),
        };

        let flow = self.exec(ins, oracle, &mut rec);

        // the thread may have finished (frames popped), so look it up again
        let thread = &mut self.threads[tid as usize];
        if ins.tag() == OpTag::Branch {
            if let Some(f) = thread.frames.last_mut() {
                f.last_branch = Some(step);
            }
        }
        match &flow {
            Flow::Next => {
                if let Some(f) = thread.frames.last_mut() {
                    f.pc += 1;
                }
            }
            Flow::Jump(target) => {
                if let Some(f) = thread.frames.last_mut() {
                    f.pc = *target;
                }
            }
            Flow::Moved | Flow::Stay | Flow::Fault(..) => {}
        }

        if rec.on {
            obs.on_step(StepEvent {
                step,
                tid,
                func,
                pc,
                line,
                op: ins.tag(),
                reads: rec.reads,
                writes: rec.writes,
                deps: rec.deps,
                ctrl,
                sync: rec.sync,
                nondet: rec.nondet,
                result: rec.result,
            });
        }
        self.step += 1;

        if let Flow::Fault(kind, message) = flow {
            self.outcome = Some(Outcome::Fault(Fault {
                kind,
                message,
                step,
                tid,
                line,
            }));
        }
        Ok(())
    }

    /// Nothing is runnable: either everyone finished, or we're stuck.
    fn settle(&self) -> Outcome {
        if self.threads.iter().all(|t| t.state == ThreadState::Done) {
            return Outcome::Completed;
        }
        let cycle = self.wait_for_cycle();
        let message = if cycle.is_empty() {
            // blocked on a lock whose owner already exited, typically
            let stuck: Vec<String> = self
                .threads
                .iter()
                .filter_map(|t| match t.state {
                    ThreadState::BlockedLock(m) => {
                        let owner = self.mutexes[m as usize].owner;
                        Some(format!(
                            "T{} waits on mutex#{m} held by T{}",
                            t.tid,
                            owner.unwrap_or(u32::MAX)
                        ))
                    }
                    ThreadState::BlockedJoin(j) => Some(format!("T{} waits to join T{j}", t.tid)),
                    _ => None,
                })
                .collect();
            format!("all threads blocked: {}", stuck.join(", "))
        } else {
            let parts: Vec<String> = cycle
                .iter()
                .map(|(t, m)| {
                    if *m == u32::MAX {
                        format!("T{t} joins")
                    } else {
                        format!("T{t} waits on mutex#{m}")
                    }
                })
                .collect();
            format!("wait-for cycle: {}", parts.join(" → "))
        };
        let last = self.step.saturating_sub(1);
        let tid = cycle.first().map(|c| c.0).unwrap_or(self.current);
        Outcome::Fault(Fault {
            kind: FaultKind::Deadlock { cycle },
            message,
            step: last,
            tid,
            line: self.line_of(tid),
        })
    }

    /// Follow "T waits on mutex held by T'" edges until something repeats.
    pub fn wait_for_cycle(&self) -> Vec<(Tid, u32)> {
        let next = |t: Tid| -> Option<(u32, Tid)> {
            match self.threads[t as usize].state {
                ThreadState::BlockedLock(m) => self.mutexes[m as usize].owner.map(|o| (m, o)),
                ThreadState::BlockedJoin(j) => Some((u32::MAX, j)),
                _ => None,
            }
        };
        for start in 0..self.threads.len() as Tid {
            let mut path: Vec<(Tid, u32)> = Vec::new();
            let mut cur = start;
            while let Some((m, owner)) = next(cur) {
                if let Some(pos) = path.iter().position(|(t, _)| *t == cur) {
                    return path[pos..].to_vec();
                }
                path.push((cur, m));
                cur = owner;
                if path.len() > self.threads.len() + 1 {
                    break;
                }
            }
            if let Some(pos) = path.iter().position(|(t, _)| *t == cur) {
                return path[pos..].to_vec();
            }
        }
        Vec::new()
    }

    fn pop(&mut self) -> Slot {
        self.threads[self.current as usize]
            .stack
            .pop()
            .expect("operand stack underflow (compiler bug)")
    }

    fn push(&mut self, val: Val, producer: Option<Step>) {
        self.threads[self.current as usize]
            .stack
            .push(Slot { val, producer });
    }

    fn peek(&self) -> &Slot {
        self.threads[self.current as usize]
            .stack
            .last()
            .expect("operand stack underflow (compiler bug)")
    }

    fn frame(&mut self) -> &mut Frame {
        self.threads[self.current as usize]
            .frames
            .last_mut()
            .unwrap()
    }

    fn pop_args(&mut self, argc: u32, rec: &mut Rec) -> Vec<Slot> {
        let mut args: Vec<Slot> = (0..argc).map(|_| self.pop()).collect();
        args.reverse();
        for a in &args {
            rec.dep(a.producer);
        }
        args
    }

    fn new_frame(&mut self, func: u32, args: Vec<Slot>, rec: &mut Rec) -> Frame {
        let f = self.program.func(func);
        let id = self.next_frame;
        self.next_frame += 1;
        let mut locals = vec![Val::Nil; f.nlocals as usize];
        let mut writers = vec![None; f.nlocals as usize];
        for (slot, a) in args.into_iter().enumerate() {
            rec.write(
                Loc::Local {
                    frame: id,
                    slot: slot as u32,
                },
                &Val::Nil,
                &a.val,
            );
            locals[slot] = a.val;
            writers[slot] = Some(self.step);
        }
        Frame {
            func,
            pc: 0,
            id,
            locals,
            writers,
            last_branch: Some(self.step),
        }
    }

    /// On an out-of-bounds access the array's length matters as much as the
    /// index, so depend on whoever last grew it.
    fn length_dep(&self, arr: &Val, rec: &mut Rec) {
        if let Val::Array(obj) = arr {
            if let Some(w) = self
                .heap_writers
                .get(*obj as usize)
                .and_then(|w| w.iter().flatten().max())
            {
                rec.dep(Some(*w));
            }
        }
    }

    fn array_index(&self, arr: &Val, idx: &Val) -> Result<(u32, u32), Flow> {
        let Val::Array(obj) = arr else {
            return Err(Flow::Fault(
                FaultKind::TypeError,
                format!("cannot index into {}", arr.type_name()),
            ));
        };
        let Val::Int(i) = idx else {
            return Err(Flow::Fault(
                FaultKind::TypeError,
                format!("array index must be int, got {}", idx.type_name()),
            ));
        };
        let len = self.heap[*obj as usize].len();
        if *i < 0 || *i as usize >= len {
            return Err(Flow::Fault(
                FaultKind::IndexOutOfBounds,
                format!("index {i} out of bounds for array#{obj} of length {len}"),
            ));
        }
        Ok((*obj, *i as u32))
    }

    fn exec(&mut self, ins: &Instr, oracle: &mut dyn Oracle, rec: &mut Rec) -> Flow {
        let step = self.step;
        let tid = self.current;
        match ins {
            Instr::Push(v) => {
                rec.result(v);
                self.push(v.clone(), Some(step));
            }
            Instr::Pop => {
                let s = self.pop();
                rec.dep(s.producer);
            }
            Instr::LoadLocal(slot) => {
                let f = self.frame();
                let (val, writer, id) = (
                    f.locals[*slot as usize].clone(),
                    f.writers[*slot as usize],
                    f.id,
                );
                rec.read(
                    Loc::Local {
                        frame: id,
                        slot: *slot,
                    },
                    &val,
                    writer,
                );
                rec.result(&val);
                self.push(val, Some(step));
            }
            Instr::StoreLocal(slot) => {
                let s = self.pop();
                rec.dep(s.producer);
                let f = self.frame();
                let id = f.id;
                let old = std::mem::replace(&mut f.locals[*slot as usize], s.val.clone());
                f.writers[*slot as usize] = Some(step);
                rec.write(
                    Loc::Local {
                        frame: id,
                        slot: *slot,
                    },
                    &old,
                    &s.val,
                );
            }
            Instr::LoadGlobal(g) => {
                let val = self.globals[*g as usize].clone();
                rec.read(Loc::Global(*g), &val, self.global_writers[*g as usize]);
                rec.result(&val);
                self.push(val, Some(step));
            }
            Instr::StoreGlobal(g) => {
                let s = self.pop();
                rec.dep(s.producer);
                let old = std::mem::replace(&mut self.globals[*g as usize], s.val.clone());
                self.global_writers[*g as usize] = Some(step);
                rec.write(Loc::Global(*g), &old, &s.val);
            }
            Instr::Binary(op) => {
                let b = self.pop();
                let a = self.pop();
                rec.dep(a.producer);
                rec.dep(b.producer);
                match binary(*op, &a.val, &b.val) {
                    Ok(v) => {
                        rec.result(&v);
                        self.push(v, Some(step));
                    }
                    Err((kind, msg)) => return Flow::Fault(kind, msg),
                }
            }
            Instr::Unary(op) => {
                let a = self.pop();
                rec.dep(a.producer);
                let v = match (op, &a.val) {
                    (UnOp::Neg, Val::Int(i)) => Val::Int(i.wrapping_neg()),
                    (UnOp::Not, v) => Val::Bool(!v.truthy()),
                    (UnOp::Neg, v) => {
                        return Flow::Fault(
                            FaultKind::TypeError,
                            format!("cannot negate {}", v.type_name()),
                        )
                    }
                };
                rec.result(&v);
                self.push(v, Some(step));
            }
            Instr::Jump(t) => return Flow::Jump(*t),
            Instr::JumpIfFalse(t) => {
                let c = self.pop();
                rec.dep(c.producer);
                rec.result(&Val::Bool(c.val.truthy()));
                if !c.val.truthy() {
                    return Flow::Jump(*t);
                }
            }
            Instr::JumpIfFalseKeep(t) | Instr::JumpIfTrueKeep(t) => {
                let want_true = matches!(ins, Instr::JumpIfTrueKeep(_));
                let c = self.peek().clone();
                rec.dep(c.producer);
                if c.val.truthy() == want_true {
                    return Flow::Jump(*t);
                }
                self.pop();
            }
            Instr::MakeArray(n) => {
                let items = self.pop_args(*n, rec);
                let obj = self.heap.len() as u32;
                for (i, it) in items.iter().enumerate() {
                    rec.write(Loc::Heap { obj, idx: i as u32 }, &Val::Nil, &it.val);
                }
                self.heap_writers.push(vec![Some(step); items.len()]);
                self.heap.push(items.into_iter().map(|s| s.val).collect());
                rec.result(&Val::Array(obj));
                self.push(Val::Array(obj), Some(step));
            }
            Instr::Index => {
                let idx = self.pop();
                let arr = self.pop();
                rec.dep(arr.producer);
                rec.dep(idx.producer);
                let (obj, i) = match self.array_index(&arr.val, &idx.val) {
                    Ok(x) => x,
                    Err(f) => {
                        self.length_dep(&arr.val, rec);
                        return f;
                    }
                };
                let val = self.heap[obj as usize][i as usize].clone();
                rec.read(
                    Loc::Heap { obj, idx: i },
                    &val,
                    self.heap_writers[obj as usize][i as usize],
                );
                rec.result(&val);
                self.push(val, Some(step));
            }
            Instr::StoreIndex => {
                let val = self.pop();
                let idx = self.pop();
                let arr = self.pop();
                rec.dep(val.producer);
                rec.dep(idx.producer);
                rec.dep(arr.producer);
                let (obj, i) = match self.array_index(&arr.val, &idx.val) {
                    Ok(x) => x,
                    Err(f) => {
                        self.length_dep(&arr.val, rec);
                        return f;
                    }
                };
                let old =
                    std::mem::replace(&mut self.heap[obj as usize][i as usize], val.val.clone());
                self.heap_writers[obj as usize][i as usize] = Some(step);
                rec.write(Loc::Heap { obj, idx: i }, &old, &val.val);
            }
            Instr::Call { func, argc } => {
                if self.threads[tid as usize].frames.len() >= MAX_FRAMES {
                    return Flow::Fault(
                        FaultKind::Panic,
                        format!("stack overflow ({MAX_FRAMES} frames)"),
                    );
                }
                let args = self.pop_args(*argc, rec);
                self.frame().pc += 1; // return address
                let frame = self.new_frame(*func, args, rec);
                self.threads[tid as usize].frames.push(frame);
                return Flow::Moved;
            }
            Instr::Return => {
                let ret = self.pop();
                rec.dep(ret.producer);
                rec.result(&ret.val);
                let t = &mut self.threads[tid as usize];
                t.frames.pop();
                if t.frames.is_empty() {
                    t.result = ret.val;
                    self.finish_thread(tid, rec);
                } else {
                    // the return value is attributed to this step, which depends on the callee's value
                    t.stack.push(Slot {
                        val: ret.val,
                        producer: Some(step),
                    });
                }
                return Flow::Moved;
            }
            Instr::Spawn { func, argc } => {
                let args = self.pop_args(*argc, rec);
                let child = self.threads.len() as Tid;
                let frame = self.new_frame(*func, args, rec);
                let name = format!("{}#{child}", self.program.func(*func).name);
                self.threads.push(Thread {
                    tid: child,
                    name,
                    frames: vec![frame],
                    stack: Vec::new(),
                    state: ThreadState::Runnable,
                    result: Val::Nil,
                    spawned_at: step,
                    finished_at: None,
                });
                rec.sync = Some(SyncOp::Spawn { child });
                rec.result(&Val::Thread(child));
                self.push(Val::Thread(child), Some(step));
            }
            Instr::Builtin { b, argc } => return self.builtin(*b, *argc, oracle, rec),
            Instr::Halt => self.finish_thread(tid, rec),
        }
        Flow::Next
    }

    fn finish_thread(&mut self, tid: Tid, rec: &mut Rec) {
        let t = &mut self.threads[tid as usize];
        t.state = ThreadState::Done;
        t.finished_at = Some(self.step);
        rec.sync = Some(SyncOp::ThreadExit);
        for other in &mut self.threads {
            if other.state == ThreadState::BlockedJoin(tid) {
                other.state = ThreadState::Runnable;
            }
        }
    }

    fn builtin(&mut self, b: Builtin, argc: u32, oracle: &mut dyn Oracle, rec: &mut Rec) -> Flow {
        let step = self.step;
        let tid = self.current;

        // lock/join may block, so they peek and only pop once they succeed
        match b {
            Builtin::Lock => {
                let Val::Mutex(m) = self.peek().val else {
                    let got = self.peek().val.type_name();
                    return Flow::Fault(
                        FaultKind::TypeError,
                        format!("lock() expects a mutex, got {got}"),
                    );
                };
                return match self.mutexes[m as usize].owner {
                    None => {
                        let s = self.pop();
                        rec.dep(s.producer);
                        self.mutexes[m as usize].owner = Some(tid);
                        rec.sync = Some(SyncOp::Acquire { mutex: m });
                        self.push(Val::Nil, Some(step));
                        Flow::Next
                    }
                    Some(owner) if owner == tid => Flow::Fault(
                        FaultKind::Deadlock {
                            cycle: vec![(tid, m)],
                        },
                        format!("T{tid} tried to lock mutex#{m}, which it already holds"),
                    ),
                    Some(owner) => {
                        self.threads[tid as usize].state = ThreadState::BlockedLock(m);
                        rec.sync = Some(SyncOp::BlockLock { mutex: m, owner });
                        Flow::Stay
                    }
                };
            }
            Builtin::Join => {
                let Val::Thread(target) = self.peek().val else {
                    let got = self.peek().val.type_name();
                    return Flow::Fault(
                        FaultKind::TypeError,
                        format!("join() expects a thread, got {got}"),
                    );
                };
                if target == tid {
                    return Flow::Fault(
                        FaultKind::Deadlock {
                            cycle: vec![(tid, u32::MAX)],
                        },
                        format!("T{tid} tried to join itself"),
                    );
                }
                if self.threads[target as usize].state == ThreadState::Done {
                    let s = self.pop();
                    rec.dep(s.producer);
                    let result = self.threads[target as usize].result.clone();
                    rec.sync = Some(SyncOp::Join { target });
                    rec.result(&result);
                    self.push(result, Some(step));
                    return Flow::Next;
                }
                self.threads[tid as usize].state = ThreadState::BlockedJoin(target);
                rec.sync = Some(SyncOp::BlockJoin { target });
                return Flow::Stay;
            }
            _ => {}
        }

        let args = self.pop_args(argc, rec);
        let ret = match b {
            Builtin::Print => {
                let text: Vec<String> = args.iter().map(|a| self.display(&a.val)).collect();
                self.output.push(OutputLine {
                    step,
                    tid,
                    text: text.join(" "),
                });
                Val::Nil
            }
            Builtin::Assert => {
                if !args[0].val.truthy() {
                    let msg = args
                        .get(1)
                        .map(|m| self.display(&m.val))
                        .unwrap_or_else(|| "assertion failed".into());
                    return Flow::Fault(FaultKind::AssertionFailed, msg);
                }
                Val::Nil
            }
            Builtin::Panic => return Flow::Fault(FaultKind::Panic, self.display(&args[0].val)),
            Builtin::Len => match &args[0].val {
                Val::Array(obj) => Val::Int(self.heap[*obj as usize].len() as i64),
                Val::Str(s) => Val::Int(s.chars().count() as i64),
                other => {
                    return Flow::Fault(
                        FaultKind::TypeError,
                        format!("len() of {}", other.type_name()),
                    )
                }
            },
            Builtin::Push => {
                let Val::Array(obj) = args[0].val else {
                    return Flow::Fault(
                        FaultKind::TypeError,
                        format!("push() onto {}", args[0].val.type_name()),
                    );
                };
                let cells = &mut self.heap[obj as usize];
                let idx = cells.len() as u32;
                cells.push(args[1].val.clone());
                self.heap_writers[obj as usize].push(Some(step));
                rec.write(Loc::Heap { obj, idx }, &Val::Nil, &args[1].val);
                Val::Int(idx as i64 + 1)
            }
            Builtin::Str => Val::str(&self.display(&args[0].val)),
            Builtin::Rand => {
                let Val::Int(bound) = args[0].val else {
                    return Flow::Fault(FaultKind::TypeError, "rand() expects an int bound".into());
                };
                if bound <= 0 {
                    return Flow::Fault(
                        FaultKind::Panic,
                        format!("rand({bound}): bound must be positive"),
                    );
                }
                rec.nondet = Some(NondetKind::Rand);
                match oracle.rand(step, bound) {
                    Ok(v) => Val::Int(v),
                    Err(e) => return Flow::Fault(FaultKind::Panic, e.to_string()),
                }
            }
            Builtin::Now => {
                rec.nondet = Some(NondetKind::Clock);
                match oracle.clock(step) {
                    Ok(v) => Val::Int(v),
                    Err(e) => return Flow::Fault(FaultKind::Panic, e.to_string()),
                }
            }
            Builtin::Input => {
                rec.nondet = Some(NondetKind::Input);
                match oracle.input(step) {
                    Ok(v) => Val::str(&v),
                    Err(e) => return Flow::Fault(FaultKind::Panic, e.to_string()),
                }
            }
            Builtin::Mutex => {
                let id = self.mutexes.len() as u32;
                self.mutexes.push(MutexState {
                    owner: None,
                    created_at: step,
                });
                Val::Mutex(id)
            }
            Builtin::Unlock => {
                let Val::Mutex(m) = args[0].val else {
                    return Flow::Fault(FaultKind::TypeError, "unlock() expects a mutex".into());
                };
                if self.mutexes[m as usize].owner != Some(tid) {
                    return Flow::Fault(
                        FaultKind::UnlockNotHeld,
                        format!("T{tid} unlocked mutex#{m} it does not hold"),
                    );
                }
                self.mutexes[m as usize].owner = None;
                rec.sync = Some(SyncOp::Release { mutex: m });
                for t in &mut self.threads {
                    if t.state == ThreadState::BlockedLock(m) {
                        t.state = ThreadState::Runnable;
                    }
                }
                Val::Nil
            }
            Builtin::Yield => {
                self.yielded = true;
                Val::Nil
            }
            Builtin::Lock | Builtin::Join => unreachable!(),
        };
        rec.result(&ret);
        self.push(ret, Some(step));
        Flow::Next
    }

    /// How a value prints: strings raw, arrays expanded (one level deep is
    /// plenty for a debugger panel).
    pub fn display(&self, v: &Val) -> String {
        match v {
            Val::Str(s) => s.to_string(),
            Val::Array(obj) => {
                let items: Vec<String> = self.heap[*obj as usize]
                    .iter()
                    .map(|x| match x {
                        Val::Array(o) => format!("array#{o}"),
                        Val::Str(s) => format!("{s:?}"),
                        other => other.to_string(),
                    })
                    .collect();
                format!("[{}]", items.join(", "))
            }
            other => other.to_string(),
        }
    }
}

fn binary(op: BinOp, a: &Val, b: &Val) -> Result<Val, (FaultKind, String)> {
    use Val::*;
    let overflow = || {
        (
            FaultKind::Panic,
            format!("integer overflow in {a} {op:?} {b}"),
        )
    };
    Ok(match (op, a, b) {
        (BinOp::Add, Int(x), Int(y)) => Int(x.checked_add(*y).ok_or_else(overflow)?),
        (BinOp::Sub, Int(x), Int(y)) => Int(x.checked_sub(*y).ok_or_else(overflow)?),
        (BinOp::Mul, Int(x), Int(y)) => Int(x.checked_mul(*y).ok_or_else(overflow)?),
        (BinOp::Div | BinOp::Mod, Int(_), Int(0)) => {
            return Err((
                FaultKind::DivisionByZero,
                format!("{a} {} 0", if op == BinOp::Div { "/" } else { "%" }),
            ))
        }
        (BinOp::Div, Int(x), Int(y)) => Int(x.wrapping_div(*y)),
        (BinOp::Mod, Int(x), Int(y)) => Int(x.wrapping_rem(*y)),
        (BinOp::Add, Str(x), y) => Val::str(&format!("{x}{}", plain(y))),
        (BinOp::Add, x, Str(y)) => Val::str(&format!("{}{y}", plain(x))),
        (BinOp::Eq, x, y) => Bool(x == y),
        (BinOp::Ne, x, y) => Bool(x != y),
        (BinOp::Lt, Int(x), Int(y)) => Bool(x < y),
        (BinOp::Le, Int(x), Int(y)) => Bool(x <= y),
        (BinOp::Gt, Int(x), Int(y)) => Bool(x > y),
        (BinOp::Ge, Int(x), Int(y)) => Bool(x >= y),
        (op, x, y) => {
            return Err((
                FaultKind::TypeError,
                format!(
                    "unsupported operation: {} {op:?} {}",
                    x.type_name(),
                    y.type_name()
                ),
            ))
        }
    })
}

fn plain(v: &Val) -> String {
    match v {
        Val::Str(s) => s.to_string(),
        other => other.to_string(),
    }
}
