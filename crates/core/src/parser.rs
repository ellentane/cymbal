use crate::ast::{BindStmt, Combinator, Expr, LoopStmt, Note, Program, Stmt, VoiceKind};
use crate::error::{Error, ErrorKind, Result, Span};
use crate::lexer::{Token, TokenKind};

pub fn parse(tokens: &[Token]) -> Result<Program> {
    debug_assert!(
        !tokens.is_empty(),
        "parse requires lexer output ending in Eof"
    );
    Parser { tokens, pos: 0 }.parse_program()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }
    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }
    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.pos += 1;
        t
    }
    fn span(&self) -> Span {
        self.peek().span
    }

    fn skip_newlines(&mut self) {
        while self.peek_kind() == &TokenKind::Newline {
            self.pos += 1;
        }
    }

    fn parse_program(mut self) -> Result<Program> {
        let mut statements = Vec::new();
        self.skip_newlines();
        while self.peek_kind() != &TokenKind::Eof {
            let stmt = match self.peek_kind().clone() {
                TokenKind::Let => self.parse_let()?,
                TokenKind::Loop => Stmt::Loop(self.parse_loop()?),
                TokenKind::Tempo => {
                    let span = self.span();
                    self.advance();
                    let TokenKind::Number(n) = self.peek_kind().clone() else {
                        return Err(Error::new(
                            self.span(),
                            ErrorKind::Parse,
                            "expected tempo value",
                        ));
                    };
                    self.advance();
                    Stmt::Tempo(n, span)
                }
                _ => {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected statement",
                    ));
                }
            };
            statements.push(stmt);
            self.skip_newlines();
        }
        Ok(Program { statements })
    }

    fn parse_let(&mut self) -> Result<Stmt> {
        let span = self.span();
        self.advance(); // let
        let TokenKind::Ident(name) = self.peek_kind().clone() else {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "expected name after let",
            ));
        };
        self.advance();
        if self.peek_kind() != &TokenKind::Assign {
            return Err(Error::new(self.span(), ErrorKind::Parse, "expected '='"));
        }
        self.advance();
        let value = self.parse_expr()?;
        Ok(Stmt::Let { name, value, span })
    }

    fn parse_loop(&mut self) -> Result<LoopStmt> {
        let span = self.span();
        self.advance(); // loop
        let TokenKind::String(name) = self.peek_kind().clone() else {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "expected loop name string",
            ));
        };
        self.advance();
        let mut tempo = None;
        if self.peek_kind() == &TokenKind::Tempo {
            self.advance();
            if self.peek_kind() != &TokenKind::Assign {
                return Err(Error::new(
                    self.span(),
                    ErrorKind::Parse,
                    "expected '=' after tempo",
                ));
            }
            self.advance();
            let TokenKind::Number(n) = self.peek_kind().clone() else {
                return Err(Error::new(
                    self.span(),
                    ErrorKind::Parse,
                    "expected tempo value",
                ));
            };
            self.advance();
            tempo = Some(n);
        }
        if self.peek_kind() != &TokenKind::Colon {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "expected ':' after loop header",
            ));
        }
        self.advance();
        let mut binds = Vec::new();
        loop {
            match self.peek_kind() {
                TokenKind::Newline | TokenKind::Eof => {
                    self.skip_newlines();
                    if matches!(
                        self.peek_kind(),
                        TokenKind::Let | TokenKind::Loop | TokenKind::Tempo | TokenKind::Eof
                    ) {
                        break;
                    }
                }
                _ => binds.push(self.parse_bind()?),
            }
        }
        Ok(LoopStmt {
            name,
            tempo,
            binds,
            span,
        })
    }

    fn parse_bind(&mut self) -> Result<BindStmt> {
        let span = self.span();
        let voice = self.parse_expr()?;
        if self.peek_kind() != &TokenKind::Bind {
            return Err(Error::new(self.span(), ErrorKind::Parse, "expected '<<'"));
        }
        self.advance();
        let pattern = self.parse_expr()?;
        let mut combinators = Vec::new();
        while self.peek_kind() == &TokenKind::Pipe {
            self.advance();
            combinators.push(self.parse_combinator()?);
        }
        Ok(BindStmt {
            voice,
            pattern,
            combinators,
            span,
        })
    }

    fn parse_combinator(&mut self) -> Result<Combinator> {
        match self.peek_kind() {
            TokenKind::Rev => {
                self.advance();
                Ok(Combinator::Rev)
            }
            TokenKind::Every => {
                self.advance();
                if self.peek_kind() != &TokenKind::LParen {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected '(' after every",
                    ));
                }
                self.advance();
                let TokenKind::Number(n) = self.peek_kind().clone() else {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected number in every(n, ...)",
                    ));
                };
                if n < 1.0 || n.fract() != 0.0 {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "every() needs a positive integer",
                    ));
                }
                self.advance();
                if self.peek_kind() != &TokenKind::Comma {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected ',' in every(n, ...)",
                    ));
                }
                self.advance();
                if self.peek_kind() != &TokenKind::Rev {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "only every(n, rev) is supported",
                    ));
                }
                self.advance();
                if self.peek_kind() != &TokenKind::RParen {
                    return Err(Error::new(self.span(), ErrorKind::Parse, "expected ')'"));
                }
                self.advance();
                Ok(Combinator::Every(n as u64, Box::new(Combinator::Rev)))
            }
            _ => Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "expected combinator (rev or every(n, rev))",
            )),
        }
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        match self.peek_kind().clone() {
            TokenKind::Ident(name) => {
                self.advance();
                if self.peek_kind() == &TokenKind::LParen {
                    self.advance();
                    if self.peek_kind() != &TokenKind::RParen {
                        return Err(Error::new(self.span(), ErrorKind::Parse, "expected ')'"));
                    }
                    self.advance();
                }
                let kind = match name.as_str() {
                    "kick" => VoiceKind::Kick,
                    "snare" => VoiceKind::Snare,
                    "hat" => VoiceKind::Hat,
                    "bass" => VoiceKind::Bass,
                    "lead" => VoiceKind::Lead,
                    other => {
                        return Err(Error::new(
                            span,
                            ErrorKind::Parse,
                            format!("unknown voice '{other}'"),
                        ));
                    }
                };
                Ok(Expr::Voice(kind, span))
            }
            TokenKind::String(s) => {
                self.advance();
                Ok(Expr::PatternString(s, span))
            }
            TokenKind::LBracket => {
                self.advance();
                let mut notes = Vec::new();
                if self.peek_kind() == &TokenKind::RBracket {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "note array cannot be empty",
                    ));
                }
                loop {
                    let nspan = self.span();
                    let TokenKind::Note(midi) = self.peek_kind().clone() else {
                        return Err(Error::new(
                            self.span(),
                            ErrorKind::Parse,
                            "expected note in array",
                        ));
                    };
                    self.advance();
                    notes.push(Note { midi, span: nspan });
                    match self.peek_kind() {
                        TokenKind::Comma => {
                            self.advance();
                        }
                        TokenKind::RBracket => {
                            self.advance();
                            break;
                        }
                        _ => {
                            return Err(Error::new(
                                self.span(),
                                ErrorKind::Parse,
                                "expected ',' or ']'",
                            ));
                        }
                    }
                }
                Ok(Expr::Notes(notes, span))
            }
            TokenKind::LParen => {
                self.advance();
                if self.peek_kind() != &TokenKind::LBracket {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected note array in tuple",
                    ));
                }
                self.advance();
                let mut notes = Vec::new();
                if self.peek_kind() == &TokenKind::RBracket {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "tuple note array cannot be empty",
                    ));
                }
                loop {
                    let nspan = self.span();
                    let TokenKind::Note(midi) = self.peek_kind().clone() else {
                        return Err(Error::new(
                            self.span(),
                            ErrorKind::Parse,
                            "expected note in tuple",
                        ));
                    };
                    self.advance();
                    notes.push(Note { midi, span: nspan });
                    match self.peek_kind() {
                        TokenKind::Comma => {
                            self.advance();
                        }
                        TokenKind::RBracket => {
                            self.advance();
                            break;
                        }
                        _ => {
                            return Err(Error::new(
                                self.span(),
                                ErrorKind::Parse,
                                "expected ',' or ']'",
                            ));
                        }
                    }
                }
                if self.peek_kind() != &TokenKind::Comma {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected ',' after notes in tuple",
                    ));
                }
                self.advance();
                if !matches!(self.peek_kind(), TokenKind::String(_)) {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected pattern string in tuple",
                    ));
                }
                let TokenKind::String(s) = self.advance().kind else {
                    unreachable!()
                };
                if self.peek_kind() != &TokenKind::RParen {
                    return Err(Error::new(self.span(), ErrorKind::Parse, "expected ')'"));
                }
                self.advance();
                Ok(Expr::Tuple(notes, s, span))
            }
            _ => Err(Error::new(span, ErrorKind::Parse, "expected expression")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    #[test]
    fn parses_let_loop_tempo_and_binds() {
        let src = r#"
tempo 120
let kick = kick()
loop "beat" tempo=90:
    kick << "x . . x . . x ."
    lead << [c4, e4, g4] >> every(4, rev)
"#;
        let program = parse(&lex(src).unwrap()).unwrap();
        assert_eq!(program.statements.len(), 3);
        let Stmt::Tempo(t, _) = &program.statements[0] else {
            panic!("expected tempo")
        };
        assert_eq!(*t, 120.0);
        let Stmt::Let { name, value, .. } = &program.statements[1] else {
            panic!("expected let")
        };
        assert_eq!(name, "kick");
        assert_eq!(
            value,
            &Expr::Voice(VoiceKind::Kick, Span { line: 3, col: 12 })
        );
        let Stmt::Loop(loop_stmt) = &program.statements[2] else {
            panic!("expected loop")
        };
        assert_eq!(loop_stmt.name, "beat");
        assert_eq!(loop_stmt.tempo, Some(90.0));
        assert_eq!(loop_stmt.binds.len(), 2);
        assert_eq!(
            loop_stmt.binds[0].pattern,
            Expr::PatternString("x . . x . . x .".into(), Span { line: 5, col: 29 })
        );
        assert_eq!(
            loop_stmt.binds[1].combinators,
            vec![Combinator::Every(4, Box::new(Combinator::Rev))]
        );
    }

    #[test]
    fn parses_tuple_pattern() {
        let src = "loop \"a\":\n    bass << ([c2, f2], \"x . x .\")\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert_eq!(
            l.binds[0].pattern,
            Expr::Tuple(
                vec![
                    Note {
                        midi: 36,
                        span: Span { line: 2, col: 15 }
                    },
                    Note {
                        midi: 41,
                        span: Span { line: 2, col: 19 }
                    }
                ],
                "x . x .".into(),
                Span { line: 2, col: 13 }
            )
        );
    }

    #[test]
    fn parse_error_carries_span() {
        let err = parse(&lex("loop \"a\":\n    kick <<\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert_eq!(err.span.line, 2);
    }

    #[test]
    fn bind_outside_loop_is_parse_error() {
        let err = parse(&lex("kick << \"x\"\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn missing_loop_name_is_parse_error() {
        let err = parse(&lex("loop:\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn comment_after_bind_does_not_break_loop_body() {
        let src = "loop \"a\":\n    kick << \"x\" -- a comment\nlet y = kick()\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        assert_eq!(program.statements.len(), 2);
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!("expected loop")
        };
        assert_eq!(l.binds.len(), 1);
        let Stmt::Let { name, .. } = &program.statements[1] else {
            panic!("expected let")
        };
        assert_eq!(name, "y");
    }
}
