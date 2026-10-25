use cf_trace::{OpTag, Val};
use serde::Serialize;

pub use crate::ast::{BinOp, UnOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Builtin {
    Print,
    Assert,
    Len,
    Push,
    Str,
    Rand,
    Now,
    Input,
    Mutex,
    Lock,
    Unlock,
    Join,
    Yield,
    Panic,
}

impl Builtin {
    pub fn lookup(name: &str) -> Option<Builtin> {
        Some(match name {
            "print" => Builtin::Print,
            "assert" => Builtin::Assert,
            "len" => Builtin::Len,
            "push" => Builtin::Push,
            "str" => Builtin::Str,
            "rand" => Builtin::Rand,
            "now" => Builtin::Now,
            "input" => Builtin::Input,
            "mutex" => Builtin::Mutex,
            "lock" => Builtin::Lock,
            "unlock" => Builtin::Unlock,
            "join" => Builtin::Join,
            "yield" => Builtin::Yield,
            "panic" => Builtin::Panic,
            _ => return None,
        })
    }

    /// (min, max) argument count; `None` max means variadic.
    pub fn arity(self) -> (usize, Option<usize>) {
        match self {
            Builtin::Print => (0, None),
            Builtin::Assert => (1, Some(2)),
            Builtin::Push => (2, Some(2)),
            Builtin::Now | Builtin::Input | Builtin::Mutex | Builtin::Yield => (0, Some(0)),
            _ => (1, Some(1)),
        }
    }

    pub fn is_nondeterministic(self) -> bool {
        matches!(self, Builtin::Rand | Builtin::Now | Builtin::Input)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Instr {
    Push(Val),
    Pop,
    LoadLocal(u32),
    StoreLocal(u32),
    LoadGlobal(u32),
    StoreGlobal(u32),
    Binary(BinOp),
    Unary(UnOp),
    Jump(u32),
    JumpIfFalse(u32),
    /// Short-circuit helpers: jump and keep the value, or pop and fall through.
    JumpIfFalseKeep(u32),
    JumpIfTrueKeep(u32),
    MakeArray(u32),
    Index,
    StoreIndex,
    Call {
        func: u32,
        argc: u32,
    },
    Return,
    Spawn {
        func: u32,
        argc: u32,
    },
    Builtin {
        b: Builtin,
        argc: u32,
    },
    Halt,
}

impl Instr {
    pub fn tag(&self) -> OpTag {
        match self {
            Instr::Push(_) => OpTag::Const,
            Instr::Pop => OpTag::Pop,
            Instr::LoadLocal(_) | Instr::LoadGlobal(_) => OpTag::Load,
            Instr::StoreLocal(_) | Instr::StoreGlobal(_) => OpTag::Store,
            Instr::Binary(op) => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => OpTag::Arith,
                _ => OpTag::Compare,
            },
            Instr::Unary(_) => OpTag::Arith,
            Instr::Jump(_) => OpTag::Jump,
            Instr::JumpIfFalse(_) | Instr::JumpIfFalseKeep(_) | Instr::JumpIfTrueKeep(_) => {
                OpTag::Branch
            }
            Instr::MakeArray(_) => OpTag::Array,
            Instr::Index => OpTag::Index,
            Instr::StoreIndex => OpTag::StoreIndex,
            Instr::Call { .. } => OpTag::Call,
            Instr::Return => OpTag::Return,
            Instr::Spawn { .. } => OpTag::Spawn,
            Instr::Builtin { b, .. } => match b {
                Builtin::Lock => OpTag::Lock,
                Builtin::Unlock => OpTag::Unlock,
                Builtin::Join => OpTag::Join,
                Builtin::Yield => OpTag::Yield,
                Builtin::Assert => OpTag::Assert,
                Builtin::Print => OpTag::Print,
                b if b.is_nondeterministic() => OpTag::Nondet,
                _ => OpTag::Builtin,
            },
            Instr::Halt => OpTag::Halt,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Function {
    pub name: String,
    pub params: u32,
    pub nlocals: u32,
    pub local_names: Vec<String>,
    pub code: Vec<Instr>,
    /// Source line for every instruction (same length as `code`).
    pub lines: Vec<u32>,
    pub line: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Program {
    pub source: String,
    pub funcs: Vec<Function>,
    pub globals: Vec<String>,
    pub main: u32,
    pub plan: crate::instrument::Plan,
}

impl Program {
    pub fn func(&self, id: u32) -> &Function {
        &self.funcs[id as usize]
    }

    /// Human readable listing, used by `chronoforge disasm`.
    pub fn disassemble(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (id, f) in self.funcs.iter().enumerate() {
            let _ = writeln!(
                out,
                "fn#{id} {} (params={}, locals={})",
                f.name, f.params, f.nlocals
            );
            for (pc, ins) in f.code.iter().enumerate() {
                let probe = self.plan.probe(id as u32, pc as u32);
                let _ = writeln!(
                    out,
                    "  {pc:>4}  L{:<4} {:<6} {ins:?}",
                    f.lines[pc],
                    probe.label()
                );
            }
        }
        out
    }
}
