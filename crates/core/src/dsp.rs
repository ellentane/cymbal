use crate::ast::VoiceKind;
use crate::scheduler::SampleData;
use crate::transport::Transport;
use std::sync::Arc;

fn noise(t: u64) -> f64 {
    let x = t as f64 * 12.9898;
    (x.sin() * 43758.5453).rem_euclid(1.0) * 2.0 - 1.0
}

fn shifted_pitch(pitch: Option<u8>, semitone: i32) -> u8 {
    ((pitch.unwrap_or(60) as i32 + semitone).clamp(0, 127)) as u8
}

pub struct Kick {
    t: u64,
    phase: f64,
    dur: u64,
    semitone: i32,
    fx: VoiceFx,
}
pub struct Snare {
    t: u64,
    dur: u64,
    semitone: i32,
    fx: VoiceFx,
}
pub struct Hat {
    t: u64,
    prev_noise: f64,
    dur: u64,
    fx: VoiceFx,
}
pub struct Bass {
    t: u64,
    phase: f64,
    st: (f64, f64, f64, f64),
    freq: f64,
    dur: u64,
    fx: VoiceFx,
}
pub struct Lead {
    t: u64,
    phase: f64,
    st: (f64, f64, f64, f64),
    freq: f64,
    dur: u64,
    fx: VoiceFx,
}
pub struct Sample {
    data: Arc<SampleData>,
    pos: f64,
    start: f64,
    end: f64,
    rate: f64,
    t: u64,
    cycle: bool,
    fx: VoiceFx,
}

pub struct VoiceParams {
    pub pitch: Option<u8>,
    pub sample: Option<Arc<SampleData>>,
    pub semitone: i32,
    pub bass: f32,
    pub treble: f32,
    pub comp: f32,
    pub sample_start: f64,
    pub sample_end: f64,
    pub sample_cycle: bool,
}

impl VoiceParams {
    pub fn default_for(kind: VoiceKind, pitch: Option<u8>) -> Self {
        let _ = kind;
        Self {
            pitch,
            sample: None,
            semitone: 0,
            bass: 0.0,
            treble: 0.0,
            comp: 0.0,
            sample_start: 0.0,
            sample_end: 1.0,
            sample_cycle: false,
        }
    }

    pub fn from_event(e: &crate::scheduler::Event) -> Self {
        Self {
            pitch: e.pitch,
            sample: e.sample.clone(),
            semitone: e.semitone,
            bass: e.bass,
            treble: e.treble,
            comp: e.comp,
            sample_start: e.sample_start,
            sample_end: e.sample_end,
            sample_cycle: e.sample_loop,
        }
    }
}

pub struct VoiceFx {
    bass: f64,
    treble: f64,
    comp: f64,
    lp: f64,
    env: f64,
    lp_a: f64,
}

impl VoiceFx {
    pub fn new(bass: f32, treble: f32, comp: f32, sr: u32) -> Self {
        Self {
            bass: 10f64.powf(12.0 * bass.clamp(0.0, 1.0) as f64 / 20.0),
            treble: 10f64.powf(12.0 * treble.clamp(0.0, 1.0) as f64 / 20.0),
            comp: comp.clamp(0.0, 1.0) as f64,
            lp: 0.0,
            env: 0.0,
            lp_a: 1.0 - (-2.0 * std::f64::consts::PI * 200.0 / sr as f64).exp(),
        }
    }

    fn apply(&mut self, x: f64) -> f32 {
        self.lp += self.lp_a * (x - self.lp);
        let lp = self.lp;
        let bass_out = x + (self.bass - 1.0) * lp;
        let out = bass_out + (self.treble - 1.0) * (bass_out - lp);
        let peak = out.abs();
        self.env = if peak > self.env {
            peak
        } else {
            self.env * 0.9995
        };
        let over = (self.env - 0.5).max(0.0);
        let gain = 1.0 - self.comp * (over / self.env.max(1e-9)).min(0.5);
        (out * gain) as f32
    }
}

pub enum Voice {
    Kick(Kick),
    Snare(Snare),
    Hat(Hat),
    Bass(Bass),
    Lead(Lead),
    Sample(Sample),
}

