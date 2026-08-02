use crate::ast::{Combinator, Expr, Program, Stmt, VoiceKind};
use crate::error::{Error, ErrorKind, Result};
use crate::pattern::bar_triggers;
use crate::transport::Transport;

pub fn voice_default_duration(kind: VoiceKind) -> u64 {
    match kind {
        VoiceKind::Kick => 14400,
        VoiceKind::Snare => 7200,
        VoiceKind::Hat => 2400,
        VoiceKind::Bass => 14400,
        VoiceKind::Lead => 9600,
    }
}

pub fn voice_default_pitch(kind: VoiceKind) -> Option<u8> {
    match kind {
        VoiceKind::Kick | VoiceKind::Snare | VoiceKind::Hat => None,
        VoiceKind::Bass | VoiceKind::Lead => Some(60),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub sample_offset: u64,
    pub voice: VoiceKind,
    pub pitch: Option<u8>,
    pub velocity: f32,
    pub duration: u64,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub struct Timeline {
    pub events: Vec<Event>,
    pub generation: u64,
    pub tempo: f64,
    pub bar_samples: u64,
    pub sample_rate: u32,
}

pub fn schedule(
    program: &Program,
    generation: u64,
    max_samples: u64,
    sample_rate: u32,
) -> Result<Timeline> {
    let tempo = program
        .statements
        .iter()
        .find_map(|s| {
            if let Stmt::Tempo(t, _) = s {
                Some(*t)
            } else {
                None
            }
        })
        .unwrap_or(120.0);
    let transport = Transport::new(tempo, sample_rate);
    let transport_bar = transport.bar_samples();

    let mut events = Vec::new();
    let mut seen_loops = std::collections::HashSet::new();

    for stmt in &program.statements {
        let Stmt::Loop(loop_stmt) = stmt else {
            continue;
        };
        if !seen_loops.insert(loop_stmt.name.as_str()) {
            return Err(Error::new(
                loop_stmt.span,
                ErrorKind::Eval,
                format!("duplicate loop name '{}'", loop_stmt.name),
            ));
        }
        let loop_tempo = loop_stmt.tempo.unwrap_or(tempo);
        let bar = Transport::new(loop_tempo, sample_rate).bar_samples();
        let bars = max_samples.div_ceil(bar);

        for bind in &loop_stmt.binds {
            let Expr::Voice(voice, _) = &bind.voice else {
                return Err(Error::new(
                    bind.span,
                    ErrorKind::Eval,
                    "left side of '<<' must be a voice",
                ));
            };
            let default_pitch = voice_default_pitch(*voice).unwrap_or(60);
            let (steps, base_pitches) = bar_triggers(&bind.pattern, default_pitch)?;
            let step_samples = bar / steps as u64;

            for bar_idx in 0..bars {
                let mut steps_vec: Vec<Option<u8>> = base_pitches.clone();
                for comb in &bind.combinators {
                    steps_vec = apply_combinator(comb, steps_vec, bar_idx);
                }
                for (step_idx, pitch) in steps_vec.into_iter().enumerate() {
                    let Some(pitch) = pitch else { continue };
                    let offset = bar_idx * bar + step_idx as u64 * step_samples;
                    if offset >= max_samples {
                        continue;
                    }
                    events.push(Event {
                        sample_offset: offset,
                        voice: *voice,
                        pitch: (*voice != VoiceKind::Kick
                            && *voice != VoiceKind::Snare
                            && *voice != VoiceKind::Hat)
                            .then_some(pitch),
                        velocity: 1.0,
                        duration: voice_default_duration(*voice),
                        generation,
                    });
                }
            }
        }
    }
    events.sort_by_key(|e| e.sample_offset);
    Ok(Timeline {
        events,
        generation,
        tempo,
        bar_samples: transport_bar,
        sample_rate,
    })
}

fn apply_combinator(comb: &Combinator, steps: Vec<Option<u8>>, bar_idx: u64) -> Vec<Option<u8>> {
    match comb {
        Combinator::Rev => steps.into_iter().rev().collect(),
        Combinator::Every(n, inner) => {
            if (bar_idx + 1).is_multiple_of(*n) {
                apply_combinator(inner, steps, bar_idx)
            } else {
                steps
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;

    fn src2timeline(src: &str, generation: u64, max_samples: u64) -> Timeline {
        let program = parse(&lex(src).unwrap()).unwrap();
        schedule(&program, generation, max_samples, 48000).unwrap()
    }

    #[test]
    fn kicks_at_exact_offsets() {
        // "x . x . x . x ." = 8 steps, hits at 0,2,4,6 -> offsets 0, 24000, 48000, 72000
        let tl = src2timeline(
            "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x . x . x . x .\"\n",
            0,
            96000,
        );
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![0, 24000, 48000, 72000]);
        assert_eq!(tl.events[0].voice, VoiceKind::Kick);
        assert_eq!(tl.events[0].pitch, None);
        assert_eq!(tl.events[0].duration, 14400);
        assert_eq!(tl.events[0].velocity, 1.0);
        assert_eq!(tl.events[0].generation, 0);
        assert_eq!(tl.tempo, 120.0);
        assert_eq!(tl.bar_samples, 96000);
    }

    #[test]
    fn events_are_sorted_and_clamped() {
        // lead [c4, e4, f2, c2] >> rev: 4 steps -> step_samples 24000;
        // offsets [0, 24000, 48000, 72000] -> clamped < 40000 -> [0, 24000];
        // rev reverses pitches: [c2, f2, e4, c4] = [36, 41, 64, 60]
        let tl = src2timeline(
            "let kick = kick()\nlet lead = lead()\nloop \"b\":\n    kick << \"x . x .\"\n    lead << [c4, e4, f2, c2] >> rev\n",
            3,
            40000,
        );
        let windows: Vec<&[Event]> = tl.events.windows(2).collect();
        for w in windows {
            assert!(w[0].sample_offset <= w[1].sample_offset);
        }
        assert!(tl.events.iter().all(|e| e.sample_offset < 40000));
        assert!(tl.events.iter().all(|e| e.generation == 3));
        let lead_offsets: Vec<u64> = tl
            .events
            .iter()
            .filter(|e| e.voice == VoiceKind::Lead)
            .map(|e| e.sample_offset)
            .collect();
        assert_eq!(lead_offsets, vec![0, 24000]);
        let lead_pitches: Vec<Option<u8>> = tl
            .events
            .iter()
            .filter(|e| e.voice == VoiceKind::Lead)
            .map(|e| e.pitch)
            .collect();
        assert_eq!(lead_pitches, vec![Some(36), Some(41)]);
    }

    #[test]
    fn duplicate_loop_names_rejected() {
        let program = parse(
            &lex("loop \"a\":\n    kick << \"x\"\nloop \"a\":\n    kick << \"x\"\n").unwrap(),
        )
        .unwrap();
        let err = schedule(&program, 0, 96000, 48000).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
    }

    #[test]
    fn polyrhythm_phases_by_lcm() {
        // loop a: "x . x . x . x ." = 8 steps, 4 hits/bar -> 28 hits in 7 bars;
        // loop b: 7 steps/bar -> 7 hits, one per bar start.
        // Exact grid relation for hats: o*7 ≡ 0 (mod 96000).
        let tl = src2timeline(
            "tempo 120\nlet kick = kick()\nlet hat = hat()\nloop \"a\":\n    kick << \"x . x . x . x .\"\nloop \"b\":\n    hat << \"x . . . . . .\"\n",
            0,
            96000 * 7,
        );
        let kick: Vec<u64> = tl
            .events
            .iter()
            .filter(|e| e.voice == VoiceKind::Kick)
            .map(|e| e.sample_offset)
            .collect();
        let hat: Vec<u64> = tl
            .events
            .iter()
            .filter(|e| e.voice == VoiceKind::Hat)
            .map(|e| e.sample_offset)
            .collect();
        assert_eq!(kick.len(), 28);
        assert_eq!(hat.len(), 7);
        assert_eq!(kick[0], 0);
        assert_eq!(hat[0], 0);
        assert!(kick.iter().all(|o| o % 24000 == 0));
        assert!(hat.iter().all(|o| (o * 7) % 96000 == 0));
    }
}
