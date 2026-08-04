use cymbal_core::ast::VoiceKind;
use cymbal_core::dsp::{Voice, VoiceParams};
use cymbal_core::wav::encode_wav;
use std::path::Path;

fn render(kind: VoiceKind, seconds: f64) -> Vec<f32> {
    let mut v = Voice::new(kind, VoiceParams::default_for(kind, None), 48000);
    let n = (seconds * 48000.0) as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        match v.next_sample(48000) {
            Some(s) => out.push(s),
            None => break,
        }
    }
    out
}

fn main() {
    let dir = Path::new("assets/samples");
    std::fs::create_dir_all(dir).unwrap();
    for (name, kind, seconds) in [
        ("kick.wav", VoiceKind::Kick, 0.3),
        ("snare.wav", VoiceKind::Snare, 0.3),
        ("hat.wav", VoiceKind::Hat, 0.2),
        ("clap.wav", VoiceKind::Snare, 0.4),
        ("loop.wav", VoiceKind::Hat, 0.5),
    ] {
        let s = render(kind, seconds);
        std::fs::write(dir.join(name), encode_wav(&s, 48000, 1)).unwrap();
        println!("wrote {}/{} ({} frames)", dir.display(), name, s.len());
    }
}
