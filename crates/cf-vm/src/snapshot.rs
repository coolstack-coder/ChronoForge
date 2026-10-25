//! A serialisable picture of the machine at one moment — what the debugger
//! panels render.

use cf_trace::{Loc, OutputLine, Step, Tid, Val};
use serde::Serialize;

use crate::machine::{Machine, ThreadState};

#[derive(Clone, Debug, Serialize)]
pub struct VarSnap {
    pub name: String,
    pub loc: Loc,
    pub value: String,
    pub kind: &'static str,
    /// Step that last wrote this variable — the "who did this?" jump.
    pub writer: Option<Step>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameSnap {
    pub func: String,
    pub line: u32,
    pub locals: Vec<VarSnap>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThreadSnap {
    pub tid: Tid,
    pub name: String,
    pub state: String,
    pub line: u32,
    pub frames: Vec<FrameSnap>,
    pub stack: Vec<String>,
    pub spawned_at: Step,
    pub finished_at: Option<Step>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HeapSnap {
    pub id: u32,
    pub items: Vec<VarSnap>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MutexSnap {
    pub id: u32,
    pub owner: Option<Tid>,
    pub waiters: Vec<Tid>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub position: Step,
    pub current: Tid,
    pub threads: Vec<ThreadSnap>,
    pub globals: Vec<VarSnap>,
    pub heap: Vec<HeapSnap>,
    pub mutexes: Vec<MutexSnap>,
    pub output: Vec<OutputLine>,
}

fn var(m: &Machine, name: String, loc: Loc, v: &Val, writer: Option<Step>) -> VarSnap {
    VarSnap {
        name,
        loc,
        value: m.display_quoted(v),
        kind: v.type_name(),
        writer,
    }
}

impl Machine {
    fn display_quoted(&self, v: &Val) -> String {
        match v {
            Val::Str(s) => format!("{s:?}"),
            other => self.display(other),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let threads = self
            .threads
            .iter()
            .map(|t| {
                let frames = t
                    .frames
                    .iter()
                    .rev()
                    .map(|f| {
                        let func = self.program.func(f.func);
                        let pc = (f.pc as usize).min(func.lines.len() - 1);
                        let locals = f
                            .locals
                            .iter()
                            .enumerate()
                            // hide compiler temporaries like `i.end`
                            .filter(|(i, _)| !func.local_names[*i].contains('.'))
                            .map(|(i, v)| {
                                let loc = Loc::Local {
                                    frame: f.id,
                                    slot: i as u32,
                                };
                                var(self, func.local_names[i].clone(), loc, v, f.writers[i])
                            })
                            .collect();
                        FrameSnap {
                            func: func.name.clone(),
                            line: func.lines[pc],
                            locals,
                        }
                    })
                    .collect();
                ThreadSnap {
                    tid: t.tid,
                    name: t.name.clone(),
                    state: match t.state {
                        ThreadState::Runnable => "runnable".into(),
                        ThreadState::BlockedLock(m) => format!("blocked on mutex#{m}"),
                        ThreadState::BlockedJoin(j) => format!("joining T{j}"),
                        ThreadState::Done => "done".into(),
                    },
                    line: self.line_of(t.tid),
                    frames,
                    stack: t
                        .stack
                        .iter()
                        .map(|s| self.display_quoted(&s.val))
                        .collect(),
                    spawned_at: t.spawned_at,
                    finished_at: t.finished_at,
                }
            })
            .collect();

        let globals = self
            .program
            .globals
            .iter()
            .enumerate()
            .map(|(i, name)| {
                var(
                    self,
                    name.clone(),
                    Loc::Global(i as u32),
                    &self.globals[i],
                    self.global_writers[i],
                )
            })
            .collect();

        let heap = self
            .heap
            .iter()
            .enumerate()
            .map(|(obj, cells)| HeapSnap {
                id: obj as u32,
                items: cells
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let loc = Loc::Heap {
                            obj: obj as u32,
                            idx: i as u32,
                        };
                        var(self, format!("[{i}]"), loc, v, self.heap_writers[obj][i])
                    })
                    .collect(),
            })
            .collect();

        let mutexes = self
            .mutexes
            .iter()
            .enumerate()
            .map(|(id, mx)| MutexSnap {
                id: id as u32,
                owner: mx.owner,
                waiters: self
                    .threads
                    .iter()
                    .filter(|t| t.state == ThreadState::BlockedLock(id as u32))
                    .map(|t| t.tid)
                    .collect(),
            })
            .collect();

        Snapshot {
            position: self.step,
            current: self.current,
            threads,
            globals,
            heap,
            mutexes,
            output: self.output.clone(),
        }
    }

    /// Friendly name for a location, e.g. `balance`, `withdraw.b`, `array#2[1]`.
    pub fn loc_name(&self, loc: Loc) -> String {
        match loc {
            Loc::Global(g) => self
                .program
                .globals
                .get(g as usize)
                .cloned()
                .unwrap_or_else(|| format!("g{g}")),
            Loc::Heap { obj, idx } => format!("array#{obj}[{idx}]"),
            Loc::Local { frame, slot } => {
                for t in &self.threads {
                    if let Some(f) = t.frames.iter().find(|f| f.id == frame) {
                        let func = self.program.func(f.func);
                        return format!("{}.{}", func.name, func.local_names[slot as usize]);
                    }
                }
                format!("local{frame}.{slot}")
            }
        }
    }
}
