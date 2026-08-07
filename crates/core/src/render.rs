use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::Stmt;
use crate::dsp::Voice;
use crate::error::Result;
use crate::lexer::lex;
use crate::mixer::{Master, VoiceOutput};
use crate::parser::parse;
use crate::scheduler::{SampleData, Timeline, schedule, schedule_window};

pub const STREAM_WINDOW_LEN: u64 = 300 * 48000;

const CHUNK_FRAMES: usize = 48000;

pub fn render_offline(
    src: &str,
    max_samples: u64,
    sample_rate: u32,
    samples: &HashMap<String, Arc<SampleData>>,
) -> Result<Vec<f32>> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    let timeline = schedule(&program, &HashMap::new(), samples, max_samples, sample_rate)?;
    Ok(render_timeline(&timeline, max_samples, sample_rate))
}

/// Streams `max_samples` frames of stereo audio in `out` callbacks, rendering
/// the source window-by-window (STREAM_WINDOW_LEN) with the mixer state and
/// ringing voices carried across windows, so render length is unbounded.
pub fn render_offline_streaming(
    src: &str,
    max_samples: u64,
    sample_rate: u32,
    samples: &HashMap<String, Arc<SampleData>>,
    out: &mut impl FnMut(&[f32]),
) -> Result<()> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    render_program_streaming(
        &program,
        samples,
        max_samples,
        sample_rate,
        STREAM_WINDOW_LEN,
        out,
    )
}

struct Active {
    until: u64,
    voice: Voice,
    velocity: f32,
    pan: f32,
    delay_send: f32,
    reverb_send: f32,
    loop_name: String,
    generation: u64,
}

fn render_program_streaming(
    program: &crate::ast::Program,
    samples: &HashMap<String, Arc<SampleData>>,
    max_samples: u64,
    sample_rate: u32,
    window_len: u64,
    out: &mut impl FnMut(&[f32]),
) -> Result<()> {
    let mut names = Vec::new();
    for stmt in &program.statements {
        if let Stmt::Loop(l) = stmt {
            names.push(l.name.clone());
        }
    }
    // One fixed generation per loop: unchanged across windows, so a voice
    // ringing across a boundary survives the swap exactly like the single
    // shot, where generation is uniform and unused by the mixer.
    let generations: HashMap<String, u64> = names.into_iter().map(|n| (n, 0)).collect();
    let mut master: Option<Master> = None;
    let mut active: Vec<Active> = Vec::new();
    let mut buf: Vec<f32> = Vec::with_capacity(CHUNK_FRAMES * 2);
    let mut window_start = 0u64;
    while window_start < max_samples {
        let len = (max_samples - window_start).min(window_len);
        let tl = schedule_window(
            program,
            &generations,
            samples,
            window_start,
            len,
            sample_rate,
        )?;
        if master.is_none() {
            master = Some(Master::new(sample_rate, tl.bar_samples));
        }
        let master = master.as_mut().unwrap();
        master.set_bar_samples(tl.bar_samples);
        active.retain(|a| {
            tl.loops
                .iter()
                .position(|n| *n == a.loop_name)
                .map(|i| tl.loop_generations[i].1 == a.generation)
                .unwrap_or(false)
        });
        let mut idx = 0usize;
        let mut frame = 0u64;
        while frame < len {
            let chunk = ((len - frame) as usize).min(CHUNK_FRAMES);
            buf.clear();
            for i in 0..chunk {
                let now = window_start + frame + i as u64;
                master.begin_frame();
                while idx < tl.events.len() && window_start + tl.events[idx].sample_offset <= now {
                    let ev = &tl.events[idx];
                    idx += 1;
                    active.push(Active {
                        until: now + ev.duration,
                        voice: Voice::new(
                            ev.voice,
                            crate::dsp::VoiceParams::from_event(ev),
                            sample_rate,
                        ),
                        velocity: ev.velocity,
                        pan: ev.pan,
                        delay_send: ev.delay_send,
                        reverb_send: ev.reverb_send,
                        loop_name: ev.loop_name.clone(),
                        generation: ev.generation,
                    });
                }
                active.retain(|a| a.until > now);
                for a in &mut active {
                    if let Some(s) = a.voice.next_sample(sample_rate) {
                        master.add_voice(VoiceOutput {
                            sample: s,
                            velocity: a.velocity,
                            pan: a.pan,
                            delay_send: a.delay_send,
                            reverb_send: a.reverb_send,
                        });
                    }
                }
                let mut frame_out = [0.0f32; 2];
                master.end_frame(&mut frame_out);
                buf.push(frame_out[0]);
                buf.push(frame_out[1]);
            }
            out(&buf);
            frame += chunk as u64;
        }
        window_start += len;
    }
    Ok(())
}

