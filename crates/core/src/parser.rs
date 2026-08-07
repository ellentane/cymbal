use crate::ast::{BindStmt, Combinator, Expr, LoopStmt, Note, Param, Program, Stmt, VoiceKind};
use crate::docs::{PARAM_NAMES, VOICE_NAMES, nearest};
use crate::error::{Error, ErrorKind, Result, Span};
use crate::lexer::{Token, TokenKind};

fn param_range(p: Option<Param>, lo: f32, hi: f32) -> bool {
    match p {
        None => true,
        Some(Param::Const(v)) => (lo..=hi).contains(&v),
        Some(Param::Ramp(a, b)) => (lo..=hi).contains(&a) && (lo..=hi).contains(&b),
    }
}

fn builtin_voice(name: &str) -> Option<VoiceKind> {
    match name {
        "kick" => Some(VoiceKind::Kick),
        "snare" => Some(VoiceKind::Snare),
        "hat" => Some(VoiceKind::Hat),
        "bass" => Some(VoiceKind::Bass),
        "lead" => Some(VoiceKind::Lead),
        _ => None,
    }
}

pub fn parse(tokens: &[Token]) -> Result<Program> {
    debug_assert!(
        !tokens.is_empty(),
        "parse requires lexer output ending in Eof"
    );
    let program = Parser { tokens, pos: 0 }.parse_program()?;
    resolve_names(program)
}

