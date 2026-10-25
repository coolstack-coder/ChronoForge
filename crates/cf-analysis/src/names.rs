use std::collections::HashMap;

use cf_lang::Program;
use cf_trace::{Loc, StepEvent, Val};

/// Turns raw locations into names a human recognises. Locals need the trace
/// (frame ids are only known at runtime), mutexes need to know which global
/// they were stored in.
pub struct Names<'a> {
    program: &'a Program,
    frames: HashMap<u64, u32>,
    mutexes: HashMap<u32, String>,
    arrays: HashMap<u32, String>,
}

impl<'a> Names<'a> {
    pub fn new(program: &'a Program, steps: &[StepEvent]) -> Self {
        let mut frames = HashMap::new();
        let mut mutexes = HashMap::new();
        let mut arrays: HashMap<u32, String> = HashMap::new();
        for s in steps {
            for w in &s.writes {
                if let Loc::Local { frame, .. } = w.loc {
                    // params are written by the Call step itself, from the caller's func
                    if !matches!(s.op, cf_trace::OpTag::Call | cf_trace::OpTag::Spawn) {
                        frames.entry(frame).or_insert(s.func);
                    }
                }
                // name heap objects after the first global they're stored in
                match (&w.new, w.loc) {
                    (Val::Mutex(m), Loc::Global(g)) => {
                        mutexes
                            .entry(*m)
                            .or_insert_with(|| program.globals[g as usize].clone());
                    }
                    (Val::Array(a), Loc::Global(g)) => {
                        arrays
                            .entry(*a)
                            .or_insert_with(|| program.globals[g as usize].clone());
                    }
                    _ => {}
                }
            }
            for r in &s.reads {
                if let Loc::Local { frame, .. } = r.loc {
                    frames.entry(frame).or_insert(s.func);
                }
            }
        }
        // arrays literal-initialised with mutexes are created before they're
        // stored anywhere, so fix those names up once we know the array's name
        for s in steps {
            for w in &s.writes {
                if let (Val::Mutex(m), Loc::Heap { obj, idx }) = (&w.new, w.loc) {
                    if let Some(arr) = arrays.get(&obj) {
                        mutexes.insert(*m, format!("{arr}[{idx}]"));
                    }
                }
            }
        }
        Names {
            program,
            frames,
            mutexes,
            arrays,
        }
    }

    pub fn loc(&self, loc: Loc) -> String {
        match loc {
            Loc::Global(g) => self.program.globals[g as usize].clone(),
            Loc::Heap { obj, idx } => match self.arrays.get(&obj) {
                Some(name) => format!("{name}[{idx}]"),
                None => format!("array#{obj}[{idx}]"),
            },
            Loc::Local { frame, slot } => match self.frames.get(&frame) {
                Some(func) => {
                    let f = self.program.func(*func);
                    let local = f
                        .local_names
                        .get(slot as usize)
                        .map(String::as_str)
                        .unwrap_or("?");
                    format!("{}.{}", f.name, local.split('.').next().unwrap_or(local))
                }
                None => format!("local#{frame}.{slot}"),
            },
        }
    }

    pub fn mutex(&self, m: u32) -> String {
        self.mutexes
            .get(&m)
            .cloned()
            .unwrap_or_else(|| format!("mutex#{m}"))
    }

    pub fn func(&self, id: u32) -> &str {
        &self.program.func(id).name
    }

    pub fn source_line(&self, line: u32) -> String {
        self.program
            .source
            .lines()
            .nth(line.saturating_sub(1) as usize)
            .map(|l| l.trim().to_string())
            .unwrap_or_default()
    }
}