fn render_timeline(timeline: &Timeline, max_samples: u64, sample_rate: u32) -> Vec<f32> {
    let mut master = Master::new(sample_rate, timeline.bar_samples);
    let mut out = vec![0.0f32; max_samples as usize * 2];
    let events = timeline.events.clone();
    let mut idx = 0usize;
    let mut active: Vec<(u64, Voice, crate::scheduler::Event)> = Vec::new();
    for frame in 0..max_samples {
        master.begin_frame();
        while idx < events.len() && events[idx].sample_offset <= frame {
            let ev = events[idx].clone();
            idx += 1;
            active.push((
                frame + ev.duration,
                Voice::new(
                    ev.voice,
                    crate::dsp::VoiceParams::from_event(&ev),
                    sample_rate,
                ),
                ev,
            ));
        }
        active.retain(|(until, _, _)| *until > frame);
        for (_, voice, ev) in &mut active {
            if let Some(s) = voice.next_sample(sample_rate) {
                master.add_voice(VoiceOutput {
                    sample: s,
                    velocity: ev.velocity,
                    pan: ev.pan,
                    delay_send: ev.delay_send,
                    reverb_send: ev.reverb_send,
                });
            }
        }
        let mut frame_out = [0.0f32; 2];
        master.end_frame(&mut frame_out);
        out[frame as usize * 2] = frame_out[0];
        out[frame as usize * 2 + 1] = frame_out[1];
    }
    out
}

