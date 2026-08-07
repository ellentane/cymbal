use crate::ast::Expr;
use crate::error::{Error, ErrorKind, Result, Span};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub on: bool,
    pub velocity: f32,
    pub semitone: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub pitch: u8,
    pub velocity: f32,
    pub semitone: i32,
}

fn dot_followed_by_digit(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut probe = chars.clone();
    probe.next();
    probe.next().is_some_and(|c| c.is_ascii_digit())
}

pub(crate) fn parse_modifier_ops(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<(f32, i32, String)> {
    let mut velocity = 1.0f32;
    let mut semitone = 0i32;
    let mut text = String::new();
    loop {
        match chars.peek() {
            Some('*') => {
                chars.next();
                text.push('*');
                let mut num = String::new();
                while let Some(&c2) = chars.peek() {
                    if c2.is_ascii_digit() || c2 == '.' {
                        num.push(c2);
                        text.push(c2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let junk = match chars.peek() {
                    Some(&c2) => {
                        c2.is_ascii_alphanumeric() || (c2 == '.' && dot_followed_by_digit(chars))
                    }
                    None => false,
                };
                if junk {
                    while let Some(&c2) = chars.peek() {
                        if c2.is_ascii_alphanumeric() || c2 == '.' {
                            num.push(c2);
                            text.push(c2);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    return Err(Error::new(
                        Span { line: 1, col: 1 },
                        ErrorKind::Parse,
                        format!("expected number after '*', got {num}"),
                    ));
                }
                let v: f32 = num.parse().map_err(|_| {
                    Error::new(
                        Span { line: 1, col: 1 },
                        ErrorKind::Parse,
                        "expected number after '*'",
                    )
                    .with_hint("write x*0.5 or x+2")
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
                if chars.peek() == Some(&'.') {
                    break;
                }
            }
            Some('+') | Some('-') => {
                let Some(&sign) = chars.peek() else {
                    unreachable!()
                };
                chars.next();
                text.push(sign);
                let mut num = String::new();
                while let Some(&c2) = chars.peek() {
                    if c2.is_ascii_digit() {
                        num.push(c2);
                        text.push(c2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let junk = match chars.peek() {
                    Some(&c2) => {
                        c2.is_ascii_alphanumeric() || (c2 == '.' && dot_followed_by_digit(chars))
                    }
                    None => false,
                };
                if junk {
                    while let Some(&c2) = chars.peek() {
                        if c2.is_ascii_alphanumeric() || c2 == '.' {
                            num.push(c2);
                            text.push(c2);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    return Err(Error::new(
                        Span { line: 1, col: 1 },
                        ErrorKind::Parse,
                        format!("expected integer after '{sign}', got {num}"),
                    ));
                }
                let n: i32 = num.parse().map_err(|_| {
                    Error::new(
                        Span { line: 1, col: 1 },
                        ErrorKind::Parse,
                        format!("expected integer after '{sign}'"),
                    )
                    .with_hint("write x*0.5 or x+2")
                })?;
                let shifted = if sign == '+' { n } else { -n };
                if !(-48..=48).contains(&shifted) {
                    return Err(Error::new(
                        Span { line: 1, col: 1 },
                        ErrorKind::Parse,
                        format!("semitone shift out of range: {shifted} (max 48)"),
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
                if chars.peek() == Some(&'.') {
                    break;
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
            _ => break,
        }
    }
    Ok((velocity, semitone, text))
}

pub fn expand_string(s: &str) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'x' => {
                let (velocity, semitone, _) = parse_modifier_ops(&mut chars)?;
                hits.push(Hit {
                    on: true,
                    velocity,
                    semitone,
                });
            }
            '.' => hits.push(Hit {
                on: false,
                velocity: 1.0,
                semitone: 0,
            }),
            ' ' | '\t' | '\r' => {}
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
    if hits.is_empty() {
        return Err(Error::new(
            Span { line: 1, col: 1 },
            ErrorKind::Parse,
            "pattern cannot be empty",
        ));
    }
    Ok(hits)
}

pub fn bar_triggers(pattern: &Expr, default_pitch: u8) -> Result<(usize, Vec<Option<Step>>)> {
    match pattern {
        Expr::PatternString(s, span) => {
            let hits = expand_string(s).map_err(|mut e| {
                e.span = *span;
                e
            })?;
            Ok((
                hits.len(),
                hits.iter()
                    .map(|h| {
                        if h.on {
                            Some(Step {
                                pitch: default_pitch,
                                velocity: h.velocity,
                                semitone: h.semitone,
                            })
                        } else {
                            None
                        }
                    })
                    .collect(),
            ))
        }
        Expr::Notes(notes, span) => {
            if notes.is_empty() {
                return Err(Error::new(
                    *span,
                    ErrorKind::Parse,
                    "note array cannot be empty",
                ));
            }
            let steps: Vec<Option<Step>> = notes
                .iter()
                .map(|n| {
                    Some(Step {
                        pitch: n.midi,
                        velocity: 1.0,
                        semitone: 0,
                    })
                })
                .collect();
            Ok((notes.len(), steps))
        }
        Expr::Tuple(notes, s, span) => {
            if notes.is_empty() {
                return Err(Error::new(
                    *span,
                    ErrorKind::Parse,
                    "tuple note array cannot be empty",
                ));
            }
            let hits = expand_string(s).map_err(|mut e| {
                e.span = *span;
                e
            })?;
            let mut hit_index = 0;
            let steps = hits
                .iter()
                .map(|h| {
                    if h.on {
                        let step = Some(Step {
                            pitch: notes[hit_index % notes.len()].midi,
                            velocity: h.velocity,
                            semitone: h.semitone,
                        });
                        hit_index += 1;
                        step
                    } else {
                        None
                    }
                })
                .collect();
            Ok((hits.len(), steps))
        }
        Expr::Voice(_, span) => Err(Error::new(
            *span,
            ErrorKind::Eval,
            "expected a pattern, got a voice",
        )),
        Expr::Sample(_, span) => Err(Error::new(
            *span,
            ErrorKind::Eval,
            "expected a pattern, got a sample voice",
        )),
        Expr::Name(name, span) => Err(Error::new(
            *span,
            ErrorKind::Eval,
            format!("unresolved voice name '{name}'"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Note;

    #[test]
    fn expands_hits_and_rests() {
        assert_eq!(
            expand_string("x . . x . . x .").unwrap(),
            vec![
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
            ]
        );
        assert_eq!(
            expand_string("x").unwrap(),
            vec![Hit {
                on: true,
                velocity: 1.0,
                semitone: 0
            }]
        );
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(
            expand_string("  x \t .  x").unwrap(),
            vec![
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
            ]
        );
    }

    #[test]
    fn illegal_chars_rejected() {
        assert_eq!(expand_string("x y x").unwrap_err().kind, ErrorKind::Parse);
    }

    #[test]
    fn empty_pattern_rejected() {
        assert_eq!(expand_string("   ").unwrap_err().kind, ErrorKind::Parse);
    }

    #[test]
    fn string_triggers_use_default_pitch() {
        let (steps, hits) = bar_triggers(
            &Expr::PatternString("x . x .".into(), Span { line: 1, col: 1 }),
            60,
        )
        .unwrap();
        assert_eq!(steps, 4);
        let pitches: Vec<Option<u8>> = hits.iter().map(|s| s.map(|st| st.pitch)).collect();
        assert_eq!(pitches, vec![Some(60), None, Some(60), None]);
    }

    #[test]
    fn notes_are_evenly_spaced() {
        let (steps, hits) = bar_triggers(
            &Expr::Notes(
                vec![
                    Note {
                        midi: 60,
                        span: Span { line: 1, col: 1 },
                    },
                    Note {
                        midi: 64,
                        span: Span { line: 1, col: 1 },
                    },
                ],
                Span { line: 1, col: 1 },
            ),
            60,
        )
        .unwrap();
        assert_eq!(steps, 2);
        let pitches: Vec<Option<u8>> = hits.iter().map(|s| s.map(|st| st.pitch)).collect();
        assert_eq!(pitches, vec![Some(60), Some(64)]);
    }

    #[test]
    fn crlf_is_ignored() {
        assert_eq!(
            expand_string("x .\rx .\r").unwrap(),
            vec![
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
            ]
        );
    }

    #[test]
    fn empty_notes_rejected() {
        let err = bar_triggers(&Expr::Notes(vec![], Span { line: 1, col: 1 }), 60).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
    }

    #[test]
    fn tuple_combines_rhythm_and_pitches_cyclically() {
        let (steps, hits) = bar_triggers(
            &Expr::Tuple(
                vec![
                    Note {
                        midi: 36,
                        span: Span { line: 1, col: 1 },
                    },
                    Note {
                        midi: 41,
                        span: Span { line: 1, col: 1 },
                    },
                ],
                "x . x . x".into(),
                Span { line: 1, col: 1 },
            ),
            60,
        )
        .unwrap();
        assert_eq!(steps, 5);
        let pitches: Vec<Option<u8>> = hits.iter().map(|s| s.map(|st| st.pitch)).collect();
        assert_eq!(pitches, vec![Some(36), None, Some(41), None, Some(36)]);
    }

    #[test]
    fn hit_with_velocity_and_semitone() {
        assert_eq!(
            expand_string("x*0.5+3 . x").unwrap(),
            vec![
                Hit {
                    on: true,
                    velocity: 0.5,
                    semitone: 3
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
            ]
        );
    }

    #[test]
    fn negative_semitone_parses() {
        assert_eq!(
            expand_string("x-2").unwrap(),
            vec![Hit {
                on: true,
                velocity: 1.0,
                semitone: -2
            }]
        );
    }

    #[test]
    fn semitone_shift_bounded_to_four_octaves() {
        assert_eq!(expand_string("x+49").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(expand_string("x-49").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(
            expand_string("x+48").unwrap(),
            vec![Hit {
                on: true,
                velocity: 1.0,
                semitone: 48
            }]
        );
        assert_eq!(
            expand_string("x-48").unwrap(),
            vec![Hit {
                on: true,
                velocity: 1.0,
                semitone: -48
            }]
        );
    }

    #[test]
    fn velocity_range_checked() {
        assert_eq!(expand_string("x*1.5").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(expand_string("x*-0.5").unwrap_err().kind, ErrorKind::Parse);
    }

    #[test]
    fn modifiers_must_follow_hit() {
        assert_eq!(expand_string("!0.5").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(expand_string("@3").unwrap_err().kind, ErrorKind::Parse);
    }

    #[test]
    fn semitone_allowed_on_all_voices() {
        let (_, steps) = bar_triggers(
            &Expr::PatternString("x+2".into(), Span { line: 1, col: 1 }),
            60,
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![Some(Step {
                pitch: 60,
                velocity: 1.0,
                semitone: 2
            })]
        );
    }

    #[test]
    fn modifier_errors_carry_pattern_span() {
        let span = Span { line: 7, col: 3 };
        let err = bar_triggers(&Expr::PatternString("x*1.5".into(), span), 60).unwrap_err();
        assert_eq!(err.span, span);
        let err = bar_triggers(&Expr::Tuple(vec![], "x*1.5".into(), span), 60).unwrap_err();
        assert_eq!(err.span, span);
    }

    #[test]
    fn arithmetic_modifiers_parse() {
        assert_eq!(
            expand_string("x*0.5 . x+3 . x-2").unwrap(),
            vec![
                Hit {
                    on: true,
                    velocity: 0.5,
                    semitone: 0
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 3
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: -2
                },
            ]
        );
    }

    #[test]
    fn modifiers_combine_order_free() {
        assert_eq!(
            expand_string("x+2*0.5").unwrap()[0],
            Hit {
                on: true,
                velocity: 0.5,
                semitone: 2
            }
        );
        assert_eq!(
            expand_string("x*0.5+2").unwrap()[0],
            Hit {
                on: true,
                velocity: 0.5,
                semitone: 2
            }
        );
    }

    #[test]
    fn repeats_accumulate() {
        assert_eq!(
            expand_string("x+2+3").unwrap()[0],
            Hit {
                on: true,
                velocity: 1.0,
                semitone: 5
            }
        );
        assert_eq!(
            expand_string("x*0.5*0.7").unwrap()[0],
            Hit {
                on: true,
                velocity: 0.35,
                semitone: 0
            }
        );
        assert_eq!(
            expand_string("x+2-3").unwrap()[0],
            Hit {
                on: true,
                velocity: 1.0,
                semitone: -1
            }
        );
    }

    #[test]
    fn old_modifiers_rejected_with_hints() {
        let err = expand_string("x!0.5").unwrap_err();
        assert!(err.message.contains('*'));
        assert!(err.hint.is_some());
        let err = expand_string("x@2").unwrap_err();
        assert!(err.message.contains('+'));
        assert!(err.hint.is_some());
    }

    #[test]
    fn standalone_old_modifiers_get_hints() {
        let err = expand_string("!0.5").unwrap_err();
        assert!(err.hint.is_some());
        let err = expand_string("@3").unwrap_err();
        assert!(err.hint.is_some());
        let err = expand_string("-2").unwrap_err();
        assert!(err.hint.is_some());
    }

    #[test]
    fn fractional_transpose_reports_integer_error() {
        let err = expand_string("x+2.5").unwrap_err();
        assert!(err.message.contains("integer"), "got: {}", err.message);
        let err = expand_string("x+1e2").unwrap_err();
        assert!(err.message.contains("integer"), "got: {}", err.message);
    }

    #[test]
    fn dot_after_transpose_starts_rest() {
        assert_eq!(
            expand_string("x+2. x+3.x").unwrap(),
            vec![
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 2
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 3
                },
                Hit {
                    on: false,
                    velocity: 1.0,
                    semitone: 0
                },
                Hit {
                    on: true,
                    velocity: 1.0,
                    semitone: 0
                },
            ]
        );
    }

    #[test]
    fn empty_operands_and_junk_rejected() {
        let err = expand_string("x*").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.hint.is_some());
        let err = expand_string("x+").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Parse);
        assert!(err.hint.is_some());
        let err = expand_string("x-0.5").unwrap_err();
        assert!(err.message.contains("got 0.5"), "got: {}", err.message);
        let err = expand_string("x+2abc").unwrap_err();
        assert!(err.message.contains("got 2abc"), "got: {}", err.message);
        let err = expand_string("x*2abc").unwrap_err();
        assert!(err.message.contains("got 2abc"), "got: {}", err.message);
        let err = expand_string("x+48+1").unwrap_err();
        assert!(err.message.contains("49"), "got: {}", err.message);
    }

    #[test]
    fn velocity_cap_has_hint() {
        let err = expand_string("x*2").unwrap_err();
        assert!(err.message.contains("0..=1"));
        assert!(err.hint.is_some());
    }
}
