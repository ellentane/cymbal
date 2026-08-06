use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{Combinator, Expr, Param, Program, Stmt, VoiceKind};
use crate::error::{Error, ErrorKind, Result};
use crate::pattern;
use crate::pattern::bar_triggers;
use crate::transport::Transport;

pub fn voice_default_duration(kind: VoiceKind) -> u64 {
    match kind {
        VoiceKind::Kick => 14400,
        VoiceKind::Snare => 7200,
        VoiceKind::Hat => 2400,
        VoiceKind::Bass => 14400,
        VoiceKind::Lead => 9600,
        VoiceKind::Sample => 0,
    }
}

pub fn voice_default_pitch(kind: VoiceKind) -> Option<u8> {
    match kind {
        VoiceKind::Kick | VoiceKind::Snare | VoiceKind::Hat => None,
        VoiceKind::Bass | VoiceKind::Lead => Some(60),
        VoiceKind::Sample => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SampleData {
    pub frames: Arc<Vec<f32>>,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub sample_offset: u64,
    pub loop_name: String,
    pub loop_index: u32,
    pub voice: VoiceKind,
    pub pitch: Option<u8>,
    pub semitone: i32,
    pub velocity: f32,
    pub duration: u64,
    pub generation: u64,
    pub pan: f32,
    pub delay_send: f32,
    pub reverb_send: f32,
    pub bass: f32,
    pub treble: f32,
    pub comp: f32,
    pub sample: Option<Arc<SampleData>>,
    pub sample_start: f64,
    pub sample_end: f64,
    pub sample_loop: bool,
}

#[derive(Debug, Clone)]
pub struct Timeline {
    pub events: Vec<Event>,
    pub generation: u64,
    pub tempo: f64,
    pub bar_samples: u64,
    pub sample_rate: u32,
    pub loops: Vec<String>,
    pub loop_generations: Vec<(String, u64)>,
    pub midi: Vec<crate::midi::MidiEvent>,
}

pub fn schedule(
    program: &Program,
    loop_generations: &HashMap<String, u64>,
    samples: &HashMap<String, Arc<SampleData>>,
    max_samples: u64,
    sample_rate: u32,
) -> Result<Timeline> {
    let tempo = program
        .statements
        .iter()
        .find_map(|s| {
            if let Stmt::Tempo(t, _) = s {
                Some(Transport::new(*t, sample_rate).tempo)
            } else {
                None
            }
        })
        .unwrap_or(120.0);
    let transport = Transport::new(tempo, sample_rate);
    let transport_bar = transport.bar_samples();

    let mut events = Vec::new();
    let mut loops = Vec::new();
    let mut loop_gens = Vec::new();
    let mut max_generation = 0;
    let mut seen_loops = std::collections::HashSet::new();
    let auto_generation = loop_generations.values().max().copied().unwrap_or(0) + 1;

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
        let generation = loop_generations
            .get(&loop_stmt.name)
            .copied()
            .unwrap_or(auto_generation);
        max_generation = max_generation.max(generation);
        let loop_index = loops.len() as u32;
        loops.push(loop_stmt.name.clone());
        loop_gens.push((loop_stmt.name.clone(), generation));
        let loop_tempo = loop_stmt.tempo.unwrap_or(tempo);
        let bar = Transport::new(loop_tempo, sample_rate).bar_samples();
        let bars = max_samples.div_ceil(bar);

        for bind in &loop_stmt.binds {
            let (voice, sample_data) = match &bind.voice {
                Expr::Voice(voice, _) => (*voice, None),
                Expr::Sample(path, span) => {
                    let data = samples.get(path).ok_or_else(|| {
                        Error::new(
                            *span,
                            ErrorKind::Eval,
                            format!("sample '{path}' not loaded"),
                        )
                    })?;
                    (VoiceKind::Sample, Some(data.clone()))
                }
                _ => {
                    return Err(Error::new(
                        bind.span,
                        ErrorKind::Eval,
                        "left side of '<<' must be a voice",
                    ));
                }
            };
            let default_pitch = voice_default_pitch(voice).unwrap_or(60);
            let (steps, step_list) = bar_triggers(&bind.pattern, default_pitch)?;
            if bars * steps as u64 > 1_000_000 {
                return Err(Error::new(
                    bind.span,
                    ErrorKind::Eval,
                    "pattern too dense for the render window",
                ));
            }
            let step_samples = (bar / steps as u64).max(1);

            for bar_idx in 0..bars {
                let mut steps_vec: Vec<Option<pattern::Step>> = step_list.clone();
                for comb in &bind.combinators {
                    steps_vec = apply_combinator(comb, steps_vec, bar_idx);
                }
                for (step_idx, step) in steps_vec.into_iter().enumerate() {
                    let Some(step) = step else { continue };
                    let mut offset = bar_idx * bar + step_idx as u64 * step_samples;
                    let swing = tone_value(&bind.swing);
                    if swing > 0.0 && steps.is_multiple_of(2) && step_idx % 2 == 1 {
                        let shifted = offset as i64 + (swing * step_samples as f32).round() as i64;
                        let bar_start = (bar_idx * bar) as i64;
                        offset = shifted.clamp(bar_start, bar_start + bar as i64 - 1) as u64;
                    }
                    if offset >= max_samples {
                        continue;
                    }
                    let velocity = (step.velocity * resolve_param(&bind.vel, 1.0, step_idx, steps))
                        .clamp(0.0, 1.0);
                    let cycle_on = tone_value(&bind.cycle) > 0.0;
                    let duration = match voice {
                        VoiceKind::Sample => {
                            let data = sample_data.as_ref().unwrap();
                            let rate = 2f64.powf(step.semitone as f64 / 12.0);
                            if let Some(Param::Const(d)) = bind.dur {
                                (d as f64 * sample_rate as f64).round() as u64
                            } else if cycle_on {
                                max_samples - offset
                            } else {
                                let total = data.frames.len() as f64;
                                let start =
                                    resolve_param(&bind.start, 0.0, step_idx, steps) as f64 * total;
                                let end =
                                    resolve_param(&bind.end, 1.0, step_idx, steps) as f64 * total;
                                if start >= end {
                                    return Err(Error::new(
                                        bind.span,
                                        ErrorKind::Eval,
                                        "sample start must be before end",
                                    ));
                                }
                                (((end - start) * sample_rate as f64 / data.sample_rate as f64)
                                    / rate)
                                    .ceil() as u64
                            }
                        }
                        _ => voice_default_duration(voice),
                    };
                    events.push(Event {
                        sample_offset: offset,
                        loop_name: loop_stmt.name.clone(),
                        loop_index,
                        voice,
                        pitch: if voice == VoiceKind::Sample {
                            None
                        } else {
                            voice_default_pitch(voice).map(|_| step.pitch)
                        },
                        semitone: step.semitone,
                        velocity,
                        duration,
                        generation,
                        pan: resolve_param(&bind.pan, 0.0, step_idx, steps),
                        delay_send: resolve_param(&bind.delay_send, 0.0, step_idx, steps),
                        reverb_send: resolve_param(&bind.reverb_send, 0.0, step_idx, steps),
                        bass: tone_value(&bind.bass),
                        treble: tone_value(&bind.treble),
                        comp: tone_value(&bind.comp),
                        sample: sample_data.clone(),
                        sample_start: resolve_param(&bind.start, 0.0, step_idx, steps) as f64,
                        sample_end: resolve_param(&bind.end, 1.0, step_idx, steps) as f64,
                        sample_loop: cycle_on,
                    });
                }
            }
        }
    }
    events.sort_by_key(|e| e.sample_offset);
    let midi = crate::midi::build_timeline_midi(&Timeline {
        events: events.clone(),
        generation: max_generation,
        tempo,
        bar_samples: transport_bar,
        sample_rate,
        loops: loops.clone(),
        loop_generations: loop_gens.clone(),
        midi: vec![],
    });
    Ok(Timeline {
        events,
        generation: max_generation,
        tempo,
        bar_samples: transport_bar,
        sample_rate,
        loops,
        loop_generations: loop_gens,
        midi,
    })
}

pub fn sample_paths(program: &Program) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in &program.statements {
        let Stmt::Loop(l) = stmt else { continue };
        for bind in &l.binds {
            if let Expr::Sample(path, _) = &bind.voice
                && !out.contains(path)
            {
                out.push(path.clone());
            }
        }
    }
    out
}

