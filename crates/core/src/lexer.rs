use crate::error::{Error, ErrorKind, Result, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Number(f64),
    Note(u8),
    String(String),
    Let,
    Loop,
    Tempo,
    Rev,
    Every,
    Help,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Assign,
    Colon,
    DotDot,
    Bind,
    Pipe,
    Comma,
    Newline,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

struct Lexer {
    chars: Vec<(char, Span)>,
    pos: usize,
}

pub fn note_midi(letter: char, sharp: i32, octave: u32) -> u8 {
    let base = match letter {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => unreachable!(),
    };
    ((octave + 1) as i32 * 12 + base + sharp) as u8
}

pub fn lex(src: &str) -> Result<Vec<Token>> {
    let mut chars = Vec::new();
    for (i, c) in src.char_indices() {
        let (line, col) = line_col(src, i);
        chars.push((c, Span { line, col }));
    }
    let mut lx = Lexer { chars, pos: 0 };
    let mut tokens = Vec::new();
    loop {
        let tok = lx.next_token()?;
        let done = tok.kind == TokenKind::Eof;
        tokens.push(tok);
        if done {
            break;
        }
    }
    Ok(tokens)
}

fn line_col(src: &str, byte: usize) -> (u32, u32) {
    let mut line = 1u32;
    let mut col = 1u32;
    for (i, c) in src.char_indices() {
        if i >= byte {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

impl Lexer {
    fn peek(&self) -> Option<(char, Span)> {
        self.chars.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<(char, Span)> {
        self.chars.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<(char, Span)> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn next_token(&mut self) -> Result<Token> {
        loop {
            let Some((c, span)) = self.peek() else {
                return Ok(Token {
                    kind: TokenKind::Eof,
                    span: Span { line: 1, col: 1 },
                });
            };
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '\n' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::Newline,
                        span,
                    });
                }
                '-' => {
                    if self.peek2().map(|(c2, _)| c2) == Some('-') {
                        while let Some((c, _)) = self.peek() {
                            if c == '\n' {
                                break;
                            }
                            self.bump();
                        }
                    } else if self.peek2().is_some_and(|(c2, _)| c2.is_ascii_digit()) {
                        return self.lex_number(span);
                    } else {
                        return Err(Error::new(span, ErrorKind::Lex, "unexpected character '-'")
                            .with_hint("glyphs are quoted in help topics: help \"-\""));
                    }
                }
                '"' => return self.lex_string(span),
                '(' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::LParen,
                        span,
                    });
                }
                ')' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::RParen,
                        span,
                    });
                }
                '[' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::LBracket,
                        span,
                    });
                }
                ']' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::RBracket,
                        span,
                    });
                }
                '=' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::Assign,
                        span,
                    });
                }
                ':' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::Colon,
                        span,
                    });
                }
                ',' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::Comma,
                        span,
                    });
                }
                '<' => {
                    if self.peek2().map(|(c2, _)| c2) == Some('<') {
                        self.bump();
                        self.bump();
                        return Ok(Token {
                            kind: TokenKind::Bind,
                            span,
                        });
                    }
                    return Err(Error::new(span, ErrorKind::Lex, "expected '<<'"));
                }
                '>' => {
                    if self.peek2().map(|(c2, _)| c2) == Some('>') {
                        self.bump();
                        self.bump();
                        return Err(Error::new(span, ErrorKind::Lex, "expected '|'")
                            .with_hint("transforms chain with '|' now: \"x . x .\" | rev"));
                    }
                    return Err(Error::new(span, ErrorKind::Lex, "expected '|'"));
                }
                '|' => {
                    self.bump();
                    return Ok(Token {
                        kind: TokenKind::Pipe,
                        span,
                    });
                }
                '.' => {
                    if self.peek2().map(|(c2, _)| c2) == Some('.') {
                        self.bump();
                        self.bump();
                        return Ok(Token {
                            kind: TokenKind::DotDot,
                            span,
                        });
                    }
                    return Err(Error::new(span, ErrorKind::Lex, "unexpected character '.'")
                        .with_hint("glyphs are quoted in help topics: help \".\""));
                }
                '0'..='9' => return self.lex_number(span),
                'a'..='g' => return self.lex_alpha(span),
                _ if c.is_ascii_alphabetic() => return self.lex_ident(span),
                _ => {
                    let hint = matches!(c, '*' | '+')
                        .then(|| "glyphs are quoted in help topics: help \".\"");
                    let mut err =
                        Error::new(span, ErrorKind::Lex, format!("unexpected character '{c}'"));
                    if let Some(h) = hint {
                        err = err.with_hint(h);
                    }
                    return Err(err);
                }
            }
        }
    }

    fn lex_string(&mut self, open_span: Span) -> Result<Token> {
        self.bump(); // opening quote
        let mut s = String::new();
        while let Some((c, span)) = self.peek() {
            self.bump();
            if c == '"' {
                return Ok(Token {
                    kind: TokenKind::String(s),
                    span,
                });
            }
            if c == '\n' {
                return Err(Error::new(span, ErrorKind::Lex, "unterminated string"));
            }
            s.push(c);
        }
        Err(Error::new(open_span, ErrorKind::Lex, "unterminated string"))
    }

    fn lex_number(&mut self, span: Span) -> Result<Token> {
        let mut s = String::new();
        if self.peek().is_some_and(|(c, _)| c == '-') {
            s.push('-');
            self.bump();
        }
        while let Some((c, _)) = self.peek() {
            if c.is_ascii_digit()
                || (c == '.' && self.peek2().is_some_and(|(c2, _)| c2.is_ascii_digit()))
            {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        let n: f64 = s
            .parse()
            .map_err(|_| Error::new(span, ErrorKind::Lex, "invalid number"))?;
        Ok(Token {
            kind: TokenKind::Number(n),
            span,
        })
    }

    fn lex_alpha(&mut self, span: Span) -> Result<Token> {
        let (c, _) = self.chars[self.pos];
        let has_accidental = matches!(self.peek2().map(|(c2, _)| c2), Some('#') | Some('b'));
        let third = self.chars.get(self.pos + 2).map(|(c3, _)| *c3);
        if has_accidental && third.is_some_and(|c3| c3.is_ascii_digit()) {
            let sharp = match self.chars[self.pos + 1].0 {
                '#' => 1,
                'b' => -1,
                _ => 0,
            };
            let octave = self.chars[self.pos + 2].0.to_digit(10).unwrap();
            self.pos += 3;
            return Ok(Token {
                kind: TokenKind::Note(note_midi(c, sharp, octave)),
                span,
            });
        }
        if (third.is_none() || third.is_some_and(|c3| !c3.is_ascii_alphabetic()))
            && let Some((c2, _)) = self.peek2()
            && c2.is_ascii_digit()
        {
            let octave = c2.to_digit(10).unwrap();
            self.pos += 2;
            return Ok(Token {
                kind: TokenKind::Note(note_midi(c, 0, octave)),
                span,
            });
        }
        self.lex_ident(span)
    }

    fn lex_ident(&mut self, span: Span) -> Result<Token> {
        let mut s = String::new();
        while let Some((c, _)) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        let kind = match s.as_str() {
            "let" => TokenKind::Let,
            "loop" => TokenKind::Loop,
            "tempo" => TokenKind::Tempo,
            "rev" => TokenKind::Rev,
            "every" => TokenKind::Every,
            "help" => TokenKind::Help,
            _ => TokenKind::Ident(s),
        };
        Ok(Token { kind, span })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn k(tokens: &[Token]) -> Vec<TokenKind> {
        tokens.iter().map(|t| t.kind.clone()).collect()
    }

    #[test]
    fn lexes_bind_statement() {
        let tokens = lex("kick << \"x . . x\"\n").unwrap();
        assert_eq!(
            k(&tokens),
            vec![
                TokenKind::Ident("kick".into()),
                TokenKind::Bind,
                TokenKind::String("x . . x".into()),
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_notes_and_numbers_and_keywords() {
        let tokens = lex("let x = kick()\nloop \"a\" tempo=90: [c4, eb3, f#2]\n").unwrap();
        assert_eq!(
            k(&tokens),
            vec![
                TokenKind::Let,
                TokenKind::Ident("x".into()),
                TokenKind::Assign,
                TokenKind::Ident("kick".into()),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Newline,
                TokenKind::Loop,
                TokenKind::String("a".into()),
                TokenKind::Tempo,
                TokenKind::Assign,
                TokenKind::Number(90.0),
                TokenKind::Colon,
                TokenKind::LBracket,
                TokenKind::Note(60),
                TokenKind::Comma,
                TokenKind::Note(51),
                TokenKind::Comma,
                TokenKind::Note(42),
                TokenKind::RBracket,
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_combinator_pipe() {
        let tokens = lex("x << \"a\" | every(4, rev)\n").unwrap();
        assert_eq!(
            k(&tokens),
            vec![
                TokenKind::Ident("x".into()),
                TokenKind::Bind,
                TokenKind::String("a".into()),
                TokenKind::Pipe,
                TokenKind::Every,
                TokenKind::LParen,
                TokenKind::Number(4.0),
                TokenKind::Comma,
                TokenKind::Rev,
                TokenKind::RParen,
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_pipe_operator() {
        let tokens = lex("x << \"a\" | rev\n").unwrap();
        assert_eq!(
            k(&tokens),
            vec![
                TokenKind::Ident("x".into()),
                TokenKind::Bind,
                TokenKind::String("a".into()),
                TokenKind::Pipe,
                TokenKind::Rev,
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_dotdot_ramp() {
        let tokens = lex("pan=0.5..0.6\n").unwrap();
        assert_eq!(
            k(&tokens),
            vec![
                TokenKind::Ident("pan".into()),
                TokenKind::Assign,
                TokenKind::Number(0.5),
                TokenKind::DotDot,
                TokenKind::Number(0.6),
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn number_does_not_swallow_dotdot() {
        let tokens = lex("vel=1..0.5\n").unwrap();
        assert_eq!(
            k(&tokens),
            vec![
                TokenKind::Ident("vel".into()),
                TokenKind::Assign,
                TokenKind::Number(1.0),
                TokenKind::DotDot,
                TokenKind::Number(0.5),
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn double_gt_rejected_with_hint() {
        let err = lex("x << \"a\" >> rev\n").unwrap_err();
        assert!(err.kind == ErrorKind::Lex);
        assert!(err.hint.as_deref().unwrap().contains('|'));
    }

    #[test]
    fn leading_dot_number_rejected() {
        let err = lex("pan=.5..1\n").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Lex);
    }

    #[test]
    fn glyph_typos_in_help_get_hints() {
        let err = lex("help .\n").unwrap_err();
        assert!(err.hint.is_some());
        let err = lex("help -\n").unwrap_err();
        assert!(err.hint.is_some());
    }

    #[test]
    fn note_midi_math() {
        assert_eq!(note_midi('c', 0, 4), 60);
        assert_eq!(note_midi('a', 0, 4), 69);
        assert_eq!(note_midi('e', -1, 3), 51); // eb3
        assert_eq!(note_midi('f', 1, 2), 42); // f#2
        assert_eq!(note_midi('c', 0, 2), 36);
    }

    #[test]
    fn comments_and_whitespace_skipped() {
        let tokens = lex("-- a comment\n  kick << \"x\"  \n").unwrap();
        assert_eq!(
            k(&tokens),
            vec![
                TokenKind::Newline,
                TokenKind::Ident("kick".into()),
                TokenKind::Bind,
                TokenKind::String("x".into()),
                TokenKind::Newline,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn unterminated_string_is_lex_error() {
        let err = lex("kick << \"x").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Lex);
    }

    #[test]
    fn invalid_char_is_lex_error() {
        let err = lex("kick << \"x\" @").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Lex);
    }
}
