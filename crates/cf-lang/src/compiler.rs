use std::collections::HashMap;

use cf_trace::Val;

use crate::ast::*;
use crate::bytecode::{Builtin, Function, Instr, Program};
use crate::CompileError;

struct LoopCtx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

struct FnCtx {
    scopes: Vec<Vec<(String, u32)>>,
    nlocals: u32,
    local_names: Vec<String>,
    code: Vec<Instr>,
    lines: Vec<u32>,
    loops: Vec<LoopCtx>,
    is_main: bool,
}

impl FnCtx {
    fn new(is_main: bool) -> Self {
        FnCtx {
            scopes: vec![Vec::new()],
            nlocals: 0,
            local_names: Vec::new(),
            code: Vec::new(),
            lines: Vec::new(),
            loops: Vec::new(),
            is_main,
        }
    }

    fn emit(&mut self, ins: Instr, line: u32) -> usize {
        self.code.push(ins);
        self.lines.push(line);
        self.code.len() - 1
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn patch(&mut self, at: usize, target: u32) {
        match &mut self.code[at] {
            Instr::Jump(t)
            | Instr::JumpIfFalse(t)
            | Instr::JumpIfFalseKeep(t)
            | Instr::JumpIfTrueKeep(t) => *t = target,
            other => unreachable!("patching a non-jump {other:?}"),
        }
    }

    fn declare(&mut self, name: &str) -> u32 {
        let slot = self.nlocals;
        self.nlocals += 1;
        self.local_names.push(name.to_string());
        self.scopes
            .last_mut()
            .unwrap()
            .push((name.to_string(), slot));
        slot
    }

    fn lookup(&self, name: &str) -> Option<u32> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, slot)| *slot)
    }
}

pub struct Compiler {
    globals: Vec<String>,
    global_ids: HashMap<String, u32>,
    func_ids: HashMap<String, u32>,
    func_arity: Vec<usize>,
}

impl Compiler {
    pub fn compile(source: &str, stmts: Vec<Stmt>) -> Result<Program, CompileError> {
        let mut c = Compiler {
            globals: Vec::new(),
            global_ids: HashMap::new(),
            func_ids: HashMap::new(),
            func_arity: Vec::new(),
        };

        // Pass 1: every top-level `let` is a global and every `fn` gets an id,
        // so functions can reference globals and each other in any order.
        let mut decls = Vec::new();
        let mut main_body = Vec::new();
        for s in stmts {
            match s.kind {
                StmtKind::Fn(decl) => {
                    if Builtin::lookup(&decl.name).is_some() {
                        return Err(CompileError::new(
                            decl.line,
                            1,
                            format!("`{}` is a builtin", decl.name),
                        ));
                    }
                    if c.func_ids.contains_key(&decl.name) {
                        return Err(CompileError::new(
                            decl.line,
                            1,
                            format!("function `{}` defined twice", decl.name),
                        ));
                    }
                    c.func_ids.insert(decl.name.clone(), decls.len() as u32);
                    c.func_arity.push(decl.params.len());
                    decls.push(decl);
                }
                _ => {
                    if let StmtKind::Let(name, _) = &s.kind {
                        c.global(name);
                    }
                    main_body.push(s);
                }
            }
        }

        let mut funcs = Vec::new();
        for decl in &decls {
            funcs.push(c.function(decl)?);
        }

        let mut main = FnCtx::new(true);
        for s in &main_body {
            c.stmt(&mut main, s)?;
        }
        let last_line = main_body.last().map(|s| s.line).unwrap_or(1);
        main.emit(Instr::Halt, last_line);
        let main_id = funcs.len() as u32;
        funcs.push(Function {
            name: "main".into(),
            params: 0,
            nlocals: main.nlocals,
            local_names: main.local_names,
            code: main.code,
            lines: main.lines,
            line: 1,
        });

        let plan = crate::instrument::instrument(&funcs);
        Ok(Program {
            source: source.to_string(),
            funcs,
            globals: c.globals,
            main: main_id,
            plan,
        })
    }

    fn global(&mut self, name: &str) -> u32 {
        if let Some(id) = self.global_ids.get(name) {
            return *id;
        }
        let id = self.globals.len() as u32;
        self.globals.push(name.to_string());
        self.global_ids.insert(name.to_string(), id);
        id
    }

    fn function(&mut self, decl: &FnDecl) -> Result<Function, CompileError> {
        let mut f = FnCtx::new(false);
        for p in &decl.params {
            f.declare(p);
        }
        for s in &decl.body {
            self.stmt(&mut f, s)?;
        }
        let end = decl.body.last().map(|s| s.line).unwrap_or(decl.line);
        f.emit(Instr::Push(Val::Nil), end);
        f.emit(Instr::Return, end);
        Ok(Function {
            name: decl.name.clone(),
            params: decl.params.len() as u32,
            nlocals: f.nlocals,
            local_names: f.local_names,
            code: f.code,
            lines: f.lines,
            line: decl.line,
        })
    }