/// ("master", full mix) + ("<loop>", dry stem) per loop, in declaration order.
pub fn render_offline_tracks(
    src: &str,
    max_samples: u64,
    sample_rate: u32,
    samples: &HashMap<String, Arc<SampleData>>,
) -> Result<Vec<(String, Vec<f32>)>> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    let timeline = schedule(&program, &HashMap::new(), samples, max_samples, sample_rate)?;
    let mut out = vec![(
        "master".to_string(),
        render_timeline(&timeline, max_samples, sample_rate),
    )];
    for loop_name in &timeline.loops {
        let mut stem = timeline.clone();
        stem.events.retain(|e| &e.loop_name == loop_name);
        for ev in &mut stem.events {
            ev.delay_send = 0.0;
            ev.reverb_send = 0.0;
        }
        out.push((
            loop_name.clone(),
            render_timeline(&stem, max_samples, sample_rate),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::scheduler::SampleData;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn render(src: &str, max_samples: u64) -> Vec<f32> {
        render_offline(src, max_samples, 48000, &HashMap::new()).unwrap()
    }

    #[test]
    fn streamed_render_identical_to_single_shot() {
        // Constant-tempo, non-cycle source (beat.cym). A single-shot 8-bar
        // render must equal the streamed render split over two 4-bar windows
        // (Master + ringing voices carried across the boundary).
        let src = include_str!("../../../examples/beat.cym");
        let single = render_offline(src, 768000, 48000, &HashMap::new()).unwrap();
        let program = parse(&lex(src).unwrap()).unwrap();
        let mut streamed = Vec::new();
        render_program_streaming(
            &program,
            &HashMap::new(),
            768000,
            48000,
            384000,
            &mut |chunk| streamed.extend_from_slice(chunk),
        )
        .unwrap();
        assert_eq!(single, streamed);
    }

    #[test]
    fn renders_deterministically() {
        let src = "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x . . x . . x .\"\n";
        let a = render(src, 384000);
        let b = render(src, 384000);
        assert_eq!(a, b);
    }

    #[test]
    fn output_is_stereo_interleaved() {
        let out = render("", 100);
        assert_eq!(out.len(), 200, "2 channels");
    }

    #[test]
    fn empty_program_is_digital_black() {
        let out = render("", 48000);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn all_samples_are_finite() {
        let src = "tempo 120\nlet kick = kick()\nlet snare = snare()\nlet hat = hat()\nlet bass = bass()\nlet lead = lead()\nloop \"b\":\n    kick << \"x . . x . . x .\"\n    snare << \"x\" | every(4, rev)\n    hat << \"x . x . x . x .\"\n    bass << [c2, f2] \"x . . x\"\n    lead << [c4, e4, g4] | rev\n";
        let out = render(src, 384000);
        assert!(out.iter().all(|s| s.is_finite()));
        let peak = out.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak > 0.1);
    }

    #[test]
    fn pan_places_signal_off_center() {
        let src = "let lead = lead()\nloop \"b\":\n    lead << [c4] pan=1.0\n";
        let out = render(src, 48000);
        let l = out[2000];
        let r = out[2001];
        assert!(
            r.abs() > l.abs() * 10.0,
            "pan=1 must favor right: l={l} r={r}"
        );
    }

    #[test]
    fn sample_renders_and_downmixed_mono_ok() {
        let frames: Vec<f32> = vec![1.0; 48000];
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        let mut samples = HashMap::new();
        samples.insert("kick.wav".to_string(), Arc::new(data));
        let src = "loop \"b\":\n    sample \"kick.wav\" << \"x\" pan=-1.0\n";
        let out = render_offline(src, 48000, 48000, &samples).unwrap();
        assert!(out[0].abs() > 0.1, "sample should be audible on left");
        assert!(out[1].abs() < 0.01, "pan=-1 keeps it off the right");
    }

    #[test]
    fn error_propagates() {
        assert_eq!(
            render_offline(
                "loop \"a\":\n    kick << \"x y\"\n",
                48000,
                48000,
                &HashMap::new()
            )
            .unwrap_err()
            .kind,
            ErrorKind::Parse
        );
    }

    #[test]
    fn sends_reach_delay_and_reverb() {
        // 120 bpm at 48k: 4/4 bar = 96000 samples, delay tap = 0.75 * 96000 = 72000.
        // Render 96000 = one bar, so a single kick hits at frame 0; the dry kick
        // (duration 14400) has ended long before either send window.
        let src_plain = "let kick = kick()\nloop \"b\":\n    kick << \"x\"\n";
        let src_delay = "let kick = kick()\nloop \"b\":\n    kick << \"x\" delay=1.0\n";
        let src_rev = "let kick = kick()\nloop \"b\":\n    kick << \"x\" reverb=1.0\n";
        let plain = render(src_plain, 96000);
        let delayed = render(src_delay, 96000);
        let reverb = render(src_rev, 96000);
        let window_has_energy = |out: &[f32], lo: usize, hi: usize, threshold: f32| {
            out[lo * 2..hi * 2].iter().any(|s| s.abs() > threshold)
        };
        assert!(
            window_has_energy(&plain, 0, 16, 0.05),
            "dry kick must be audible at the start"
        );
        assert!(
            window_has_energy(&delayed, 0, 16, 0.05),
            "delayed render keeps the dry hit"
        );
        assert!(
            window_has_energy(&reverb, 0, 16, 0.05),
            "reverb render keeps the dry hit"
        );
        assert!(
            !window_has_energy(&plain, 72000, 72020, 0.05),
            "dry kick must end before the tap window"
        );
        assert!(
            window_has_energy(&delayed, 72000, 72020, 0.05),
            "delay tap reads one sample late; the first audible echo is the kick's t=1 sample at frame 72001"
        );
        assert!(
            !window_has_energy(&plain, 20000, 30000, 1e-3),
            "dry kick must end before the reverb window"
        );
        assert!(
            window_has_energy(&reverb, 20000, 30000, 1e-3),
            "comb feedback must keep the reverb tail audible"
        );
    }

    #[test]
    fn tone_params_change_output_but_bypass_is_identical() {
        let src = "let kick = kick()\nloop \"b\":\n    kick << \"x . . x . . x .\"\n";
        let plain = render(src, 384000);
        let shaped = render(
            "let kick = kick()\nloop \"b\":\n    kick << \"x . . x . . x .\" bass=1.0 treble=0.5 comp=0.3\n",
            384000,
        );
        assert_ne!(plain, shaped, "shaping must change the audio");
        let with_zero = render(
            "let kick = kick()\nloop \"b\":\n    kick << \"x . . x . . x .\" bass=0 treble=0 comp=0\n",
            384000,
        );
        assert_eq!(plain, with_zero, "zeroed params must be bit-identical");
    }

    #[test]
    fn automation_ramp_pans_across_bar() {
        // 8 steps over one 48000-sample bar: step 6000; lead duration 9600.
        // At frame 2000 the active voices are step 0 (pan -1) -> L. At frame
        // 34000 the active voice is step 5 (pan -1 + 2*5/7 = 0.43) -> R.
        let src = "tempo 240\nlet lead = lead()\nloop \"b\":\n    lead << \"x x x x x x x x\" pan=-1..1\n";
        let out = render(src, 48000);
        let l = out[2000 * 2];
        let r = out[2000 * 2 + 1];
        assert!(l > r, "early steps favor the left: {l} vs {r}");
        let l2 = out[34000 * 2];
        let r2 = out[34000 * 2 + 1];
        assert!(r2 > l2, "late steps favor the right: {l2} vs {r2}");
    }

    #[test]
    fn tracks_are_dry_per_loop() {
        let src = "tempo 120\nlet kick = kick()\nlet hat = hat()\nloop \"k\":\n    kick << \"x\" delay=1.0\nloop \"h\":\n    hat << \"x\" reverb=1.0\n";
        let tracks = render_offline_tracks(src, 96000, 48000, &HashMap::new()).unwrap();
        let names: Vec<String> = tracks.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            names,
            vec!["master".to_string(), "k".to_string(), "h".to_string()]
        );
        let (_, master) = &tracks[0];
        let (_, k) = &tracks[1];
        let (_, h) = &tracks[2];
        assert!(
            master[0..16].iter().any(|s| s.abs() > 0.05),
            "master has audio"
        );
        assert!(
            k[0..16].iter().any(|s| s.abs() > 0.05),
            "kick stem has audio"
        );
        assert!(
            k[72000 * 2..72000 * 2 + 20].iter().all(|s| s.abs() < 1e-4),
            "kick stem has no delay tail"
        );
        assert!(
            h[20000 * 2..30000 * 2].iter().all(|s| s.abs() < 1e-3),
            "hat stem has no reverb tail"
        );
    }

    #[test]
    fn tracks_are_empty_for_empty_program() {
        let tracks = render_offline_tracks("", 4800, 48000, &HashMap::new()).unwrap();
        assert_eq!(tracks.len(), 1, "only the master");
        assert!(tracks[0].1.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn stems_are_filtered_per_loop() {
        let src = "tempo 120\nlet kick = kick()\nlet hat = hat()\nloop \"k\":\n    kick << \"x\"\nloop \"h\":\n    hat << \"x . . . . . . x\"\n";
        let tracks = render_offline_tracks(src, 96000, 48000, &HashMap::new()).unwrap();
        let (_, k) = &tracks[1];
        let (_, h) = &tracks[2];
        // hat hits at frames 0 and 84000 (8 steps of 12000)
        assert!(
            h[84000 * 2..84000 * 2 + 16].iter().any(|s| s.abs() > 0.05),
            "hat stem has its late hit"
        );
        assert!(
            k[84000 * 2..84000 * 2 + 16].iter().all(|s| s.abs() < 1e-4),
            "kick stem must not contain the hat's late hit"
        );
    }
}
