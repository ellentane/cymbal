use crate::ast::{Expr, Stmt};
use crate::error::{Error, ErrorKind, Result, Span};
use crate::lexer::lex;
use crate::parser::parse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformKind {
    Reverse,
    RotateLeft,
    RotateRight,
    HalfSpeed,
}

pub fn tokenize_pattern(s: &str) -> Result<Vec<String>> {
    let chars: Vec<char> = s.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\r' => i += 1,
            'x' => {
                let mut tok = String::from("x");
                i += 1;
                let mut in_number = false;
                while i < chars.len() {
                    let c = chars[i];
                    if c == '!' || c == '@' {
                        tok.push(c);
                        i += 1;
                        in_number = true;
                    } else if c.is_ascii_digit()
                        || (c == '-' && in_number)
                        || (c == '.' && in_number)
                    {
                        tok.push(c);
                        i += 1;
                    } else {
                        break;
                    }
                }
                tokens.push(tok);
            }
            '.' => {
                tokens.push(".".to_string());
                i += 1;
            }
            other => {
                return Err(Error::new(
                    Span { line: 1, col: 1 },
                    ErrorKind::Parse,
                    format!("invalid pattern character '{other}'"),
                ));
            }
        }
    }
    if tokens.is_empty() {
        return Err(Error::new(
            Span { line: 1, col: 1 },
            ErrorKind::Parse,
            "pattern cannot be empty",
        ));
    }
    Ok(tokens)
}

pub fn apply_kind(pattern: &str, kind: TransformKind) -> Result<String> {
    let tokens = tokenize_pattern(pattern)?;
    let out = match kind {
        TransformKind::Reverse => tokens.into_iter().rev().collect::<Vec<_>>(),
        TransformKind::RotateLeft => {
            let mut t = tokens;
            if !t.is_empty() {
                t.rotate_left(1);
            }
            t
        }
        TransformKind::RotateRight => {
            let mut t = tokens;
            if !t.is_empty() {
                t.rotate_right(1);
            }
            t
        }
        TransformKind::HalfSpeed => {
            let mut doubled = Vec::with_capacity(tokens.len() * 2);
            for tok in tokens {
                doubled.push(tok.clone());
                doubled.push(tok);
            }
            doubled
        }
    };
    Ok(out.join(" "))
}

