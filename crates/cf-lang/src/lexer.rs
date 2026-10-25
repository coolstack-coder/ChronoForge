use crate::CompileError;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Str(String),
    // keywords
    Let,
    Fn,
    If,
    Else,
    While,
    For,
    In,
    Return,
    Spawn,
    Break,
    Continue,
    True,
    False,
    Nil,
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Assign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    AndAnd,
    OrOr,
    DotDot,
    PlusAssign,
    MinusAssign,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
}

pub fn lex(src: &str) -> Result<Vec<Token>, CompileError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1u32, 1u32);

    macro_rules! push {
        ($t:expr, $len:expr) => {{
            out.push(Token { tok: $t, line, col });
            i += $len;
            col += $len as u32;
        }};
    }

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\n' => {
                i += 1;
                line += 1;
                col = 1;
            }
            c if c.is_whitespace() => {
                i += 1;
                col += 1;
            }
            '/' if next == Some('/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '0'..='9' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().filter(|c| **c != '_').collect();
                let value = text.parse::<i64>().map_err(|_| {
                    CompileError::new(line, col, format!("integer literal `{text}` is too large"))
                })?;
                out.push(Token {
                    tok: Tok::Int(value),
                    line,
                    col,
                });
                col += (i - start) as u32;
            }
            '"' => {
                let (start_line, start_col) = (line, col);
                let mut s = String::new();
                i += 1;
                col += 1;
                loop {
                    match chars.get(i) {
                        None | Some('\n') => {
                            return Err(CompileError::new(
                                start_line,
                                start_col,
                                "unterminated string literal",
                            ))
                        }
                        Some('"') => {
                            i += 1;
                            col += 1;
                            break;
                        }
                        Some('\\') => {
                            let esc = chars.get(i + 1).copied();
                            s.push(match esc {
                                Some('n') => '\n',
                                Some('t') => '\t',
                                Some('"') => '"',
                                Some('\\') => '\\',
                                _ => {
                                    return Err(CompileError::new(
                                        line,
                                        col,
                                        "unknown escape sequence",
                                    ))
                                }
                            });
                            i += 2;
                            col += 2;
                        }
                        Some(&ch) => {
                            s.push(ch);
                            i += 1;
                            col += 1;
                        }
                    }
                }
                out.push(Token {
                    tok: Tok::Str(s),
                    line: start_line,
                    col: start_col,
                });
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let tok = match word.as_str() {
                    "let" => Tok::Let,
                    "fn" => Tok::Fn,
                    "if" => Tok::If,
                    "else" => Tok::Else,
                    "while" => Tok::While,
                    "for" => Tok::For,
                    "in" => Tok::In,
                    "return" => Tok::Return,
                    "spawn" => Tok::Spawn,
                    "break" => Tok::Break,
                    "continue" => Tok::Continue,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "nil" => Tok::Nil,
                    _ => Tok::Ident(word),
                };
                out.push(Token { tok, line, col });
                col += (i - start) as u32;
            }
            _ => {
                let two = next.map(|n| (c, n));
                match two {
                    Some(('=', '=')) => push!(Tok::Eq, 2),
                    Some(('!', '=')) => push!(Tok::Ne, 2),
                    Some(('<', '=')) => push!(Tok::Le, 2),
                    Some(('>', '=')) => push!(Tok::Ge, 2),
                    Some(('&', '&')) => push!(Tok::AndAnd, 2),
                    Some(('|', '|')) => push!(Tok::OrOr, 2),
                    Some(('.', '.')) => push!(Tok::DotDot, 2),
                    Some(('+', '=')) => push!(Tok::PlusAssign, 2),
                    Some(('-', '=')) => push!(Tok::MinusAssign, 2),
                    _ => match c {
                        '(' => push!(Tok::LParen, 1),
                        ')' => push!(Tok::RParen, 1),
                        '{' => push!(Tok::LBrace, 1),
                        '}' => push!(Tok::RBrace, 1),
                        '[' => push!(Tok::LBracket, 1),
                        ']' => push!(Tok::RBracket, 1),
                        ',' => push!(Tok::Comma, 1),
                        ';' => push!(Tok::Semi, 1),
                        '=' => push!(Tok::Assign, 1),
                        '<' => push!(Tok::Lt, 1),
                        '>' => push!(Tok::Gt, 1),
                        '+' => push!(Tok::Plus, 1),
                        '-' => push!(Tok::Minus, 1),
                        '*' => push!(Tok::Star, 1),
                        '/' => push!(Tok::Slash, 1),
                        '%' => push!(Tok::Percent, 1),
                        '!' => push!(Tok::Bang, 1),
                        other => {
                            return Err(CompileError::new(
                                line,
                                col,
                                format!("unexpected character `{other}`"),
                            ))
                        }
                    },
                }
            }
        }
    }
    out.push(Token {
        tok: Tok::Eof,
        line,
        col,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_a_line() {
        let toks: Vec<Tok> = lex("let x = 1_000 + y; // hi\n\"a\\n\"")
            .unwrap()
            .into_iter()
            .map(|t| t.tok)
            .collect();
        assert_eq!(
            toks,
            vec![
                Tok::Let,
                Tok::Ident("x".into()),
                Tok::Assign,
                Tok::Int(1000),
                Tok::Plus,
                Tok::Ident("y".into()),
                Tok::Semi,
                Tok::Str("a\n".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn reports_position_of_bad_char() {
        let err = lex("let x = 1;\n  @").unwrap_err();
        assert_eq!((err.line, err.col), (2, 3));
    }
}
