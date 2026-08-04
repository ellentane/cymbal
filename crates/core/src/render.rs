use std::collections::HashMap;
use std::sync::Arc;

use crate::dsp::Voice;
use crate::error::Result;
use crate::lexer::lex;
use crate::mixer::{Master, VoiceOutput};
use crate::parser::parse;
use crate::scheduler::{SampleData, schedule};

pub fn render_offline(
    src: &str,
    max_samples: u64,
    sample_rate: u32,
    samples: &HashMap<String, Arc<SampleData>>,
) -> Result<Vec<f32>> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    let timeline = schedule(&program, &HashMap::new(), samples, max_samples, sample_rate)?;
    let mut master = Master::new(sample_rate, timeline.bar_samples);
    let mut out = vec![0.0f32; max_samples as usize * 2];
    let events = timeline.events;
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
        let src = "tempo 120\nlet kick = kick()\nlet snare = snare()\nlet hat = hat()\nlet bass = bass()\nlet lead = lead()\nloop \"b\":\n    kick << \"x . . x . . x .\"\n    snare << \"x\" >> every(4, rev)\n    hat << \"x . x . x . x .\"\n    bass << ([c2, f2], \"x . . x\")\n    lead << [c4, e4, g4] >> rev\n";
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
        let src =
            "tempo 240\nlet lead = lead()\nloop \"b\":\n    lead << \"x x x x x x x x\" pan=-1:1\n";
        let out = render(src, 48000);
        let l = out[2000 * 2];
        let r = out[2000 * 2 + 1];
        assert!(l > r, "early steps favor the left: {l} vs {r}");
        let l2 = out[34000 * 2];
        let r2 = out[34000 * 2 + 1];
        assert!(r2 > l2, "late steps favor the right: {l2} vs {r2}");
    }
}