/// Apply `kind` to the pattern string on the given 0-based line of `src`.
/// The line must be a bind line whose pattern is a string or a tuple's string.
pub fn transform_src(src: &str, line: usize, kind: TransformKind) -> Result<String> {
    let program = parse(&lex(src)?)?;
    let mut target: Option<(usize, usize, String)> = None; // (start_idx, end_idx, old)
    for stmt in &program.statements {
        let Stmt::Loop(l) = stmt else { continue };
        for bind in &l.binds {
            let (pattern, span) = match &bind.pattern {
                Expr::PatternString(s, span) => (s, span),
                Expr::Tuple(_, s, span) => (s, span),
                _ => continue,
            };
            if span.line != line as u32 + 1 {
                continue;
            }
            let raw = src.lines().nth(line).ok_or_else(|| {
                Error::new(
                    Span { line: 1, col: 1 },
                    ErrorKind::Eval,
                    "line out of range",
                )
            })?;
            let col = span.col as usize;
            let start = if col > 1 && raw.as_bytes()[col - 1] == b'"' {
                raw[..col - 1].rfind('"').ok_or_else(|| {
                    Error::new(*span, ErrorKind::Eval, "no pattern string on line")
                })?
            } else {
                raw[col - 1..]
                    .find('"')
                    .map(|i| col - 1 + i)
                    .ok_or_else(|| {
                        Error::new(*span, ErrorKind::Eval, "no pattern string on line")
                    })?
            };
            let rest = &raw[start + 1..];
            let close = rest
                .find('"')
                .ok_or_else(|| Error::new(*span, ErrorKind::Eval, "unterminated pattern string"))?;
            let end = start + 1 + close;
            let new_pattern = apply_kind(pattern, kind)?;
            target = Some((start, end, new_pattern));
            break;
        }
    }
    let (start, end, new_pattern) = target.ok_or_else(|| {
        Error::new(
            Span {
                line: line as u32 + 1,
                col: 1,
            },
            ErrorKind::Eval,
            "no pattern on this line",
        )
    })?;
    let mut lines: Vec<String> = src.lines().map(|s| s.to_string()).collect();
    let raw = &lines[line];
    lines[line] = format!("{}{}{}", &raw[..start + 1], new_pattern, &raw[end..]);
    let was_nl = src.ends_with('\n');
    let mut out = lines.join("\n");
    if was_nl {
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_hits_and_rests() {
        assert_eq!(
            tokenize_pattern("x . x .").unwrap(),
            vec!["x", ".", "x", "."]
        );
    }

    #[test]
    fn tokens_keep_modifiers() {
        assert_eq!(
            tokenize_pattern("x!0.5@3 . x@-2").unwrap(),
            vec!["x!0.5@3", ".", "x@-2"]
        );
    }

    #[test]
    fn reverse_flips_sequence() {
        assert_eq!(
            apply_kind("x . x .", TransformKind::Reverse).unwrap(),
            ". x . x"
        );
        assert_eq!(
            apply_kind("x . . x", TransformKind::Reverse).unwrap(),
            "x . . x"
        );
    }

    #[test]
    fn rotate_moves_one_step() {
        assert_eq!(
            apply_kind("x . x .", TransformKind::RotateRight).unwrap(),
            ". x . x"
        );
        assert_eq!(
            apply_kind("x . x .", TransformKind::RotateLeft).unwrap(),
            ". x . x"
        );
        assert_eq!(
            apply_kind("x x . .", TransformKind::RotateLeft).unwrap(),
            "x . . x"
        );
    }

    #[test]
    fn half_speed_doubles_every_step() {
        assert_eq!(
            apply_kind("x . x .", TransformKind::HalfSpeed).unwrap(),
            "x x . . x x . ."
        );
    }

    #[test]
    fn half_speed_preserves_modifiers() {
        assert_eq!(
            apply_kind("x!0.5 .", TransformKind::HalfSpeed).unwrap(),
            "x!0.5 x!0.5 . ."
        );
    }

    #[test]
    fn invalid_char_rejected() {
        assert_eq!(tokenize_pattern("x y").unwrap_err().kind, ErrorKind::Parse);
    }

    #[test]
    fn transform_targets_the_cursor_line() {
        let src =
            "let kick = kick()\nloop \"b\":\n    kick << \"x . x .\"\n    kick << \". x . x\"\n";
        let out = transform_src(src, 2, TransformKind::Reverse).unwrap();
        assert_eq!(
            out,
            "let kick = kick()\nloop \"b\":\n    kick << \". x . x\"\n    kick << \". x . x\"\n"
        );
        let out2 = transform_src(src, 3, TransformKind::Reverse).unwrap();
        assert_eq!(
            out2,
            "let kick = kick()\nloop \"b\":\n    kick << \"x . x .\"\n    kick << \"x . x .\"\n"
        );
    }

    #[test]
    fn transform_handles_tuple_patterns() {
        let src = "loop \"b\":\n    bass << ([c2, f2], \"x . . x\")\n";
        let out = transform_src(src, 1, TransformKind::HalfSpeed).unwrap();
        assert_eq!(
            out,
            "loop \"b\":\n    bass << ([c2, f2], \"x x . . . . x x\")\n"
        );
    }

    #[test]
    fn transform_rejects_lines_without_patterns() {
        assert_eq!(
            transform_src("let kick = kick()\n", 0, TransformKind::Reverse)
                .unwrap_err()
                .kind,
            ErrorKind::Eval
        );
        assert_eq!(
            transform_src("loop \"b\":\n", 0, TransformKind::Reverse)
                .unwrap_err()
                .kind,
            ErrorKind::Eval
        );
    }

    #[test]
    fn transformed_source_keeps_parsing_and_changes_offsets() {
        let src = "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x . x .\"\n";
        let out = transform_src(src, 3, TransformKind::HalfSpeed).unwrap();
        let program = parse(&lex(&out).unwrap()).unwrap();
        let Stmt::Loop(l) = &program.statements[2] else {
            panic!()
        };
        let Expr::PatternString(p, _) = &l.binds[0].pattern else {
            panic!()
        };
        assert_eq!(p, "x x . . x x . .");
    }
}
