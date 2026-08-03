use crate::ast::VoiceKind;
use crate::transport::Transport;

fn noise(t: u64) -> f64 {
    let x = t as f64 * 12.9898;
    (x.sin() * 43758.5453).rem_euclid(1.0) * 2.0 - 1.0
}

pub struct Kick {
    t: u64,
    phase: f64,
    dur: u64,
}
pub struct Snare {
    t: u64,
    dur: u64,
}
pub struct Hat {
    t: u64,
    prev_noise: f64,
    dur: u64,
}
pub struct Bass {
    t: u64,
    phase: f64,
    st: (f64, f64, f64, f64),
    freq: f64,
    dur: u64,
}
pub struct Lead {
    t: u64,
    phase: f64,
    st: (f64, f64, f64, f64),
    freq: f64,
    dur: u64,
}

pub enum Voice {
    Kick(Kick),
    Snare(Snare),
    Hat(Hat),
    Bass(Bass),
    Lead(Lead),
}

impl Voice {
    pub fn new(kind: VoiceKind, pitch: Option<u8>) -> Self {
        use crate::scheduler::voice_default_duration;
        let dur = voice_default_duration(kind);
        match kind {
            VoiceKind::Kick => Voice::Kick(Kick {
                t: 0,
                phase: 0.0,
                dur,
            }),
            VoiceKind::Snare => Voice::Snare(Snare { t: 0, dur }),
            VoiceKind::Hat => Voice::Hat(Hat {
                t: 0,
                prev_noise: 0.0,
                dur,
            }),
            VoiceKind::Bass => Voice::Bass(Bass {
                t: 0,
                phase: 0.0,
                st: (0.0, 0.0, 0.0, 0.0),
                freq: Transport::note_frequency(pitch.unwrap_or(60)),
                dur,
            }),
            VoiceKind::Lead => Voice::Lead(Lead {
                t: 0,
                phase: 0.0,
                st: (0.0, 0.0, 0.0, 0.0),
                freq: Transport::note_frequency(pitch.unwrap_or(60)),
                dur,
            }),
        }
    }

    pub fn next_sample(&mut self, sr: u32) -> Option<f32> {
        match self {
            Voice::Kick(k) => k.next_sample(sr),
            Voice::Snare(s) => s.next_sample(sr),
            Voice::Hat(h) => h.next_sample(sr),
            Voice::Bass(b) => b.next_sample(sr),
            Voice::Lead(l) => l.next_sample(sr),
        }
    }
}

fn lp(x: f64, cutoff: f64, q: f64, sr: u32, state: &mut (f64, f64, f64, f64)) -> f64 {
    let (x1, x2, y1, y2) = *state;
    let w0 = 2.0 * std::f64::consts::PI * cutoff / sr as f64;
    let alpha = w0.sin() / (2.0 * q);
    let a0 = 1.0 + alpha;
    let b0 = (1.0 - w0.cos()) / 2.0;
    let b1 = 1.0 - w0.cos();
    let b2 = b0;
    let a1 = -2.0 * w0.cos();
    let a2 = 1.0 - alpha;
    let y = (b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2) / a0;
    *state = (x, x1, y, y1);
    y
}

impl Kick {
    fn next_sample(&mut self, sr: u32) -> Option<f32> {
        if self.t >= self.dur {
            return None;
        }
        let t = self.t as f64;
        let ts = t / sr as f64;
        let sweep = (ts / 0.05).min(1.0);
        let f = 120.0 * (45.0f64 / 120.0).powf(sweep);
        self.phase += 2.0 * std::f64::consts::PI * f / sr as f64;
        let att = (ts / 0.0005).min(1.0);
        let body = self.phase.sin() * (-ts / 0.09).exp() * 1.4 * att;
        let click = 0.2 * (2.0 * std::f64::consts::PI * 8000.0 * ts).sin() * (-ts / 0.005).exp();
        self.t += 1;
        Some((body + click).tanh() as f32)
    }
}

impl Snare {
    fn next_sample(&mut self, sr: u32) -> Option<f32> {
        if self.t >= self.dur {
            return None;
        }
        let ts = self.t as f64 / sr as f64;
        let att = (ts / 0.0005).min(1.0);
        let noise_part = noise(self.t) * (-ts / 0.04).exp() * 0.6 * att;
        let body = (2.0 * std::f64::consts::PI * 180.0 * ts).sin() * (-ts / 0.09).exp() * 0.4;
        self.t += 1;
        Some((noise_part + body).tanh() as f32)
    }
}

