//! Recursive-descent parser. Precedence, low to high:
//! `||`  `&&`  `== !=`  `< <= > >=`  `+ -`  `* / %`  unary  postfix `[i]`

use crate::ast::*;
use crate::lexer::{Tok, Token};
use crate::CompileError;

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0 }
    }

    pub fn program(mut self) -> Result<Vec<Stmt>, CompileError> {
        let mut stmts = Vec::new();
        while !self.at(&Tok::Eof) {
            stmts.push(self.stmt(true)?);
        }
        Ok(stmts)
    }

    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn at(&self, t: &Tok) -> bool {
        &self.peek().tok == t
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, CompileError> {
        let t = self.peek();
        Err(CompileError::new(t.line, t.col, msg))
    }

    fn expect(&mut self, t: Tok, what: &str) -> Result<Token, CompileError> {
        if self.at(&t) {
            Ok(self.bump())
        } else {
            self.err(format!(
                "expected {what}, found {}",
                describe(&self.peek().tok)
            ))
        }
    }

    fn ident(&mut self, what: &str) -> Result<String, CompileError> {
        match self.peek().tok.clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok(name)
            }
            other => self.err(format!("expected {what}, found {}", describe(&other))),
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>, CompileError> {
        self.expect(Tok::LBrace, "`{`")?;
        let mut body = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.at(&Tok::Eof) {
                return self.err("unclosed block, expected `}`");
            }
            body.push(self.stmt(false)?);
        }
        self.bump();
        Ok(body)
    }

    fn stmt(&mut self, top_level: bool) -> Result<Stmt, CompileError> {
        let line = self.peek().line;
        let kind = match self.peek().tok.clone() {
            Tok::Fn => {
                if !top_level {
                    return self.err("functions can only be declared at the top level");
                }
                self.bump();
                let name = self.ident("function name")?;
                self.expect(Tok::LParen, "`(`")?;
                let mut params = Vec::new();
                while !self.at(&Tok::RParen) {
                    params.push(self.ident("parameter name")?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(Tok::RParen, "`)`")?;
                let body = self.block()?;
                StmtKind::Fn(FnDecl {
                    name,
                    params,
                    body,
                    line,
                })
            }
            Tok::Let => {
                self.bump();
                let name = self.ident("variable name")?;
                self.expect(Tok::Assign, "`=`")?;
                let value = self.expr()?;
                self.expect(Tok::Semi, "`;`")?;
                StmtKind::Let(name, value)
            }
            Tok::If => self.if_stmt()?,
            Tok::While => {
                self.bump();
                let cond = self.expr()?;
                StmtKind::While(cond, self.block()?)
            }
            Tok::For => {
                self.bump();
                let var = self.ident("loop variable")?;
                self.expect(Tok::In, "`in`")?;
                let from = self.expr()?;
                self.expect(Tok::DotDot, "`..`")?;
                let to = self.expr()?;
                StmtKind::For(var, from, to, self.block()?)
            }
            Tok::Return => {
                self.bump();
                let value = if self.at(&Tok::Semi) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(Tok::Semi, "`;`")?;
                StmtKind::Return(value)
            }
            Tok::Break => {
                self.bump();
                self.expect(Tok::Semi, "`;`")?;
                StmtKind::Break
            }
            Tok::Continue => {
                self.bump();
                self.expect(Tok::Semi, "`;`")?;
                StmtKind::Continue
            }
            _ => {
                let target = self.expr()?;
                let kind = if self.eat(&Tok::Assign) {
                    let value = self.expr()?;
                    assignment(target, value)?
                } else if self.at(&Tok::PlusAssign) || self.at(&Tok::MinusAssign) {
                    // `x += e` desugars to `x = x + e`
                    let op = if self.bump().tok == Tok::PlusAssign {
                        BinOp::Add
                    } else {
                        BinOp::Sub
                    };
                    let rhs = self.expr()?;
                    let value = Expr {
                        line,
                        kind: ExprKind::Binary(op, Box::new(target.clone()), Box::new(rhs)),
                    };
                    assignment(target, value)?
                } else {
                    StmtKind::Expr(target)
                };
                self.expect(Tok::Semi, "`;`")?;
                kind
            }
        };
        Ok(Stmt { kind, line })
    }

    fn if_stmt(&mut self) -> Result<StmtKind, CompileError> {
        self.expect(Tok::If, "`if`")?;
        let cond = self.expr()?;
        let then = self.block()?;
        let otherwise = if self.eat(&Tok::Else) {
            if self.at(&Tok::If) {
                let line = self.peek().line;
                Some(vec![Stmt {
                    kind: self.if_stmt()?,
                    line,
                }])
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(StmtKind::If(cond, then, otherwise))
    }

    pub fn expr(&mut self) -> Result<Expr, CompileError> {
        self.or()
    }

    fn or(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.and()?;
        while self.at(&Tok::OrOr) {
            let line = self.bump().line;
            let rhs = self.and()?;
            lhs = Expr {
                line,
                kind: ExprKind::Or(Box::new(lhs), Box::new(rhs)),
            };
        }
        Ok(lhs)
    }

    fn and(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.equality()?;
        while self.at(&Tok::AndAnd) {
            let line = self.bump().line;
            let rhs = self.equality()?;
            lhs = Expr {
                line,
                kind: ExprKind::And(Box::new(lhs), Box::new(rhs)),
            };
        }
        Ok(lhs)
    }

    fn binary_level(
        &mut self,
        next: fn(&mut Self) -> Result<Expr, CompileError>,
        ops: &[(Tok, BinOp)],
    ) -> Result<Expr, CompileError> {
        let mut lhs = next(self)?;
        'outer: loop {
            for (tok, op) in ops {
                if self.at(tok) {
                    let line = self.bump().line;
                    let rhs = next(self)?;
                    lhs = Expr {
                        line,
                        kind: ExprKind::Binary(*op, Box::new(lhs), Box::new(rhs)),
                    };
                    continue 'outer;
                }
            }
            return Ok(lhs);
        }
    }

    fn equality(&mut self) -> Result<Expr, CompileError> {
        self.binary_level(
            Self::comparison,
            &[(Tok::Eq, BinOp::Eq), (Tok::Ne, BinOp::Ne)],
        )
    }

    fn comparison(&mut self) -> Result<Expr, CompileError> {
        self.binary_level(
            Self::term,
            &[
                (Tok::Lt, BinOp::Lt),
                (Tok::Le, BinOp::Le),
                (Tok::Gt, BinOp::Gt),
                (Tok::Ge, BinOp::Ge),
            ],
        )
    }

    fn term(&mut self) -> Result<Expr, CompileError> {
        self.binary_level(
            Self::factor,
            &[(Tok::Plus, BinOp::Add), (Tok::Minus, BinOp::Sub)],
        )
    }

    fn factor(&mut self) -> Result<Expr, CompileError> {
        self.binary_level(
            Self::unary,
            &[
                (Tok::Star, BinOp::Mul),
                (Tok::Slash, BinOp::Div),
                (Tok::Percent, BinOp::Mod),
            ],
        )
    }

    fn unary(&mut self) -> Result<Expr, CompileError> {
        let line = self.peek().line;
        if self.eat(&Tok::Minus) {
            return Ok(Expr {
                line,
                kind: ExprKind::Unary(UnOp::Neg, Box::new(self.unary()?)),
            });
        }
        if self.eat(&Tok::Bang) {
            return Ok(Expr {
                line,
                kind: ExprKind::Unary(UnOp::Not, Box::new(self.unary()?)),
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, CompileError> {
        let mut e = self.primary()?;
        while self.at(&Tok::LBracket) {
            let line = self.bump().line;
            let idx = self.expr()?;
            self.expect(Tok::RBracket, "`]`")?;
            e = Expr {
                line,
                kind: ExprKind::Index(Box::new(e), Box::new(idx)),
            };
        }
        Ok(e)
    }

    fn args(&mut self) -> Result<Vec<Expr>, CompileError> {
        self.expect(Tok::LParen, "`(`")?;
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            args.push(self.expr()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "`)`")?;
        Ok(args)
    }

    fn primary(&mut self) -> Result<Expr, CompileError> {
        let t = self.peek().clone();
        let line = t.line;
        let kind = match t.tok {
            Tok::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Nil => {
                self.bump();
                ExprKind::Nil
            }
            Tok::Ident(name) => {
                self.bump();
                if self.at(&Tok::LParen) {
                    ExprKind::Call(name, self.args()?)
                } else {
                    ExprKind::Var(name)
                }
            }
            Tok::Spawn => {
                self.bump();
                let name = self.ident("function to spawn")?;
                ExprKind::Spawn(name, self.args()?)
            }
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
                self.expect(Tok::RParen, "`)`")?;
                return Ok(e);
            }
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                while !self.at(&Tok::RBracket) {
                    items.push(self.expr()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(Tok::RBracket, "`]`")?;
                ExprKind::Array(items)
            }
            other => {
                return self.err(format!(
                    "expected an expression, found {}",
                    describe(&other)
                ))
            }
        };
        Ok(Expr { kind, line })
    }
}

fn assignment(target: Expr, value: Expr) -> Result<StmtKind, CompileError> {
    match target.kind {
        ExprKind::Var(name) => Ok(StmtKind::Assign(name, value)),
        ExprKind::Index(arr, idx) => Ok(StmtKind::AssignIndex(*arr, *idx, value)),
        _ => Err(CompileError::new(
            target.line,
            1,
            "can only assign to a variable or an array element",
        )),
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(n) => format!("`{n}`"),
        Tok::Int(v) => format!("`{v}`"),
        Tok::Str(_) => "a string".into(),
        Tok::Eof => "end of file".into(),
        other => {
            let sym = match other {
                Tok::LParen => "(",
                Tok::RParen => ")",
                Tok::LBrace => "{",
                Tok::RBrace => "}",
                Tok::LBracket => "[",
                Tok::RBracket => "]",
                Tok::Comma => ",",
                Tok::Semi => ";",
                Tok::Assign => "=",
                Tok::Eq => "==",
                Tok::Ne => "!=",
                Tok::Lt => "<",
                Tok::Le => "<=",
                Tok::Gt => ">",
                Tok::Ge => ">=",
                Tok::Plus => "+",
                Tok::Minus => "-",
                Tok::Star => "*",
                Tok::Slash => "/",
                Tok::Percent => "%",
                Tok::Bang => "!",
                Tok::AndAnd => "&&",
                Tok::OrOr => "||",
                Tok::DotDot => "..",
                Tok::PlusAssign => "+=",
                Tok::MinusAssign => "-=",
                // keywords read fine lowercased
                kw => return format!("`{}`", format!("{kw:?}").to_lowercase()),
            };
            format!("`{sym}`")
        }
    }
}
