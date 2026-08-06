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
                let mut velocity = 1.0f32;
                let mut semitone = 0i32;
                'mods: loop {
                    match chars.get(i) {
                        Some('*') => {
                            tok.push('*');
                            i += 1;
                            let mut num = String::new();
                            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                                num.push(chars[i]);
                                tok.push(chars[i]);
                                i += 1;
                            }
                            let v: f32 = num.parse().map_err(|_| {
                                Error::new(
                                    Span { line: 1, col: 1 },
                                    ErrorKind::Parse,
                                    "expected number after '*'",
                                )
                            })?;
                            if !(0.0..=1.0).contains(&v) {
                                return Err(Error::new(
                                    Span { line: 1, col: 1 },
                                    ErrorKind::Parse,
                                    format!("velocity must be in 0..=1, got {v}"),
                                )
                                .with_hint("velocity caps at 1 — try x*0.5"));
                            }
                            velocity *= v;
                            if !(0.0..=1.0).contains(&velocity) {
                                return Err(Error::new(
                                    Span { line: 1, col: 1 },
                                    ErrorKind::Parse,
                                    "velocity must be in 0..=1",
                                ));
                            }
                        }
                        Some('+') | Some('-') => {
                            let sign = chars[i];
                            tok.push(sign);
                            i += 1;
                            let mut num = String::new();
                            while i < chars.len() && chars[i].is_ascii_digit() {
                                num.push(chars[i]);
                                tok.push(chars[i]);
                                i += 1;
                            }
                            if i < chars.len() {
                                let next = chars[i];
                                let fractional = next == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit();
                                if next.is_ascii_alphanumeric() || fractional {
                                    while i < chars.len()
                                        && (chars[i].is_ascii_alphanumeric() || chars[i] == '.')
                                    {
                                        num.push(chars[i]);
                                        i += 1;
                                    }
                                    return Err(Error::new(
                                        Span { line: 1, col: 1 },
                                        ErrorKind::Parse,
                                        format!("expected integer after '{sign}', got {num}"),
                                    ));
                                }
                                if next == '.' {
                                    break 'mods;
                                }
                            }
                            let n: i32 = num.parse().map_err(|_| {
                                Error::new(
                                    Span { line: 1, col: 1 },
                                    ErrorKind::Parse,
                                    format!("expected integer after '{sign}'"),
                                )
                            })?;
                            let shifted = if sign == '+' { n } else { -n };
                            if !(-48..=48).contains(&shifted) {
                                return Err(Error::new(
                                    Span { line: 1, col: 1 },
                                    ErrorKind::Parse,
                                    "semitone shift out of range (-48..=48)",
                                ));
                            }
                            semitone += shifted;
                            if !(-48..=48).contains(&semitone) {
                                return Err(Error::new(
                                    Span { line: 1, col: 1 },
                                    ErrorKind::Parse,
                                    format!("semitone shift out of range: {semitone} (max 48)"),
                                ));
                            }
                        }
                        Some('!') => {
                            return Err(Error::new(
                                Span { line: 1, col: 1 },
                                ErrorKind::Parse,
                                "velocity modifier '!' is '*' now",
                            )
                            .with_hint("write x*0.5 instead of x!0.5"));
                        }
                        Some('@') => {
                            return Err(Error::new(
                                Span { line: 1, col: 1 },
                                ErrorKind::Parse,
                                "transpose modifier '@' is '+' or '-' now",
                            )
                            .with_hint("write x+2 instead of x@2"));
                        }
                        _ => break 'mods,
                    }
                }
                tokens.push(tok);
            }
            '.' => {
                tokens.push(".".to_string());
                i += 1;
            }
            other => {
                let hint = match other {
                    '!' => Some("hit velocity is '*' now: x*0.5"),
                    '@' => Some("hit transpose is '+' or '-' now: x+2"),
                    '*' | '+' | '-' => Some("modifiers attach to a hit: x*0.5, x+2"),
                    _ => None,
                };
                let mut err = Error::new(
                    Span { line: 1, col: 1 },
                    ErrorKind::Parse,
                    format!("invalid pattern character '{other}' (use 'x' for hit, '.' for rest)"),
                );
                if let Some(h) = hint {
                    err = err.with_hint(h);
                }
                return Err(err);
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

fn char_col_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col.saturating_sub(1))
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