    fn block(&mut self, f: &mut FnCtx, body: &[Stmt]) -> Result<(), CompileError> {
        f.scopes.push(Vec::new());
        for s in body {
            self.stmt(f, s)?;
        }
        f.scopes.pop();
        Ok(())
    }

    fn store(&mut self, f: &mut FnCtx, name: &str, line: u32) -> Result<(), CompileError> {
        if let Some(slot) = f.lookup(name) {
            f.emit(Instr::StoreLocal(slot), line);
        } else if let Some(&g) = self.global_ids.get(name) {
            f.emit(Instr::StoreGlobal(g), line);
        } else {
            return Err(CompileError::new(
                line,
                1,
                format!("assignment to undeclared variable `{name}` (use `let`)"),
            ));
        }
        Ok(())
    }

    fn stmt(&mut self, f: &mut FnCtx, s: &Stmt) -> Result<(), CompileError> {
        let line = s.line;
        match &s.kind {
            StmtKind::Let(name, value) => {
                self.expr(f, value)?;
                // only the outermost scope of main creates globals
                if f.is_main && f.scopes.len() == 1 {
                    let g = self.global(name);
                    f.emit(Instr::StoreGlobal(g), line);
                } else {
                    let slot = f.declare(name);
                    f.emit(Instr::StoreLocal(slot), line);
                }
            }
            StmtKind::Assign(name, value) => {
                self.expr(f, value)?;
                self.store(f, name, line)?;
            }
            StmtKind::AssignIndex(arr, idx, value) => {
                self.expr(f, arr)?;
                self.expr(f, idx)?;
                self.expr(f, value)?;
                f.emit(Instr::StoreIndex, line);
            }
            StmtKind::If(cond, then, otherwise) => {
                self.expr(f, cond)?;
                let jf = f.emit(Instr::JumpIfFalse(0), line);
                self.block(f, then)?;
                match otherwise {
                    Some(other) => {
                        let jend = f.emit(Instr::Jump(0), line);
                        f.patch(jf, f.here());
                        self.block(f, other)?;
                        f.patch(jend, f.here());
                    }
                    None => f.patch(jf, f.here()),
                }
            }
            StmtKind::While(cond, body) => {
                let top = f.here();
                self.expr(f, cond)?;
                let exit = f.emit(Instr::JumpIfFalse(0), line);
                f.loops.push(LoopCtx {
                    breaks: vec![],
                    continues: vec![],
                });
                self.block(f, body)?;
                let ctx = f.loops.pop().unwrap();
                f.emit(Instr::Jump(top), line);
                let end = f.here();
                f.patch(exit, end);
                ctx.breaks.into_iter().for_each(|b| f.patch(b, end));
                ctx.continues.into_iter().for_each(|c| f.patch(c, top));
            }
            StmtKind::For(var, from, to, body) => {
                // for i in a..b { body }  =>  i = a; end = b; while i < end { body; i += 1 }
                f.scopes.push(Vec::new());
                self.expr(f, from)?;
                let i = f.declare(var);
                f.emit(Instr::StoreLocal(i), line);
                self.expr(f, to)?;
                let end_slot = f.declare(&format!("{var}.end"));
                f.emit(Instr::StoreLocal(end_slot), line);
                let top = f.here();
                f.emit(Instr::LoadLocal(i), line);
                f.emit(Instr::LoadLocal(end_slot), line);
                f.emit(Instr::Binary(BinOp::Lt), line);
                let exit = f.emit(Instr::JumpIfFalse(0), line);
                f.loops.push(LoopCtx {
                    breaks: vec![],
                    continues: vec![],
                });
                self.block(f, body)?;
                let ctx = f.loops.pop().unwrap();
                let step = f.here();
                f.emit(Instr::LoadLocal(i), line);
                f.emit(Instr::Push(Val::Int(1)), line);
                f.emit(Instr::Binary(BinOp::Add), line);
                f.emit(Instr::StoreLocal(i), line);
                f.emit(Instr::Jump(top), line);
                let end = f.here();
                f.patch(exit, end);
                ctx.breaks.into_iter().for_each(|b| f.patch(b, end));
                ctx.continues.into_iter().for_each(|c| f.patch(c, step));
                f.scopes.pop();
            }
            StmtKind::Return(value) => {
                if f.is_main {
                    return Err(CompileError::new(line, 1, "`return` outside of a function"));
                }
                match value {
                    Some(v) => self.expr(f, v)?,
                    None => {
                        f.emit(Instr::Push(Val::Nil), line);
                    }
                }
                f.emit(Instr::Return, line);
            }
            StmtKind::Break | StmtKind::Continue => {
                let is_break = matches!(s.kind, StmtKind::Break);
                if f.loops.is_empty() {
                    let what = if is_break { "break" } else { "continue" };
                    return Err(CompileError::new(
                        line,
                        1,
                        format!("`{what}` outside of a loop"),
                    ));
                }
                let j = f.emit(Instr::Jump(0), line);
                let ctx = f.loops.last_mut().unwrap();
                if is_break {
                    ctx.breaks.push(j);
                } else {
                    ctx.continues.push(j);
                }
            }
            StmtKind::Expr(e) => {
                self.expr(f, e)?;
                f.emit(Instr::Pop, line);
            }
            StmtKind::Fn(decl) => {
                return Err(CompileError::new(
                    decl.line,
                    1,
                    "nested functions are not supported",
                ));
            }
        }
        Ok(())
    }