impl Voice {
    pub fn new(kind: VoiceKind, params: VoiceParams, sr: u32) -> Self {
        use crate::scheduler::voice_default_duration;
        let dur = voice_default_duration(kind);
        let fx = VoiceFx::new(params.bass, params.treble, params.comp, sr);
        match kind {
            VoiceKind::Kick => Voice::Kick(Kick {
                t: 0,
                phase: 0.0,
                dur,
                semitone: params.semitone,
                fx,
            }),
            VoiceKind::Snare => Voice::Snare(Snare {
                t: 0,
                dur,
                semitone: params.semitone,
                fx,
            }),
            VoiceKind::Hat => Voice::Hat(Hat {
                t: 0,
                prev_noise: 0.0,
                dur,
                fx,
            }),
            VoiceKind::Bass => Voice::Bass(Bass {
                t: 0,
                phase: 0.0,
                st: (0.0, 0.0, 0.0, 0.0),
                freq: Transport::note_frequency(shifted_pitch(params.pitch, params.semitone)),
                dur,
                fx,
            }),
            VoiceKind::Lead => Voice::Lead(Lead {
                t: 0,
                phase: 0.0,
                st: (0.0, 0.0, 0.0, 0.0),
                freq: Transport::note_frequency(shifted_pitch(params.pitch, params.semitone)),
                dur,
                fx,
            }),
            VoiceKind::Sample => {
                let data = params.sample.expect("sample voice requires sample data");
                let total = data.frames.len() as f64;
                let start = params.sample_start.clamp(0.0, 1.0) * total;
                let end = (params.sample_end.clamp(0.0, 1.0) * total).max(start);
                Voice::Sample(Sample {
                    data,
                    pos: start,
                    start,
                    end,
                    rate: 2f64.powf(params.semitone as f64 / 12.0),
                    t: 0,
                    cycle: params.sample_cycle,
                    fx,
                })
            }
        }
    }

    pub fn next_sample(&mut self, sr: u32) -> Option<f32> {
        match self {
            Voice::Kick(k) => k.next_sample(sr),
            Voice::Snare(s) => s.next_sample(sr),
            Voice::Hat(h) => h.next_sample(sr),
            Voice::Bass(b) => b.next_sample(sr),
            Voice::Lead(l) => l.next_sample(sr),
            Voice::Sample(s) => s.next_sample(sr),
        }
    }
}