impl Hat {
    fn next_sample(&mut self, sr: u32) -> Option<f32> {
        if self.t >= self.dur {
            return None;
        }
        let ts = self.t as f64 / sr as f64;
        let att = (ts / 0.0005).min(1.0);
        let n = noise(self.t);
        let hp = n - self.prev_noise;
        self.prev_noise = n;
        let s = hp * 0.5 * (-ts / 0.015).exp() * att;
        self.t += 1;
        Some(s.tanh() as f32)
    }
}

impl Bass {
    fn next_sample(&mut self, sr: u32) -> Option<f32> {
        if self.t >= self.dur {
            return None;
        }
        let ts = self.t as f64 / sr as f64;
        self.phase += 2.0 * std::f64::consts::PI * self.freq / sr as f64;
        let saw = 2.0 * (self.phase / (2.0 * std::f64::consts::PI)).fract() - 1.0;
        let env = if ts < 0.005 {
            ts / 0.005
        } else {
            0.5 + 0.5 * (-(ts - 0.005) / 0.08).exp()
        };
        let filtered = lp(saw * env * 0.7, 500.0, 0.7, sr, &mut self.st);
        self.t += 1;
        Some(filtered.tanh() as f32)
    }
}

impl Lead {
    fn next_sample(&mut self, sr: u32) -> Option<f32> {
        if self.t >= self.dur {
            return None;
        }
        let ts = self.t as f64 / sr as f64;
        self.phase += 2.0 * std::f64::consts::PI * self.freq / sr as f64;
        let saw = 2.0 * (self.phase / (2.0 * std::f64::consts::PI)).fract() - 1.0;
        let env = if ts < 0.005 {
            ts / 0.005
        } else {
            0.6 + 0.4 * (-(ts - 0.005) / 0.1).exp()
        };
        let filtered = lp(saw * env * 0.6, 2500.0, 1.2, sr, &mut self.st);
        self.t += 1;
        Some(filtered.tanh() as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::voice_default_duration;

    fn render_all(kind: VoiceKind, pitch: Option<u8>) -> Vec<f32> {
        let dur = voice_default_duration(kind) as usize;
        let mut v = Voice::new(kind, pitch);
        let mut out = Vec::with_capacity(dur);
        while let Some(s) = v.next_sample(48000) {
            out.push(s);
        }
        out
    }

    #[test]
    fn every_voice_starts_quiet_peaks_and_returns_to_silence() {
        for kind in [
            VoiceKind::Kick,
            VoiceKind::Snare,
            VoiceKind::Hat,
            VoiceKind::Bass,
            VoiceKind::Lead,
        ] {
            let s = render_all(
                kind,
                if kind == VoiceKind::Bass || kind == VoiceKind::Lead {
                    Some(60)
                } else {
                    None
                },
            );
            assert!(
                s.first().unwrap().abs() < 0.01,
                "voice {kind:?} does not start quiet"
            );
            let peak = s.iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(
                peak > 0.1 && peak <= 1.0,
                "voice {kind:?} peak {peak} out of range"
            );
            if matches!(kind, VoiceKind::Kick | VoiceKind::Snare | VoiceKind::Hat) {
                let tail = &s[s.len() - 32..];
                assert!(
                    tail.iter().all(|x| x.abs() < 0.1),
                    "voice {kind:?} does not decay to silence"
                );
            }
        }
    }

    #[test]
    fn lead_renders_different_pitches() {
        let c4 = render_all(VoiceKind::Lead, Some(60));
        let c5 = render_all(VoiceKind::Lead, Some(72));
        assert_ne!(c4, c5);
    }

    #[test]
    fn voice_is_deterministic() {
        assert_eq!(
            render_all(VoiceKind::Kick, None),
            render_all(VoiceKind::Kick, None)
        );
        assert_eq!(
            render_all(VoiceKind::Snare, None),
            render_all(VoiceKind::Snare, None)
        );
    }

    #[test]
    fn voice_exhausts_and_returns_none() {
        let mut v = Voice::new(VoiceKind::Hat, None);
        let mut n = 0;
        while v.next_sample(48000).is_some() {
            n += 1;
        }
        assert_eq!(n, voice_default_duration(VoiceKind::Hat) as usize);
        assert!(v.next_sample(48000).is_none());
    }

    #[test]
    fn filter_has_persistent_state() {
        let mut st = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let y1 = lp(1.0, 500.0, 0.7, 48000, &mut st);
        let y2 = lp(1.0, 500.0, 0.7, 48000, &mut st);
        assert_ne!(y1, y2, "lowpass state must persist between samples");
    }
}
