use crate::ast::Expr;
use crate::error::{Error, ErrorKind, Result, Span};

pub fn expand_string(s: &str) -> Result<Vec<bool>> {
    let mut hits = Vec::new();
    for c in s.chars() {
        match c {
            'x' => hits.push(true),
            '.' => hits.push(false),
            ' ' | '\t' => {}
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

pub fn bar_triggers(pattern: &Expr, default_pitch: u8) -> Result<(usize, Vec<Option<u8>>)> {
    match pattern {
        Expr::PatternString(s, _) => {
            let hits = expand_string(s)?;
            Ok((
                hits.len(),
                hits.iter()
                    .map(|h| if *h { Some(default_pitch) } else { None })
                    .collect(),
            ))
        }
        Expr::Notes(notes, _) => {
            let pitches: Vec<Option<u8>> = notes.iter().map(|n| Some(n.midi)).collect();
            Ok((notes.len(), pitches))
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
            let mut hit_index = 0;
            let pitches = hits
                .iter()
                .map(|h| {
                    if *h {
                        let pitch = Some(notes[hit_index % notes.len()].midi);
                        hit_index += 1;
                        pitch
                    } else {
                        None
                    }
                })
                .collect();
            Ok((hits.len(), pitches))
        }
        Expr::Voice(_, span) => Err(Error::new(
            *span,
            ErrorKind::Eval,
            "expected a pattern, got a voice",
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
            vec![true, false, false, true, false, false, true, false]
        );
        assert_eq!(expand_string("x").unwrap(), vec![true]);
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(
            expand_string("  x \t .  x").unwrap(),
            vec![true, false, true]
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
        let (steps, pitches) = bar_triggers(
            &Expr::PatternString("x . x .".into(), Span { line: 1, col: 1 }),
            60,
        )
        .unwrap();
        assert_eq!(steps, 4);
        assert_eq!(pitches, vec![Some(60), None, Some(60), None]);
    }

    #[test]
    fn notes_are_evenly_spaced() {
        let (steps, pitches) = bar_triggers(
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
        assert_eq!(pitches, vec![Some(60), Some(64)]);
    }

    #[test]
    fn tuple_combines_rhythm_and_pitches_cyclically() {
        let (steps, pitches) = bar_triggers(
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
        assert_eq!(pitches, vec![Some(36), None, Some(41), None, Some(36)]);
    }
}