    fn call_args(&mut self, f: &mut FnCtx, args: &[Expr]) -> Result<u32, CompileError> {
        for a in args {
            self.expr(f, a)?;
        }
        Ok(args.len() as u32)
    }

    fn user_fn(&self, name: &str, argc: usize, line: u32) -> Result<u32, CompileError> {
        let id = *self
            .func_ids
            .get(name)
            .ok_or_else(|| CompileError::new(line, 1, format!("unknown function `{name}`")))?;
        let want = self.func_arity[id as usize];
        if want != argc {
            return Err(CompileError::new(
                line,
                1,
                format!("`{name}` takes {want} argument(s), got {argc}"),
            ));
        }
        Ok(id)
    }

    fn expr(&mut self, f: &mut FnCtx, e: &Expr) -> Result<(), CompileError> {
        let line = e.line;
        match &e.kind {
            ExprKind::Int(v) => {
                f.emit(Instr::Push(Val::Int(*v)), line);
            }
            ExprKind::Str(s) => {
                f.emit(Instr::Push(Val::str(s)), line);
            }
            ExprKind::Bool(b) => {
                f.emit(Instr::Push(Val::Bool(*b)), line);
            }
            ExprKind::Nil => {
                f.emit(Instr::Push(Val::Nil), line);
            }
            ExprKind::Var(name) => {
                if let Some(slot) = f.lookup(name) {
                    f.emit(Instr::LoadLocal(slot), line);
                } else if let Some(&g) = self.global_ids.get(name) {
                    f.emit(Instr::LoadGlobal(g), line);
                } else {
                    return Err(CompileError::new(
                        line,
                        1,
                        format!("undefined variable `{name}`"),
                    ));
                }
            }
            ExprKind::Array(items) => {
                let n = self.call_args(f, items)?;
                f.emit(Instr::MakeArray(n), line);
            }
            ExprKind::Index(arr, idx) => {
                self.expr(f, arr)?;
                self.expr(f, idx)?;
                f.emit(Instr::Index, line);
            }
            ExprKind::Call(name, args) => {
                if let Some(b) = Builtin::lookup(name) {
                    let (min, max) = b.arity();
                    if args.len() < min || max.is_some_and(|m| args.len() > m) {
                        return Err(CompileError::new(
                            line,
                            1,
                            format!("wrong number of arguments to `{name}`"),
                        ));
                    }
                    let argc = self.call_args(f, args)?;
                    f.emit(Instr::Builtin { b, argc }, line);
                } else {
                    let func = self.user_fn(name, args.len(), line)?;
                    let argc = self.call_args(f, args)?;
                    f.emit(Instr::Call { func, argc }, line);
                }
            }
            ExprKind::Spawn(name, args) => {
                let func = self.user_fn(name, args.len(), line)?;
                let argc = self.call_args(f, args)?;
                f.emit(Instr::Spawn { func, argc }, line);
            }
            ExprKind::Unary(op, inner) => {
                self.expr(f, inner)?;
                f.emit(Instr::Unary(*op), line);
            }
            ExprKind::Binary(op, l, r) => {
                self.expr(f, l)?;
                self.expr(f, r)?;
                f.emit(Instr::Binary(*op), line);
            }
            ExprKind::And(l, r) => {
                self.expr(f, l)?;
                let j = f.emit(Instr::JumpIfFalseKeep(0), line);
                self.expr(f, r)?;
                f.patch(j, f.here());
            }
            ExprKind::Or(l, r) => {
                self.expr(f, l)?;
                let j = f.emit(Instr::JumpIfTrueKeep(0), line);
                self.expr(f, r)?;
                f.patch(j, f.here());
            }
        }
        Ok(())
    }
}
