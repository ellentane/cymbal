use crate::dsp::Voice;
use crate::error::Result;
use crate::lexer::lex;
use crate::parser::parse;
use crate::scheduler::schedule;

pub fn render_offline(src: &str, max_samples: u64, sample_rate: u32) -> Result<Vec<f32>> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    let timeline = schedule(&program, 0, max_samples, sample_rate)?;
    let mut out = vec![0.0f32; max_samples as usize];
    for ev in &timeline.events {
        let mut voice = Voice::new(ev.voice, ev.pitch);
        let start = ev.sample_offset as usize;
        let end = (start + ev.duration as usize).min(out.len());
        for slot in out.iter_mut().take(end).skip(start) {
            match voice.next_sample(sample_rate) {
                Some(s) => *slot += s,
                None => break,
            }
        }
    }
    for s in &mut out {
        *s = s.tanh();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn renders_deterministically() {
        let src = "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x . . x . . x .\"\n";
        let a = render_offline(src, 384000, 48000).unwrap();
        let b = render_offline(src, 384000, 48000).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn empty_program_is_digital_black() {
        let out = render_offline("", 48000, 48000).unwrap();
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn all_samples_are_finite() {
        let src = "tempo 120\nlet kick = kick()\nlet snare = snare()\nlet hat = hat()\nlet bass = bass()\nlet lead = lead()\nloop \"b\":\n    kick << \"x . . x . . x .\"\n    snare << \"x\" >> every(4, rev)\n    hat << \"x . x . x . x .\"\n    bass << ([c2, f2], \"x . . x\")\n    lead << [c4, e4, g4] >> rev\n";
        let out = render_offline(src, 384000, 48000).unwrap();
        assert!(out.iter().all(|s| s.is_finite()));
        let peak = out.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak > 0.1);
    }

    #[test]
    fn error_propagates() {
        assert_eq!(
            render_offline("loop \"a\":\n    kick << \"x y\"\n", 48000, 48000)
                .unwrap_err()
                .kind,
            ErrorKind::Parse
        );
    }
}
