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

pub fn expand_string(s: &str) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'x' => {
                let mut velocity = 1.0f32;
                let mut semitone = 0i32;
                if chars.peek() == Some(&'!') {
                    chars.next();
                    let mut num = String::new();
                    while let Some(&c2) = chars.peek() {
                        if c2.is_ascii_digit() || c2 == '.' {
                            num.push(c2);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let v: f32 = num.parse().map_err(|_| {
                        Error::new(
                            Span { line: 1, col: 1 },
                            ErrorKind::Parse,
                            "expected number after '!'",
                        )
                    })?;
                    if !(0.0..=1.0).contains(&v) {
                        return Err(Error::new(
                            Span { line: 1, col: 1 },
                            ErrorKind::Parse,
                            format!("velocity must be in 0..=1, got {v}"),
                        ));
                    }
                    velocity = v;
                }
                if chars.peek() == Some(&'@') {
                    chars.next();
                    let mut num = String::new();
                    if chars.peek() == Some(&'-') {
                        num.push('-');
                        chars.next();
                    }
                    while let Some(&c2) = chars.peek() {
                        if c2.is_ascii_digit() {
                            num.push(c2);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    semitone = num.parse().map_err(|_| {
                        Error::new(
                            Span { line: 1, col: 1 },
                            ErrorKind::Parse,
                            "expected integer after '@'",
                        )
                    })?;
                }
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
                return Err(Error::new(
                    Span { line: 1, col: 1 },
                    ErrorKind::Parse,
                    format!("invalid pattern character '{other}' (use 'x' for hit, '.' for rest)"),
                ));
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

pub fn bar_triggers(
    pattern: &Expr,
    default_pitch: u8,
    sample_voice: bool,
) -> Result<(usize, Vec<Option<Step>>)> {
    match pattern {
        Expr::PatternString(s, span) => {
            let hits = expand_string(s)?;
            if !sample_voice && hits.iter().any(|h| h.semitone != 0) {
                return Err(Error::new(
                    *span,
                    ErrorKind::Eval,
                    "semitone shift '@n' is only valid on sample voices",
                ));
            }
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
            let hits = expand_string(s)?;
            if !sample_voice && hits.iter().any(|h| h.semitone != 0) {
                return Err(Error::new(
                    *span,
                    ErrorKind::Eval,
                    "semitone shift '@n' is only valid on sample voices",
                ));
            }
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
            false,
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
            false,
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
        let err =
            bar_triggers(&Expr::Notes(vec![], Span { line: 1, col: 1 }), 60, false).unwrap_err();
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
            false,
        )
        .unwrap();
        assert_eq!(steps, 5);
        let pitches: Vec<Option<u8>> = hits.iter().map(|s| s.map(|st| st.pitch)).collect();
        assert_eq!(pitches, vec![Some(36), None, Some(41), None, Some(36)]);
    }

    #[test]
    fn hit_with_velocity_and_semitone() {
        assert_eq!(
            expand_string("x!0.5@3 . x").unwrap(),
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
            expand_string("x@-2").unwrap(),
            vec![Hit {
                on: true,
                velocity: 1.0,
                semitone: -2
            }]
        );
    }

    #[test]
    fn velocity_range_checked() {
        assert_eq!(expand_string("x!1.5").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(expand_string("x!-0.5").unwrap_err().kind, ErrorKind::Parse);
    }

    #[test]
    fn modifiers_must_follow_hit() {
        assert_eq!(expand_string("!0.5").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(expand_string("@3").unwrap_err().kind, ErrorKind::Parse);
        assert_eq!(
            expand_string("x!0.5@3@1").unwrap_err().kind,
            ErrorKind::Parse
        );
    }

    #[test]
    fn sample_semitone_ok_but_synth_rejects() {
        let (_, steps) = bar_triggers(
            &Expr::PatternString("x@2".into(), Span { line: 1, col: 1 }),
            60,
            true,
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
        let err = bar_triggers(
            &Expr::PatternString("x@2".into(), Span { line: 1, col: 1 }),
            60,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
    }
}