fn resolve_names(program: Program) -> Result<Program> {
    use std::collections::HashMap;
    let mut raw: HashMap<String, Expr> = HashMap::new();
    let mut declared: Vec<String> = Vec::new();
    for stmt in &program.statements {
        if let Stmt::Let { name, value, .. } = stmt {
            raw.insert(name.clone(), value.clone());
            declared.push(name.clone());
        }
    }
    let candidates: Vec<&str> = VOICE_NAMES
        .iter()
        .copied()
        .chain(declared.iter().map(|s| s.as_str()))
        .collect();

    fn resolve_one(
        name: &str,
        raw: &HashMap<String, Expr>,
        candidates: &[&str],
        stack: &mut Vec<String>,
        span: Span,
    ) -> Result<Expr> {
        match raw.get(name) {
            Some(Expr::Name(inner, inner_span)) => {
                if stack.contains(&name.to_string()) {
                    return Err(Error::new(
                        *inner_span,
                        ErrorKind::Parse,
                        format!("cycle in let names: {}", stack.join(" -> ")),
                    ));
                }
                stack.push(name.to_string());
                let r = resolve_one(inner, raw, candidates, stack, *inner_span);
                stack.pop();
                r
            }
            Some(other) => Ok(other.clone()),
            None => match builtin_voice(name) {
                Some(kind) => Ok(Expr::Voice(kind, Span { line: 1, col: 1 })),
                None => Err(
                    Error::new(span, ErrorKind::Parse, format!("unknown voice '{name}'"))
                        .with_hint(
                            nearest(candidates, name)
                                .map(|n| format!("did you mean '{n}'?"))
                                .unwrap_or_else(|| {
                                    "declare it with let, e.g. let kick = kick()".to_string()
                                }),
                        ),
                ),
            },
        }
    }

    for stmt in &program.statements {
        if let Stmt::Let { name, value, .. } = stmt {
            let span = match value {
                Expr::Name(_, s) => *s,
                _ => Span { line: 1, col: 1 },
            };
            resolve_one(name, &raw, &candidates, &mut Vec::new(), span)?;
        }
    }

    let mut statements = Vec::new();
    for stmt in program.statements {
        match stmt {
            Stmt::Loop(mut l) => {
                for bind in &mut l.binds {
                    if let Expr::Name(n, span) = &bind.voice {
                        let n = n.clone();
                        let span = *span;
                        bind.voice = resolve_one(&n, &raw, &candidates, &mut Vec::new(), span)?;
                    }
                    if let Expr::Name(n, span) = &bind.pattern {
                        return Err(Error::new(
                            *span,
                            ErrorKind::Parse,
                            format!("'{n}' is not a pattern"),
                        )
                        .with_hint(
                            "patterns are quoted strings or note arrays: kick << \"x\" or [c4, e4]",
                        ));
                    }
                }
                statements.push(Stmt::Loop(l));
            }
            other => statements.push(other),
        }
    }
    Ok(Program { statements })
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

    fn parse_param_value(&mut self) -> Result<Param> {
        let TokenKind::Number(a) = self.peek_kind().clone() else {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "expected number after parameter",
            ));
        };
        self.advance();
        match self.peek_kind() {
            TokenKind::DotDot => {
                self.advance();
                let TokenKind::Number(b) = self.peek_kind().clone() else {
                    return Err(Error::new(
                        self.span(),
                        ErrorKind::Parse,
                        "expected ramp end after '..'",
                    )
                    .with_hint("ramps have two endpoints: pan=0.5..0.6"));
                };
                self.advance();
                Ok(Param::Ramp(a as f32, b as f32))
            }
            TokenKind::Colon => {
                Err(
                    Error::new(self.span(), ErrorKind::Parse, "ramp uses '..' now")
                        .with_hint("write pan=-0.5..0.5 instead of pan=-0.5:0.5"),
                )
            }
            _ => Ok(Param::Const(a as f32)),
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
                TokenKind::Help => self.parse_help()?,
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

    fn parse_help(&mut self) -> Result<Stmt> {
        let span = self.span();
        self.advance(); // help
        let topic = match self.peek_kind().clone() {
            TokenKind::Ident(name) | TokenKind::String(name) => {
                self.advance();
                Some(name)
            }
            TokenKind::Rev => {
                self.advance();
                Some("rev".to_string())
            }
            TokenKind::Every => {
                self.advance();
                Some("every".to_string())
            }
            TokenKind::Let => {
                self.advance();
                Some("let".to_string())
            }
            TokenKind::Loop => {
                self.advance();
                Some("loop".to_string())
            }
            TokenKind::Tempo => {
                self.advance();
                Some("tempo".to_string())
            }
            TokenKind::Help => {
                self.advance();
                Some("help".to_string())
            }
            TokenKind::Pipe => {
                self.advance();
                Some("|".to_string())
            }
            TokenKind::Bind => {
                self.advance();
                Some("<<".to_string())
            }
            TokenKind::DotDot => {
                self.advance();
                Some("..".to_string())
            }
            TokenKind::Newline | TokenKind::Eof => None,
            _ => {
                return Err(
                    Error::new(self.span(), ErrorKind::Parse, "expected a help topic")
                        .with_hint("write help pan, help rev, help |, or quote glyphs: help \"*\""),
                );
            }
        };
        Ok(Stmt::Help(topic, span))
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
        if matches!(voice, Expr::Notes(..)) {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "a note array can't be a voice",
            )
            .with_hint("notes come first after '<<': lead << [c2, f2] \"x . x .\""));
        }
        if matches!(voice, Expr::Name(..)) && matches!(self.peek_kind(), TokenKind::String(_)) {
            return Err(Error::new(self.span(), ErrorKind::Parse, "expected '<<'")
                .with_hint("did you mean 'sample'? write sample \"path\" <<"));
        }
        if self.peek_kind() != &TokenKind::Bind {
            return Err(Error::new(self.span(), ErrorKind::Parse, "expected '<<'"));
        }
        self.advance();
        let pattern = self.parse_pattern()?;
        let mut combinators = Vec::new();
        while self.peek_kind() == &TokenKind::Pipe {
            self.advance();
            combinators.push(self.parse_combinator()?);
        }
        if matches!(pattern, Expr::PatternString(..))
            && matches!(self.peek_kind(), TokenKind::LBracket)
        {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "rhythm string can't come before notes",
            )
            .with_hint("notes come first: lead << [c2, f2] \"x . x .\""));
        }
        let mut pan = None;
        let mut vel = None;
        let mut delay_send = None;
        let mut reverb_send = None;
        let mut bass = None;
        let mut treble = None;
        let mut comp = None;
        let mut swing = None;
        let mut start = None;
        let mut end = None;
        let mut dur = None;
        let mut cycle = None;
        while let TokenKind::Ident(name) = self.peek_kind().clone() {
            let slot = match name.as_str() {
                "pan" => &mut pan,
                "vel" => &mut vel,
                "delay" => &mut delay_send,
                "reverb" => &mut reverb_send,
                "bass" => &mut bass,
                "treble" => &mut treble,
                "comp" => &mut comp,
                "swing" => &mut swing,
                "start" => &mut start,
                "end" => &mut end,
                "dur" => &mut dur,
                "cycle" => &mut cycle,
                _ => break,
            };
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
            let value = self.parse_param_value()?;
            if slot.is_some() {
                return Err(Error::new(
                    pspan,
                    ErrorKind::Parse,
                    format!("duplicate parameter '{name}'"),
                ));
            }
            *slot = Some(value);
        }
        if self.peek_kind() == &TokenKind::DotDot {
            return Err(Error::new(self.span(), ErrorKind::Parse, "unexpected '..'")
                .with_hint("ramps have two endpoints: pan=0.5..0.6"));
        }
        if self.peek_kind() == &TokenKind::Pipe {
            return Err(Error::new(
                self.span(),
                ErrorKind::Parse,
                "parameters must come after combinators",
            ));
        }
        if !param_range(swing, 0.0, 0.5) {
            return Err(Error::new(
                span,
                ErrorKind::Parse,
                "swing must be in 0..=0.5",
            ));
        }
        if !param_range(pan, -1.0, 1.0) {
            return Err(Error::new(span, ErrorKind::Parse, "pan must be in -1..=1"));
        }
        for (name, v) in [
            ("vel", vel),
            ("delay", delay_send),
            ("reverb", reverb_send),
            ("bass", bass),
            ("treble", treble),
            ("comp", comp),
            ("start", start),
            ("end", end),
            ("cycle", cycle),
        ] {
            if !param_range(v, 0.0, 1.0) {
                return Err(Error::new(
                    span,
                    ErrorKind::Parse,
                    format!("{name} must be in 0..=1"),
                ));
            }
        }
        if !param_range(dur, 0.0, 60.0) {
            return Err(Error::new(span, ErrorKind::Parse, "dur must be in 0..=60"));
        }
        if matches!(dur, Some(Param::Const(0.0))) {
            return Err(Error::new(span, ErrorKind::Parse, "dur must be > 0"));
        }
        for (name, v) in [
            ("bass", bass),
            ("treble", treble),
            ("comp", comp),
            ("swing", swing),
            ("start", start),
            ("end", end),
            ("dur", dur),
            ("cycle", cycle),
        ] {
            if matches!(v, Some(Param::Ramp(..))) {
                return Err(Error::new(
                    span,
                    ErrorKind::Parse,
                    format!("{name} does not support ramps"),
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
            bass,
            treble,
            comp,
            swing,
            start,
            end,
            dur,
            cycle,
            span,
        })
    }

    fn parse_pattern(&mut self) -> Result<Expr> {
        let expr = self.parse_expr()?;
        if let Expr::Notes(notes, span) = &expr
            && let TokenKind::String(rhythm) = self.peek_kind().clone()
        {
            self.advance();
            return Ok(Expr::Tuple(notes.clone(), rhythm, *span));
        }
        Ok(expr)
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
                        )
                        .with_hint("'sample' is reserved: sample \"kick\""));
                    };
                    self.advance();
                    if path.starts_with('/') || path.split('/').any(|c| c == "..") {
                        return Err(Error::new(
                            span,
                            ErrorKind::Parse,
                            "sample path must be relative and must not contain '..'",
                        ));
                    }
                    return Ok(Expr::Sample(path, span));
                }
                if self.peek_kind() == &TokenKind::LParen {
                    self.advance();
                    if self.peek_kind() != &TokenKind::RParen {
                        return Err(Error::new(self.span(), ErrorKind::Parse, "expected ')'"));
                    }
                    self.advance();
                    return match builtin_voice(&name) {
                        Some(kind) => Ok(Expr::Voice(kind, span)),
                        None => {
                            let mut err = Error::new(
                                span,
                                ErrorKind::Parse,
                                format!("unknown voice '{name}'"),
                            );
                            if let Some(close) = nearest(VOICE_NAMES, &name) {
                                err = err.with_hint(format!("did you mean '{close}'?"));
                            }
                            Err(err)
                        }
                    };
                }
                if self.peek_kind() == &TokenKind::Assign {
                    let mut err = Error::new(
                        span,
                        ErrorKind::Parse,
                        format!("unknown parameter '{name}'"),
                    );
                    if let Some(close) = nearest(PARAM_NAMES, &name) {
                        err = err.with_hint(format!("did you mean '{close}'?"));
                    }
                    return Err(err);
                }
                Ok(Expr::Name(name, span))
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
            TokenKind::LParen => Err(Error::new(
                span,
                ErrorKind::Parse,
                "parens are gone: write [c2, f2] \"x . . .\"",
            )
            .with_hint("put the notes and the rhythm string after '<<' without parentheses")),
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
    lead << [c4, e4, g4] | every(4, rev)
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
    fn parses_notes_with_rhythm_without_parens() {
        let src = "loop \"a\":\n    bass << [c2, f2] \"x . x .\"\n";
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
                        span: Span { line: 2, col: 14 }
                    },
                    Note {
                        midi: 41,
                        span: Span { line: 2, col: 18 }
                    },
                ],
                "x . x .".into(),
                Span { line: 2, col: 13 }
            )
        );
    }

    #[test]
    fn tuple_parens_rejected_with_hint() {
        let err =
            parse(&lex("loop \"a\":\n    bass << ([c2, f2], \"x . x .\")\n").unwrap()).unwrap_err();
        assert!(err.message.contains("parens"));
        assert!(err.hint.is_some());
    }

    #[test]
    fn notes_on_left_of_bind_rejected_with_hint() {
        let err = parse(&lex("loop \"a\":\n    [c2, f2] << \"x . x .\"\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.hint.is_some());
    }

    #[test]
    fn rhythm_first_rejected_with_hint() {
        let err =
            parse(&lex("loop \"a\":\n    kick << \"x . x .\" [c2, f2]\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.message.contains("before notes") || err.message.contains("first"));
        assert!(err.hint.is_some());
    }

    #[test]
    fn unknown_param_suggests_closest_name() {
        let err = parse(&lex("loop \"a\":\n    kick << \"x\" vol=0.9\n").unwrap()).unwrap_err();
        assert!(err.message.contains("vol"));
        assert!(err.hint.as_deref().unwrap().contains("vel"));
    }

    #[test]
    fn notes_followed_by_pipe_keep_no_rhythm() {
        let src = "loop \"a\":\n    lead << [c4, d4] | rev\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert!(matches!(l.binds[0].pattern, Expr::Notes(..)));
        assert_eq!(l.binds[0].combinators.len(), 1);
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
    fn parses_help_statements() {
        let program = parse(&lex("help pan\nhelp\nhelp rev\nhelp |\nhelp <<\nhelp \"*\"\nhelp ..\nhelp tempo\nhelp let\nhelp help\n").unwrap()).unwrap();
        assert_eq!(
            program.statements,
            vec![
                Stmt::Help(Some("pan".into()), Span { line: 1, col: 1 }),
                Stmt::Help(None, Span { line: 2, col: 1 }),
                Stmt::Help(Some("rev".into()), Span { line: 3, col: 1 }),
                Stmt::Help(Some("|".into()), Span { line: 4, col: 1 }),
                Stmt::Help(Some("<<".into()), Span { line: 5, col: 1 }),
                Stmt::Help(Some("*".into()), Span { line: 6, col: 1 }),
                Stmt::Help(Some("..".into()), Span { line: 7, col: 1 }),
                Stmt::Help(Some("tempo".into()), Span { line: 8, col: 1 }),
                Stmt::Help(Some("let".into()), Span { line: 9, col: 1 }),
                Stmt::Help(Some("help".into()), Span { line: 10, col: 1 }),
            ]
        );
    }

    #[test]
    fn help_with_invalid_topic_is_an_error() {
        let err = parse(&lex("help 5\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.hint.is_some());
    }

    #[test]
    fn help_inside_loop_body_is_an_error() {
        let err = parse(&lex("loop \"a\":\n    help pan\n").unwrap()).unwrap_err();
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
        assert_eq!(l.binds[0].pan, Some(Param::Const(0.7)));
        assert_eq!(l.binds[0].vel, Some(Param::Const(0.9)));
        assert_eq!(l.binds[0].delay_send, Some(Param::Const(0.3)));
        assert_eq!(l.binds[0].reverb_send, Some(Param::Const(0.2)));
    }

    #[test]
    fn params_after_combinators() {
        let src = "loop \"a\":\n    kick << \"x . x .\" | every(2, rev) pan=-0.5\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert_eq!(l.binds[0].combinators.len(), 1);
        assert_eq!(l.binds[0].pan, Some(Param::Const(-0.5)));
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
    fn sample_paths_cannot_escape_base_dir() {
        for bad in ["/etc/hostname", "../x.wav", "a/../b.wav"] {
            let src = format!("loop \"a\":\n    sample \"{bad}\" << \"x\"\n");
            let err = parse(&lex(&src).unwrap()).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Parse, "path '{bad}' must be rejected");
            assert!(
                err.message.contains(".."),
                "path '{bad}' error must explain: {}",
                err.message
            );
        }
        for good in ["kick", "sub/kick.wav"] {
            let src = format!("loop \"a\":\n    sample \"{good}\" << \"x\"\n");
            let program = parse(&lex(&src).unwrap()).unwrap();
            let Stmt::Loop(l) = &program.statements[0] else {
                panic!()
            };
            assert_eq!(
                l.binds[0].voice,
                Expr::Sample(good.into(), Span { line: 2, col: 5 })
            );
        }
    }

    #[test]
    fn param_without_value_is_parse_error() {
        let err = parse(&lex("loop \"a\":\n    kick << \"x\" pan=\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn param_boundary_values_are_accepted() {
        for (src, field, expected) in [
            (
                "loop \"a\":\n    kick << \"x\" pan=1.0\n",
                "pan",
                Param::Const(1.0),
            ),
            (
                "loop \"a\":\n    kick << \"x\" pan=-1.0\n",
                "pan",
                Param::Const(-1.0),
            ),
            (
                "loop \"a\":\n    kick << \"x\" vel=1.0\n",
                "vel",
                Param::Const(1.0),
            ),
            (
                "loop \"a\":\n    kick << \"x\" reverb=0\n",
                "reverb",
                Param::Const(0.0),
            ),
            (
                "loop \"a\":\n    sample \"k.wav\" << \"x\" cycle=0\n",
                "cycle",
                Param::Const(0.0),
            ),
        ] {
            let program = parse(&lex(src).unwrap()).unwrap();
            let Stmt::Loop(l) = &program.statements[0] else {
                panic!()
            };
            assert_eq!(l.binds.len(), 1, "source: {src}");
            let value = match field {
                "pan" => l.binds[0].pan,
                "vel" => l.binds[0].vel,
                "reverb" => l.binds[0].reverb_send,
                "cycle" => l.binds[0].cycle,
                _ => unreachable!(),
            };
            assert_eq!(value, Some(expected), "source: {src}");
        }
    }

    #[test]
    fn params_before_combinator_is_parse_error() {
        let err =
            parse(&lex("loop \"a\":\n    kick << \"x\" pan=0.5 | rev\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert_eq!(err.message, "parameters must come after combinators");
    }

    #[test]
    fn old_colon_ramp_rejected_with_hint() {
        let err = parse(&lex("loop \"a\":\n    kick << \"x\" pan=0:1\n").unwrap()).unwrap_err();
        assert!(err.message.contains(".."));
        assert!(err.hint.is_some());
    }

    #[test]
    fn spaced_ramp_parses() {
        let src = "loop \"a\":\n    kick << \"x\" pan=0.5 .. 0.6\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert_eq!(l.binds[0].pan, Some(Param::Ramp(0.5, 0.6)));
    }

    #[test]
    fn triple_dotdot_is_an_error_with_hint() {
        let err =
            parse(&lex("loop \"a\":\n    kick << \"x\" pan=0.5..0.5..0.5\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.hint.is_some());
    }

    #[test]
    fn parses_ramp_params() {
        let src =
            "loop \"a\":\n    kick << \"x . x .\" pan=0..1 vel=1..0 delay=0.2..0.5 reverb=0..0.1\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert_eq!(l.binds[0].pan, Some(Param::Ramp(0.0, 1.0)));
        assert_eq!(l.binds[0].vel, Some(Param::Ramp(1.0, 0.0)));
        assert_eq!(l.binds[0].delay_send, Some(Param::Ramp(0.2, 0.5)));
        assert_eq!(l.binds[0].reverb_send, Some(Param::Ramp(0.0, 0.1)));
    }

    #[test]
    fn parses_tone_params() {
        let src = "loop \"a\":\n    kick << \"x\" bass=0.5 treble=1 comp=0.25 swing=0.3\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert_eq!(l.binds[0].bass, Some(Param::Const(0.5)));
        assert_eq!(l.binds[0].treble, Some(Param::Const(1.0)));
        assert_eq!(l.binds[0].comp, Some(Param::Const(0.25)));
        assert_eq!(l.binds[0].swing, Some(Param::Const(0.3)));
    }

    #[test]
    fn ramp_endpoints_validated() {
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" pan=0..2\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" vel=1..-0.5\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" pan=0..1..2\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
    }

    #[test]
    fn tone_params_reject_ramps_and_ranges() {
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" bass=0..1\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" comp=1.5\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    kick << \"x\" swing=0.6\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
    }

    #[test]
    fn swing_ramp_is_parse_error() {
        let err = parse(&lex("loop \"a\":\n    kick << \"x\" swing=0..1\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.message.contains("swing"));
    }

    #[test]
    fn parses_sample_region_params() {
        let src =
            "loop \"a\":\n    sample \"kick.wav\" << \"x\" start=0.25 end=0.75 dur=0.2 cycle=1\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        let b = &l.binds[0];
        assert_eq!(b.start, Some(Param::Const(0.25)));
        assert_eq!(b.end, Some(Param::Const(0.75)));
        assert_eq!(b.dur, Some(Param::Const(0.2)));
        assert_eq!(b.cycle, Some(Param::Const(1.0)));
    }

    #[test]
    fn let_names_resolve_to_sample_voices() {
        let src = "let clap = sample \"clap\"\nloop \"a\":\n    clap << \"x . x .\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[1] else {
            panic!()
        };
        assert_eq!(
            l.binds[0].voice,
            Expr::Sample("clap".into(), Span { line: 1, col: 12 })
        );
    }

    #[test]
    fn let_names_resolve_to_builtin_voices() {
        let src = "let k = kick()\nloop \"a\":\n    k << \"x\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[1] else {
            panic!()
        };
        assert!(matches!(l.binds[0].voice, Expr::Voice(VoiceKind::Kick, _)));
    }

    #[test]
    fn declared_names_win_over_builtins() {
        let src = "let kick = lead()\nloop \"a\":\n    kick << \"x\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[1] else {
            panic!()
        };
        assert!(matches!(l.binds[0].voice, Expr::Voice(VoiceKind::Lead, _)));
    }

    #[test]
    fn let_chains_resolve() {
        let src = "let a = b\nlet b = kick()\nloop \"l\":\n    a << \"x\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[2] else {
            panic!()
        };
        assert!(matches!(l.binds[0].voice, Expr::Voice(VoiceKind::Kick, _)));
    }

    #[test]
    fn bare_alias_of_redeclared_builtin_resolves_declared() {
        let src = "let kick = lead()\nlet bass = kick\nloop \"l\":\n    bass << \"x\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[2] else {
            panic!()
        };
        assert!(matches!(l.binds[0].voice, Expr::Voice(VoiceKind::Lead, _)));
    }

    #[test]
    fn let_cycles_are_errors() {
        let src = "let a = b\nlet b = a\nloop \"l\":\n    a << \"x\"\n";
        let err = parse(&lex(src).unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.message.contains("cycle"));
    }

    #[test]
    fn bare_self_alias_is_cycle_error() {
        let err = parse(&lex("let kick = kick\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.message.contains("cycle in let names: kick"));
    }

    #[test]
    fn unused_cycles_are_errors() {
        let src = "let a = b\nlet b = a\n";
        let err = parse(&lex(src).unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.message.contains("cycle"));
    }

    #[test]
    fn redeclaration_last_wins() {
        let src = "let a = kick()\nlet a = lead()\nloop \"l\":\n    a << \"x\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[2] else {
            panic!()
        };
        assert!(matches!(l.binds[0].voice, Expr::Voice(VoiceKind::Lead, _)));
    }

    #[test]
    fn unknown_let_name_suggests_closest() {
        let err =
            parse(&lex("let a = kick()\nloop \"l\":\n    q << \"x\"\n").unwrap()).unwrap_err();
        assert!(err.message.contains("q"));
        assert!(err.hint.is_some());
    }

    #[test]
    fn declared_names_suggest_typos() {
        let src = "let kik = kick()\nloop \"l\":\n    kix << \"x\"\n";
        let err = parse(&lex(src).unwrap()).unwrap_err();
        assert!(err.hint.as_deref().unwrap().contains("kik"));
    }

    #[test]
    fn param_typos_still_hint_after_let_resolution() {
        let err = parse(&lex("loop \"a\":\n    kick << \"x\" vol=0.9\n").unwrap()).unwrap_err();
        assert!(err.message.contains("vol"));
        assert!(err.hint.as_deref().unwrap().contains("vel"));
    }

    #[test]
    fn pattern_slot_names_error_with_hint() {
        let err = parse(&lex("loop \"a\":\n    kick << smaple\n").unwrap()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.hint.is_some());
    }

    #[test]
    fn bare_builtin_name_in_bind_still_works() {
        let src = "loop \"a\":\n    kick << \"x\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[0] else {
            panic!()
        };
        assert!(matches!(l.binds[0].voice, Expr::Voice(VoiceKind::Kick, _)));
    }

    #[test]
    fn sample_typo_before_string_hints() {
        let err =
            parse(&lex("loop \"a\":\n    smaple \"x\" << \"x . x .\"\n").unwrap()).unwrap_err();
        assert!(err.hint.is_some());
    }

    #[test]
    fn sample_region_params_validated() {
        assert_eq!(
            parse(&lex("loop \"a\":\n    sample \"k.wav\" << \"x\" start=1.5\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    sample \"k.wav\" << \"x\" dur=0\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
        assert_eq!(
            parse(&lex("loop \"a\":\n    sample \"k.wav\" << \"x\" cycle=2\n").unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
    }
}