/// Apply `kind` to the pattern string on the given 0-based line of `src`.
/// The line must be a bind line whose pattern is a string or a tuple's string.
pub fn transform_src(src: &str, line: usize, kind: TransformKind) -> Result<String> {
    let program = parse(&lex(src)?)?;
    let mut lines: Vec<String> = src.lines().map(|s| s.to_string()).collect();
    let mut target: Option<(usize, usize, String)> = None; // (start_idx, end_idx, new_pattern)
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
            let raw = lines.get(line).ok_or_else(|| {
                Error::new(
                    Span { line: 1, col: 1 },
                    ErrorKind::Eval,
                    "line out of range",
                )
            })?;
            let byte = char_col_to_byte(raw, span.col as usize);
            let start = if raw.as_bytes().get(byte) == Some(&b'"') {
                raw[..byte].rfind('"').ok_or_else(|| {
                    Error::new(*span, ErrorKind::Eval, "no pattern string on line")
                })?
            } else {
                raw[byte..].find('"').map(|i| byte + i).ok_or_else(|| {
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
            tokenize_pattern("x*0.5+3 . x-2").unwrap(),
            vec!["x*0.5+3", ".", "x-2"]
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
            apply_kind("x*0.5 .", TransformKind::HalfSpeed).unwrap(),
            "x*0.5 x*0.5 . ."
        );
    }

    #[test]
    fn tokenizer_errors_match_expand_string() {
        let err = tokenize_pattern("x!0.5").unwrap_err();
        assert!(err.message.contains('*'));
        assert!(err.hint.is_some());
        let err = tokenize_pattern("x@2").unwrap_err();
        assert!(err.message.contains('+'));
        assert!(err.hint.is_some());
        let err = tokenize_pattern("x+2.5").unwrap_err();
        assert!(err.message.contains("integer"), "got: {}", err.message);
        let err = tokenize_pattern("x*2").unwrap_err();
        assert!(err.message.contains("0..=1"));
        assert!(err.hint.is_some());
        let err = tokenize_pattern("x+30+30").unwrap_err();
        assert!(err.message.contains("out of range"));
        let err = tokenize_pattern("!0.5").unwrap_err();
        assert!(err.hint.is_some());
        let err = tokenize_pattern("+3").unwrap_err();
        assert!(err.hint.is_some());
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
        let src = "loop \"b\":\n    bass << [c2, f2] \"x . . x\"\n";
        let out = transform_src(src, 1, TransformKind::HalfSpeed).unwrap();
        assert_eq!(
            out,
            "loop \"b\":\n    bass << [c2, f2] \"x x . . . . x x\"\n"
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

    #[test]
    fn multibyte_in_pattern_does_not_panic() {
        let src = "loop \"b\":\n    kick << \"x . 🎉 x\"\n";
        let err = transform_src(src, 1, TransformKind::Reverse).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn multibyte_loop_name_on_bind_line() {
        let src = "loop \"🎉\": kick << \"x x .\"\n";
        let out = transform_src(src, 0, TransformKind::Reverse).unwrap();
        assert_eq!(out, "loop \"🎉\": kick << \". x x\"\n");
    }

    #[test]
    fn tokens_split_rest_after_semitone() {
        assert_eq!(tokenize_pattern("x+3.x").unwrap(), vec!["x+3", ".", "x"]);
    }

    #[test]
    fn tokens_keep_velocity_then_semitone() {
        assert_eq!(
            tokenize_pattern("x*0.5+3 . x").unwrap(),
            vec!["x*0.5+3", ".", "x"]
        );
    }

    #[test]
    fn half_speed_keeps_semitone_rest_parseable() {
        assert_eq!(
            apply_kind("x+3.x", TransformKind::HalfSpeed).unwrap(),
            "x+3 x+3 . . x x"
        );
    }

    #[test]
    fn rotate_right_direction() {
        assert_eq!(
            apply_kind("x x . .", TransformKind::RotateRight).unwrap(),
            ". x x ."
        );
    }

    #[test]
    fn single_token_transforms() {
        assert_eq!(apply_kind("x", TransformKind::RotateLeft).unwrap(), "x");
        assert_eq!(apply_kind("x", TransformKind::RotateRight).unwrap(), "x");
        assert_eq!(apply_kind("x", TransformKind::HalfSpeed).unwrap(), "x x");
    }

    #[test]
    fn empty_pattern_rejected() {
        assert_eq!(tokenize_pattern("").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(tokenize_pattern("   ").unwrap_err().kind, ErrorKind::Parse);
    }

    #[test]
    fn transform_keeps_trailing_comment() {
        let src = "loop \"b\":\n    kick << \"x x .\" -- keep me\n";
        let out = transform_src(src, 1, TransformKind::Reverse).unwrap();
        assert_eq!(out, "loop \"b\":\n    kick << \". x x\" -- keep me\n");
    }
}
