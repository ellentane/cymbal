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
        let mut pan = None;
        let mut vel = None;
        let mut delay_send = None;
        let mut reverb_send = None;
        while let TokenKind::Ident(name) = self.peek_kind().clone() {
            let param = match name.as_str() {
                "pan" | "vel" | "delay" | "reverb" => {
                    let pspan = self.span();
                    self.advance();
                    if self.peek_kind() != &TokenKind::Assign {
                        return Err(Error::new(
                            self.span(),
                            ErrorKind::Parse,
                            "expected '=' after parameter",
                        ));
                    }
                    self.advance();
                    let TokenKind::Number(n) = self.peek_kind().clone() else {
                        return Err(Error::new(
                            self.span(),
                            ErrorKind::Parse,
                            "expected number after parameter",
                        ));
                    };
                    self.advance();
                    (name.as_str().to_string(), n, pspan)
                }
                _ => break,
            };
            let (name, n, pspan) = param;
            let slot = match name.as_str() {
                "pan" => &mut pan,
                "vel" => &mut vel,
                "delay" => &mut delay_send,
                "reverb" => &mut reverb_send,
                _ => unreachable!(),
            };
            if slot.is_some() {
                return Err(Error::new(
                    pspan,
                    ErrorKind::Parse,
                    format!("duplicate parameter '{name}'"),
                ));
            }
            *slot = Some(n as f32);
        }
        if self.peek_kind() == &TokenKind::Pipe {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "parameters must come after combinators",
            ));
        }
        if let Some(p) = pan
            && !(-1.0..=1.0).contains(&p)
        {
            return Err(Error::new(span, ErrorKind::Parse, "pan must be in -1..=1"));
        }
        for (name, v) in [("vel", vel), ("delay", delay_send), ("reverb", reverb_send)] {
            if let Some(v) = v
                && !(0.0..=1.0).contains(&v)
            {
                return Err(Error::new(
                    span,
                    ErrorKind::Parse,
                    format!("{name} must be in 0..=1"),
                ));
            }
        }
        Ok(BindStmt {
            voice,
            pattern,
            combinators,
            pan,
            vel,
            delay_send,
            reverb_send,
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
                if name == "sample" {
                    let TokenKind::String(path) = self.peek_kind().clone() else {
                        return Err(Error::new(
                            self.span(),
                            ErrorKind::Parse,
                            "expected path string after 'sample'",
                        ));
                    };
                    self.advance();
                    return Ok(Expr::Sample(path, span));
                }
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

    #[test]
    fn parses_sample_voice_and_params() {
        let src = "loop \"a\":\n    sample \"kick.wav\" << \"x . x .\" pan=0.7 vel=0.9 delay=0.3 reverb=0.2\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert_eq!(
            l.binds[0].voice,
            Expr::Sample("kick.wav".into(), Span { line: 2, col: 5 })
        );
        assert_eq!(l.binds[0].pan, Some(0.7));
        assert_eq!(l.binds[0].vel, Some(0.9));
        assert_eq!(l.binds[0].delay_send, Some(0.3));
        assert_eq!(l.binds[0].reverb_send, Some(0.2));
    }

    #[test]
    fn params_after_combinators() {
        let src = "loop \"a\":\n    kick << \"x . x .\" >> every(2, rev) pan=-0.5\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert_eq!(l.binds[0].combinators.len(), 1);
        assert_eq!(l.binds[0].pan, Some(-0.5));
    }

    #[test]
    fn duplicate_param_is_error() {
        let err =
            parse(&lex("loop \"a\":\n    kick << \"x\" pan=0.5 pan=0.6\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert_eq!(err.message, "duplicate parameter 'pan'");
    }

    #[test]
    fn out_of_range_params_are_errors() {
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" pan=2.0\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" vel=1.5\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" delay=-0.1\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
    }

    #[test]
    fn unknown_param_is_parse_error() {
        let err = parse(&lex("loop \"a\":\n    kick << \"x\" foo=1\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn sample_missing_path_is_parse_error() {
        let err = parse(&lex("loop \"a\":\n    sample << \"x\"\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn param_without_value_is_parse_error() {
        let err = parse(&lex("loop \"a\":\n    kick << \"x\" pan=\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn param_boundary_values_are_accepted() {
        for src in [
            "loop \"a\":\n    kick << \"x\" pan=1.0\n",
            "loop \"a\":\n    kick << \"x\" pan=-1.0\n",
            "loop \"a\":\n    kick << \"x\" vel=1.0\n",
            "loop \"a\":\n    kick << \"x\" reverb=0\n",
        ] {
            let program = parse(&lex(src).unwrap()).unwrap();
            let Stmt::Loop(l) = &program.statements[0] else {
                panic!()
            };
            assert_eq!(l.binds.len(), 1, "source: {src}");
        }
    }

    #[test]
    fn params_before_combinator_is_parse_error() {
        let err =
            parse(&lex("loop \"a\":\n    kick << \"x\" pan=0.5 >> rev\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert_eq!(err.message, "parameters must come after combinators");
    }
}