impl Sample {
    fn next_sample(&mut self, sr: u32) -> Option<f32> {
        if self.pos >= self.end {
            if self.cycle && self.end > self.start {
                self.pos = self.start;
            } else {
                return None;
            }
        }
        let sr_ratio = sr as f64 / self.data.sample_rate as f64;
        let step = self.rate * sr_ratio;
        let i = self.pos.floor() as usize;
        let out = if i + 1 >= self.data.frames.len() {
            // last frame: no look-ahead available, play it verbatim
            self.data.frames[i]
        } else {
            let a = self.data.frames[i] as f64;
            let b = self.data.frames[i + 1] as f64;
            let frac = self.pos - i as f64;
            (a + (b - a) * frac) as f32
        };
        self.pos += step;
        self.t += 1;
        Some(self.fx.apply(out as f64))
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
        let shift = 2f64.powf(self.semitone as f64 / 12.0);
        let f = 120.0 * shift * (45.0f64 / 120.0).powf(sweep);
        self.phase += 2.0 * std::f64::consts::PI * f / sr as f64;
        let att = (ts / 0.0005).min(1.0);
        let body = self.phase.sin() * (-ts / 0.09).exp() * 1.4 * att;
        let click = 0.2 * (2.0 * std::f64::consts::PI * 8000.0 * ts).sin() * (-ts / 0.005).exp();
        self.t += 1;
        Some(self.fx.apply((body + click).tanh()))
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
        let shift = 2f64.powf(self.semitone as f64 / 12.0);
        let body =
            (2.0 * std::f64::consts::PI * 180.0 * shift * ts).sin() * (-ts / 0.09).exp() * 0.4;
        self.t += 1;
        Some(self.fx.apply((noise_part + body).tanh()))
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
        Some(self.fx.apply(s.tanh()))
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
        Some(self.fx.apply(filtered.tanh()))
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
        Some(self.fx.apply(filtered.tanh()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::voice_default_duration;

    fn render_all(kind: VoiceKind, pitch: Option<u8>) -> Vec<f32> {
        render_all_params(kind, VoiceParams::default_for(kind, pitch))
    }

    fn render_all_params(kind: VoiceKind, params: VoiceParams) -> Vec<f32> {
        let dur = voice_default_duration(kind) as usize;
        let mut v = Voice::new(kind, params, 48000);
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
        let mut v = Voice::new(
            VoiceKind::Hat,
            VoiceParams::default_for(VoiceKind::Hat, None),
            48000,
        );
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

    fn sample_voice_at(frames: Vec<f32>, semitone: i32, sample_rate: u32) -> Voice {
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate,
        };
        Voice::new(
            VoiceKind::Sample,
            VoiceParams {
                sample: Some(Arc::new(data)),
                semitone,
                ..VoiceParams::default_for(VoiceKind::Sample, None)
            },
            48000,
        )
    }

    fn sample_voice(frames: Vec<f32>, semitone: i32) -> Voice {
        sample_voice_at(frames, semitone, 48000)
    }

    fn region_voice(frames: Vec<f32>, start: f64, end: f64, cycle: bool) -> Voice {
        let data = SampleData {
            frames: Arc::new(frames),
            sample_rate: 48000,
        };
        Voice::new(
            VoiceKind::Sample,
            VoiceParams {
                sample: Some(Arc::new(data)),
                sample_start: start,
                sample_end: end,
                sample_cycle: cycle,
                ..VoiceParams::default_for(VoiceKind::Sample, None)
            },
            48000,
        )
    }

    #[test]
    fn sample_plays_through_and_exhausts() {
        let frames: Vec<f32> = (0..10).map(|i| i as f32 / 10.0).collect();
        let mut v = sample_voice(frames, 0);
        let mut out = Vec::new();
        while let Some(s) = v.next_sample(48000) {
            out.push(s);
        }
        assert_eq!(out.len(), 10, "all 10 frames play, including the last");
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(v.next_sample(48000).is_none());
    }

    #[test]
    fn sample_pitch_shift_changes_rate() {
        let frames: Vec<f32> = vec![0.0; 48000];
        let mut up = sample_voice(frames.clone(), 12);
        let mut down = sample_voice(frames, -12);
        let mut n_up = 0;
        let mut n_down = 0;
        while up.next_sample(48000).is_some() {
            n_up += 1;
        }
        while down.next_sample(48000).is_some() {
            n_down += 1;
        }
        assert_eq!(n_up, 24000, "@12 doubles rate, half the samples");
        assert_eq!(n_down, 96000, "@-12 halves rate, ~double the samples");
    }

    #[test]
    fn sample_interpolates_linearly() {
        let frames = vec![0.0, 1.0];
        let mut v = sample_voice(frames, -12);
        let s0 = v.next_sample(48000).unwrap();
        let s1 = v.next_sample(48000).unwrap();
        let s2 = v.next_sample(48000).unwrap();
        assert_eq!(s0, 0.0);
        assert_eq!(s1, 0.5);
        assert_eq!(s2, 1.0);
    }

    #[test]
    fn sample_plays_at_file_rate() {
        let frames = vec![0.0; 48000];
        let mut v = sample_voice_at(frames, 0, 24000);
        let mut n = 0;
        while v.next_sample(48000).is_some() {
            n += 1;
        }
        assert_eq!(n, 24000, "sr_ratio 2 plays half the frames");
    }

    #[test]
    fn sample_empty_frames_is_silent() {
        let mut v = sample_voice(vec![], 0);
        assert!(v.next_sample(48000).is_none());
    }

    #[test]
    fn sample_single_frame_plays_once() {
        let mut v = sample_voice(vec![0.25], 0);
        assert_eq!(v.next_sample(48000).unwrap(), 0.25);
        assert!(v.next_sample(48000).is_none());
    }

    #[test]
    fn sample_region_starts_at_offset_and_ends() {
        let frames: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let mut v = region_voice(frames, 0.25, 0.75, false);
        let first = v.next_sample(48000).unwrap();
        assert_eq!(first, 2.0, "starts at frame 2 of 8");
        let mut n = 0;
        while v.next_sample(48000).is_some() {
            n += 1;
        }
        assert_eq!(
            n, 3,
            "frames 3, 4, 5 play (pos 3,4,5 < end 6), 2 played above"
        );
    }

    #[test]
    fn sample_cycle_wraps_region() {
        let frames: Vec<f32> = (0..4).map(|i| i as f32).collect();
        let mut v = region_voice(frames, 0.25, 0.75, true);
        let s0 = v.next_sample(48000).unwrap();
        let s1 = v.next_sample(48000).unwrap();
        let s2 = v.next_sample(48000).unwrap();
        assert_eq!((s0, s1, s2), (1.0, 2.0, 1.0), "wraps back to start");
    }

    #[test]
    fn voice_fx_bypass_is_bit_exact() {
        for kind in [
            VoiceKind::Kick,
            VoiceKind::Snare,
            VoiceKind::Hat,
            VoiceKind::Bass,
            VoiceKind::Lead,
        ] {
            let pitch = if matches!(kind, VoiceKind::Bass | VoiceKind::Lead) {
                Some(60)
            } else {
                None
            };
            let mut plain = Voice::new(kind, VoiceParams::default_for(kind, pitch), 48000);
            let mut fx = Voice::new(
                kind,
                VoiceParams {
                    bass: 0.0,
                    treble: 0.0,
                    comp: 0.0,
                    ..VoiceParams::default_for(kind, pitch)
                },
                48000,
            );
            loop {
                let a = plain.next_sample(48000);
                let b = fx.next_sample(48000);
                assert_eq!(a, b, "zeroed fx must not change {kind:?}");
                if a.is_none() {
                    break;
                }
            }
        }
    }

    #[test]
    fn bass_shelf_boosts_dc() {
        let mut fx = VoiceFx::new(1.0, 0.0, 0.0, 48000);
        let out = fx.apply(0.1);
        let mut steady = fx.apply(0.1);
        for _ in 0..2000 {
            steady = fx.apply(0.1);
        }
        // bass=1.0 is a 12 dB shelf -> DC gain 10^(12/20) = 3.98x -> steady ~0.398
        assert!(steady > 0.3, "low shelf must boost DC content");
        assert!(steady < 0.45, "and settle at the shelf gain, not blow up");
        assert!(out > 0.1);
    }

    #[test]
    fn treble_shelf_boosts_fast_content_only() {
        let mut fx = VoiceFx::new(0.0, 1.0, 0.0, 48000);
        let mut steady = 0.0f32;
        for _ in 0..2000 {
            steady = fx.apply(0.1);
        }
        assert!(
            (steady - 0.1).abs() < 1e-6,
            "DC content unchanged by treble shelf"
        );
    }

    #[test]
    fn compressor_reduces_loud_signal() {
        let mut fx = VoiceFx::new(0.0, 0.0, 1.0, 48000);
        let mut peak = 0.0f32;
        for _ in 0..48000 {
            peak = peak.max(fx.apply(1.0));
        }
        // constant 1.0 -> env 1.0, over 0.5, gain exactly 0.5
        assert!(peak < 1.0, "compression must pull 1.0 down: {peak}");
        assert!(peak > 0.45, "and not squash it to silence: {peak}");
    }

    #[test]
    fn synth_semitone_equals_pitch_shift() {
        // kind @12 must be bit-identical to kind at +1 octave (pitch 72, semitone 0)
        for kind in [VoiceKind::Bass, VoiceKind::Lead] {
            let shifted = render_all_params(
                kind,
                VoiceParams {
                    semitone: 12,
                    ..VoiceParams::default_for(kind, Some(60))
                },
            );
            let plain = render_all(kind, Some(72));
            assert_eq!(shifted, plain, "{kind:?}@12 == {kind:?} c5");
        }
    }

    #[test]
    fn percussion_semitone_changes_body() {
        let k0 = render_all(VoiceKind::Kick, None);
        let k12 = render_all_params(
            VoiceKind::Kick,
            VoiceParams {
                semitone: 12,
                ..VoiceParams::default_for(VoiceKind::Kick, None)
            },
        );
        assert_ne!(k0, k12);
        assert!(k12.iter().all(|s| s.is_finite()));
    }
}
