//! Forge — a deliberately small language with threads, mutexes and
//! nondeterministic inputs. Small enough to instrument every instruction,
//! big enough to write real race conditions and deadlocks in.
//!
//! ```text
//! let balance = 100;
//! fn withdraw(n) {
//!     let b = balance;
//!     if b >= n { balance = b - n; }
//! }
//! spawn withdraw(80);
//! spawn withdraw(50);
//! ```

pub mod ast;
pub mod bytecode;
pub mod compiler;
pub mod instrument;
pub mod lexer;
pub mod parser;

pub use bytecode::{Builtin, Function, Instr, Program};
pub use instrument::{Plan, PlanStats, Probe};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, serde::Serialize)]
#[error("line {line}:{col}: {message}")]
pub struct CompileError {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl CompileError {
    pub fn new(line: u32, col: u32, message: impl Into<String>) -> Self {
        CompileError {
            line,
            col,
            message: message.into(),
        }
    }
}

pub fn compile(source: &str) -> Result<Program, CompileError> {
    let toks = lexer::lex(source)?;
    let stmts = parser::Parser::new(toks).program()?;
    compiler::Compiler::compile(source, stmts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_globals_functions_and_loops() {
        let p = compile(
            r#"
            let total = 0;
            fn add(n) { total = total + n; return total; }
            for i in 0..3 { add(i); }
            while total < 100 { total += 10; if total == 50 { break; } }
            "#,
        )
        .unwrap();
        assert_eq!(p.globals, vec!["total"]);
        assert_eq!(p.funcs.len(), 2);
        assert_eq!(p.funcs[p.main as usize].name, "main");
        assert!(p.plan.stats.shared_probes > 0);
    }

    #[test]
    fn rejects_undefined_names_with_a_line() {
        let err = compile("let a = 1;\nprint(b);").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.message.contains("`b`"));
    }

    #[test]
    fn checks_arity() {
        assert!(compile("fn f(a, b) {}\nf(1);").is_err());
        assert!(compile("lock();").is_err());
    }

    #[test]
    fn nested_lets_in_main_are_locals() {
        let p = compile("let a = 1;\nif a { let b = 2; print(b); }").unwrap();
        assert_eq!(p.globals, vec!["a"]);
        assert_eq!(p.funcs[p.main as usize].local_names, vec!["b"]);
    }
}