fn tone_value(p: &Option<Param>) -> f32 {
    match p {
        Some(Param::Const(v)) => *v,
        _ => 0.0,
    }
}

fn resolve_param(p: &Option<Param>, default: f32, step_idx: usize, steps: usize) -> f32 {
    match p {
        None => default,
        Some(Param::Const(v)) => *v,
        Some(Param::Ramp(a, b)) => {
            if steps <= 1 {
                *b
            } else {
                (*a as f64 + (*b as f64 - *a as f64) * step_idx as f64 / (steps - 1) as f64) as f32
            }
        }
    }
}

fn apply_combinator(
    comb: &Combinator,
    steps: Vec<Option<pattern::Step>>,
    bar_idx: u64,
) -> Vec<Option<pattern::Step>> {
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
        let mut loop_generations = HashMap::new();
        for stmt in &program.statements {
            if let Stmt::Loop(loop_stmt) = stmt {
                loop_generations.insert(loop_stmt.name.clone(), generation);
            }
        }
        schedule(
            &program,
            &loop_generations,
            &HashMap::new(),
            max_samples,
            48000,
        )
        .unwrap()
    }

    fn src2timeline_v11(
        src: &str,
        loop_generations: &HashMap<String, u64>,
        samples: &HashMap<String, Arc<SampleData>>,
        max_samples: u64,
    ) -> Result<Timeline> {
        let program = parse(&lex(src).unwrap()).unwrap();
        schedule(&program, loop_generations, samples, max_samples, 48000)
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
            "let kick = kick()\nlet lead = lead()\nloop \"b\":\n    kick << \"x . x .\"\n    lead << [c4, e4, f2, c2] | rev\n",
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
        let err = schedule(&program, &HashMap::new(), &HashMap::new(), 96000, 48000).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
    }

    #[test]
    fn dense_pattern_at_high_tempo_is_capped() {
        let err = src2timeline_v11(
            "tempo 4000\nlet kick = kick()\nloop \"b\":\n    kick << \"x x x x x x x x x x x x x x x x x\"\n",
            &HashMap::new(),
            &HashMap::new(),
            48000 * 3600,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
        assert!(err.message.contains("dense"), "{}", err.message);
        let tl = src2timeline(
            "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x . x .\"\n",
            0,
            96000,
        );
        assert!(!tl.events.is_empty(), "normal patterns still schedule");
    }

    #[test]
    fn events_carry_params_and_velocity() {
        let tl = src2timeline_v11(
            "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x!0.5 . x\" pan=0.5 vel=0.8 delay=0.2 reverb=0.1\n",
            &HashMap::new(),
            &HashMap::new(),
            96000,
        )
        .unwrap();
        let ev = &tl.events[0];
        assert_eq!(ev.velocity, 0.5 * 0.8);
        assert_eq!(ev.pan, 0.5);
        assert_eq!(ev.delay_send, 0.2);
        assert_eq!(ev.reverb_send, 0.1);
        assert_eq!(ev.loop_name, "b");
        let ev2 = &tl.events[1];
        assert_eq!(ev2.velocity, 0.8, "hits without !n use loop vel only");
    }

    #[test]
    fn per_loop_generations_assigned() {
        let mut gens = HashMap::new();
        gens.insert("b".to_string(), 3u64);
        let tl = src2timeline_v11(
            "let kick = kick()\nlet hat = hat()\nloop \"b\":\n    kick << \"x .\"\nloop \"h\":\n    hat << \"x .\"\n",
            &gens,
            &HashMap::new(),
            96000,
        )
        .unwrap();
        assert_eq!(tl.generation, 4, "max generation");
        assert_eq!(
            tl.loop_generations,
            vec![("b".to_string(), 3), ("h".to_string(), 4)]
        );
        assert!(
            tl.events
                .iter()
                .all(|e| e.generation == 3 || e.generation == 4)
        );
    }

    #[test]
    fn sample_events_get_data_and_duration() {
        let frames: Vec<f32> = vec![0.0; 48000]; // 1s at 48k
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let tl = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x\"\n",
            &HashMap::new(),
            &samples,
            96000,
        )
        .unwrap();
        let ev = &tl.events[0];
        assert_eq!(ev.voice, VoiceKind::Sample);
        assert!(ev.sample.is_some());
        assert_eq!(ev.duration, 48000);
        assert_eq!(ev.semitone, 0);
    }

    #[test]
    fn sample_pitch_shift_scales_duration() {
        let frames: Vec<f32> = vec![0.0; 48000];
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let tl = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x@12\"\n",
            &HashMap::new(),
            &samples,
            96000,
        )
        .unwrap();
        assert_eq!(
            tl.events[0].duration, 24000,
            "@12 = 2x rate = half duration"
        );
    }

    #[test]
    fn missing_sample_is_eval_error() {
        let program =
            parse(&lex("loop \"b\":\n    sample \"nope.wav\" << \"x\"\n").unwrap()).unwrap();
        let err = schedule(&program, &HashMap::new(), &HashMap::new(), 96000, 48000).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
    }

    #[test]
    fn sample_paths_are_reported() {
        let program = parse(
            &lex(
                "let kick = kick()\nloop \"b\":\n    kick << \"x\"\n    sample \"a.wav\" << \"x\"\nloop \"c\":\n    sample \"b.wav\" << \"x\"\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            sample_paths(&program),
            vec!["a.wav".to_string(), "b.wav".to_string()]
        );
    }

    #[test]
    fn polyrhythm_phases_exact() {
        // loop a: "x . x . x . x ." = 8 steps, 4 hits/bar -> 28 hits in 7 bars.
        // loop b: "x . . . x . ." = 7 steps, hits at steps 0 and 4.
        // 96000/7 truncates to 13714 (7*13714 = 95998, not 96000): the 2-sample
        // seam per bar is deliberate integer truncation, pinned exactly here.
        // step 4 offset = 4*13714 = 54856; each bar adds 96000.
        let tl = src2timeline(
            "tempo 120\nlet kick = kick()\nlet hat = hat()\nloop \"a\":\n    kick << \"x . x . x . x .\"\nloop \"b\":\n    hat << \"x . . . x . .\"\n",
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
        assert_eq!(
            kick,
            vec![
                0, 24000, 48000, 72000, 96000, 120000, 144000, 168000, 192000, 216000, 240000,
                264000, 288000, 312000, 336000, 360000, 384000, 408000, 432000, 456000, 480000,
                504000, 528000, 552000, 576000, 600000, 624000, 648000
            ]
        );
        assert_eq!(
            hat,
            vec![
                0, 54856, 96000, 150856, 192000, 246856, 288000, 342856, 384000, 438856, 480000,
                534856, 576000, 630856
            ]
        );
    }

    #[test]
    fn every_n_rev_flips_cycle_boundaries() {
        // "x . x ." = 4 steps, step_samples 24000: hits at steps 0,2 -> 0, 48000.
        // every(4) triggers on bar_idx 3; rev hits steps 1,3 -> bar_start + 24000, +72000.
        let tl = src2timeline(
            "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x . x .\" | every(4, rev)\n",
            0,
            384000,
        );
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(
            offsets,
            vec![0, 48000, 96000, 144000, 192000, 240000, 312000, 360000]
        );
    }

    #[test]
    fn rev_reverses_pitches_within_bar() {
        let tl = src2timeline(
            "let lead = lead()\nloop \"b\":\n    lead << [c4, e4, g4] | rev\n",
            0,
            96000,
        );
        let pitches: Vec<Option<u8>> = tl
            .events
            .iter()
            .filter(|e| e.voice == VoiceKind::Lead)
            .map(|e| e.pitch)
            .collect();
        assert_eq!(pitches, vec![Some(67), Some(64), Some(60)]);
    }

    #[test]
    fn rev_reverses_rhythm() {
        let tl = src2timeline(
            "let kick = kick()\nloop \"b\":\n    kick << \". x . x\" | rev\n",
            0,
            96000,
        );
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![0, 48000]);
    }

    #[test]
    fn every_n_rev_applies_on_nth_cycle() {
        let tl = src2timeline(
            "let lead = lead()\nloop \"b\":\n    lead << [c4, e4] | every(2, rev)\n",
            0,
            96000 * 4,
        );
        let by_bar: Vec<Vec<u8>> = tl
            .events
            .iter()
            .filter(|e| e.voice == VoiceKind::Lead)
            .fold(
                vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                |mut acc, e| {
                    let bar = (e.sample_offset / 96000) as usize;
                    acc[bar].push(e.pitch.unwrap());
                    acc
                },
            );
        assert_eq!(by_bar[0], vec![60, 64]); // cycle 1: normal
        assert_eq!(by_bar[1], vec![64, 60]); // cycle 2: reversed
        assert_eq!(by_bar[2], vec![60, 64]);
        assert_eq!(by_bar[3], vec![64, 60]);
    }

    #[test]
    fn timeline_records_loop_names_in_order() {
        let tl = src2timeline(
            "let kick = kick()\nlet hat = hat()\nloop \"b\":\n    kick << \"x . x .\"\nloop \"h\":\n    hat << \"x x x x\"\n",
            0,
            96000,
        );
        assert_eq!(tl.loops, vec!["b".to_string(), "h".to_string()]);
    }

    #[test]
    fn zero_and_negative_tempo_clamped_in_timeline() {
        for src in [
            "tempo 0\nlet kick = kick()\nloop \"b\":\n    kick << \"x\"\n",
            "tempo -5\nlet kick = kick()\nloop \"b\":\n    kick << \"x\"\n",
        ] {
            let tl = src2timeline(src, 0, 96000);
            assert!(
                tl.tempo >= 20.0,
                "timeline tempo must be clamped to the transport range, got {}",
                tl.tempo
            );
        }
    }

    #[test]
    fn per_loop_tempo_changes_bar_length() {
        let tl = src2timeline(
            "tempo 120\nlet kick = kick()\nloop \"b\" tempo=240:\n    kick << \"x x x x\"\n",
            0,
            96000,
        );
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(
            offsets,
            vec![0, 12000, 24000, 36000, 48000, 60000, 72000, 84000]
        );
    }

    #[test]
    fn loops_phase_and_realign_at_lcm() {
        let tl = src2timeline(
            "tempo 120\nlet kick = kick()\nlet hat = hat()\nloop \"a\":\n    kick << \"x . x .\"\nloop \"b\" tempo=240:\n    hat << \"x . . . . x .\"\n",
            0,
            192000,
        );
        let first: Vec<u64> = tl
            .events
            .iter()
            .filter(|e| e.sample_offset < 96000)
            .map(|e| e.sample_offset)
            .collect();
        let second: Vec<u64> = tl
            .events
            .iter()
            .filter(|e| e.sample_offset >= 96000)
            .map(|e| e.sample_offset - 96000)
            .collect();
        assert_eq!(
            first, second,
            "composite rhythm must repeat every lcm samples"
        );
        assert_eq!(first, vec![0, 0, 34285, 48000, 48000, 82285]);
        assert_eq!(tl.events.iter().filter(|e| e.sample_offset == 0).count(), 2);
    }

    #[test]
    fn bind_defaults_when_params_omitted() {
        let tl = src2timeline_v11(
            "let kick = kick()\nlet lead = lead()\nloop \"b\":\n    kick << \"x\"\n    lead << [c4]\n",
            &HashMap::new(),
            &HashMap::new(),
            96000,
        )
        .unwrap();
        let kick = tl
            .events
            .iter()
            .find(|e| e.voice == VoiceKind::Kick)
            .unwrap();
        assert_eq!(kick.pan, 0.0);
        assert_eq!(kick.delay_send, 0.0);
        assert_eq!(kick.reverb_send, 0.0);
        assert!(kick.sample.is_none());
        assert_eq!(kick.semitone, 0);
        assert_eq!(kick.velocity, 1.0);
        assert_eq!(kick.pitch, None, "kick is unpitched by default");
        let lead = tl
            .events
            .iter()
            .find(|e| e.voice == VoiceKind::Lead)
            .unwrap();
        assert_eq!(lead.pitch, Some(60), "pitched voices default to c4");
    }

    #[test]
    fn empty_map_gives_all_loops_one_generation() {
        let tl = src2timeline_v11(
            "let kick = kick()\nlet hat = hat()\nloop \"b\":\n    kick << \"x .\"\nloop \"h\":\n    hat << \"x .\"\n",
            &HashMap::new(),
            &HashMap::new(),
            96000,
        )
        .unwrap();
        assert!(!tl.events.is_empty());
        let shared = tl.events[0].generation;
        assert!(
            tl.events.iter().all(|e| e.generation == shared),
            "all loops must share one generation so every loop stays audible"
        );
        assert_eq!(tl.generation, shared);
        assert_eq!(
            tl.loop_generations,
            vec![("b".to_string(), shared), ("h".to_string(), shared)]
        );
    }

    #[test]
    fn ramp_params_resolve_per_step() {
        let tl = src2timeline_v11(
            "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x x x x\" pan=0:1 vel=1:0\n",
            &HashMap::new(),
            &HashMap::new(),
            96000,
        )
        .unwrap();
        let pans: Vec<f32> = tl.events.iter().map(|e| e.pan).collect();
        let vels: Vec<f32> = tl.events.iter().map(|e| e.velocity).collect();
        assert_eq!(pans, vec![0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]);
        assert_eq!(vels, vec![1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0]);
    }

    #[test]
    fn single_step_ramp_uses_end_value() {
        let tl = src2timeline_v11(
            "let kick = kick()\nloop \"b\":\n    kick << \"x\" pan=0:1\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        assert_eq!(tl.events[0].pan, 1.0);
    }

    #[test]
    fn tone_params_reach_events() {
        let tl = src2timeline_v11(
            "let kick = kick()\nloop \"b\":\n    kick << \"x\" bass=0.5 treble=0.25 comp=0.1\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        assert_eq!(tl.events[0].bass, 0.5);
        assert_eq!(tl.events[0].treble, 0.25);
        assert_eq!(tl.events[0].comp, 0.1);
    }

    #[test]
    fn swing_delays_off_beats() {
        // 8 steps at 480bpm/48k: bar 24000, step 3000; swing 0.25 -> odd steps +750.
        let tl = src2timeline_v11(
            "tempo 480\nlet kick = kick()\nloop \"b\":\n    kick << \"x x x x x x x x\" swing=0.25\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(
            offsets,
            vec![0, 3750, 6000, 9750, 12000, 15750, 18000, 21750]
        );
    }

    #[test]
    fn swing_shifts_off_beat_hits() {
        // ". x . x" hits on odd steps 1,3 -> 6000 and 18000 at bar 24000, step 6000; swing 0.25 -> +1500.
        let tl = src2timeline_v11(
            "tempo 480\nlet kick = kick()\nloop \"b\":\n    kick << \". x . x\" swing=0.25\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![7500, 19500]);
    }

    #[test]
    fn swing_leaves_even_steps_alone() {
        let tl = src2timeline_v11(
            "tempo 480\nlet kick = kick()\nloop \"b\":\n    kick << \"x . x .\" swing=0.5\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![0, 12000]);
    }

    #[test]
    fn swing_acts_on_rev_swapped_positions() {
        // rev maps step i -> 3-i: ". x . x" -> hits at 0,2 (even, unswung).
        let tl = src2timeline_v11(
            "tempo 480\nlet kick = kick()\nloop \"b\":\n    kick << \". x . x\" | rev swing=0.25\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![0, 12000]);
        // "x . x ." -> rev -> hits at 1,3 (odd, swung +1500).
        let tl = src2timeline_v11(
            "tempo 480\nlet kick = kick()\nloop \"b\":\n    kick << \"x . x .\" | rev swing=0.25\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        let offsets: Vec<u64> = tl.events.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![7500, 19500]);
    }

    #[test]
    fn sample_region_scales_duration() {
        let frames: Vec<f32> = vec![0.0; 48000]; // 1s at 48k
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let tl = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x\" start=0.25 end=0.75\n",
            &HashMap::new(),
            &samples,
            96000,
        )
        .unwrap();
        assert_eq!(tl.events[0].duration, 24000, "half the file");
        assert_eq!(tl.events[0].sample_start, 0.25);
        assert_eq!(tl.events[0].sample_end, 0.75);
    }

    #[test]
    fn sample_dur_overrides_duration() {
        let frames: Vec<f32> = vec![0.0; 48000];
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let tl = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x\" dur=0.5\n",
            &HashMap::new(),
            &samples,
            96000,
        )
        .unwrap();
        assert_eq!(tl.events[0].duration, 24000, "0.5s at 48k");
    }

    #[test]
    fn sample_cycle_runs_to_window_end() {
        let frames: Vec<f32> = vec![0.0; 4800];
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let tl = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x\" cycle=1\n",
            &HashMap::new(),
            &samples,
            48000,
        )
        .unwrap();
        assert_eq!(tl.events[0].duration, 48000, "loops until the window end");
        assert!(tl.events[0].sample_loop);
    }

    #[test]
    fn sample_cycle_value_gates_looping() {
        let frames: Vec<f32> = vec![0.0; 48000];
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let tl = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x\" cycle=0 dur=0.2\n",
            &HashMap::new(),
            &samples,
            96000,
        )
        .unwrap();
        assert!(!tl.events[0].sample_loop, "cycle=0 disables looping");
        assert_eq!(tl.events[0].duration, 9600, "0.2s at 48k");
        let tl = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x\" cycle=1 dur=0.2\n",
            &HashMap::new(),
            &samples,
            96000,
        )
        .unwrap();
        assert!(tl.events[0].sample_loop, "cycle=1 enables looping");
        assert_eq!(tl.events[0].duration, 9600, "dur still wins");
    }

    #[test]
    fn sample_start_before_end_required() {
        let frames: Vec<f32> = vec![0.0; 4800];
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let err = src2timeline_v11(
            "loop \"b\":\n    sample \"kick.wav\" << \"x\" start=0.8 end=0.2\n",
            &HashMap::new(),
            &samples,
            48000,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
    }

    #[test]
    fn events_carry_loop_index() {
        let tl = src2timeline_v11(
            "let kick = kick()\nlet hat = hat()\nloop \"b\":\n    kick << \"x\"\nloop \"h\":\n    hat << \"x\"\n",
            &HashMap::new(),
            &HashMap::new(),
            24000,
        )
        .unwrap();
        assert!(
            tl.events
                .iter()
                .all(|e| e.loop_index == 0 || e.loop_index == 1)
        );
        assert!(
            tl.events
                .iter()
                .filter(|e| e.loop_name == "b")
                .all(|e| e.loop_index == 0)
        );
        assert!(
            tl.events
                .iter()
                .filter(|e| e.loop_name == "h")
                .all(|e| e.loop_index == 1)
        );
    }
}
