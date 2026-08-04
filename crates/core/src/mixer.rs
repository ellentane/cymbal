use crate::transport::Transport;

#[derive(Debug, Clone, Copy)]
pub struct VoiceOutput {
    pub sample: f32,
    pub velocity: f32,
    pub pan: f32,
    pub delay_send: f32,
    pub reverb_send: f32,
}

pub struct Master {
    bar_samples: u64,
    dry_l: f32,
    dry_r: f32,
    delay_bus: f32,
    reverb_bus: f32,
    delay: Vec<f32>,
    delay_pos: usize,
    delay_feedback: f32,
    comb: [Comb; 4],
    allpass: [Allpass; 2],
    reverb_gain: f32,
}

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    feedback: f32,
}

impl Comb {
    fn new(len: usize, feedback: f32) -> Self {
        Self {
            buf: vec![0.0; len],
            pos: 0,
            feedback,
        }
    }

    fn tick(&mut self, input: f32) -> f32 {
        let out = self.buf[self.pos];
        self.buf[self.pos] = input + out * self.feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
    gain: f32,
}

impl Allpass {
    fn new(len: usize, gain: f32) -> Self {
        Self {
            buf: vec![0.0; len],
            pos: 0,
            gain,
        }
    }

    fn tick(&mut self, input: f32) -> f32 {
        let delayed = self.buf[self.pos];
        let out = -self.gain * input + delayed;
        self.buf[self.pos] = input + self.gain * delayed;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

impl Master {
    pub fn new(sample_rate: u32, bar_samples: u64) -> Self {
        // slowest tempo 20 bpm -> bar 144000; dotted-eighth 108000 + margin
        let max_bar = Transport::new(20.0, sample_rate).bar_samples();
        let delay_len = (0.75 * max_bar as f64) as usize + 1024;
        Self {
            bar_samples,
            dry_l: 0.0,
            dry_r: 0.0,
            delay_bus: 0.0,
            reverb_bus: 0.0,
            delay: vec![0.0; delay_len],
            delay_pos: 0,
            delay_feedback: 0.35,
            comb: [
                Comb::new(1557, 0.8),
                Comb::new(1617, 0.8),
                Comb::new(1491, 0.8),
                Comb::new(1422, 0.8),
            ],
            allpass: [Allpass::new(225, 0.5), Allpass::new(556, 0.5)],
            reverb_gain: 0.06,
        }
    }

    pub fn set_bar_samples(&mut self, bar_samples: u64) {
        self.bar_samples = bar_samples;
    }

    pub fn begin_frame(&mut self) {
        self.dry_l = 0.0;
        self.dry_r = 0.0;
        self.delay_bus = 0.0;
        self.reverb_bus = 0.0;
    }

    pub fn add_voice(&mut self, v: VoiceOutput) {
        let s = v.sample * v.velocity;
        let angle = (v.pan + 1.0) * std::f32::consts::PI / 4.0;
        self.dry_l += s * angle.cos();
        self.dry_r += s * angle.sin();
        self.delay_bus += s * v.delay_send;
        self.reverb_bus += s * v.reverb_send;
    }

    pub fn end_frame(&mut self, out: &mut [f32; 2]) {
        let delay_out = self.delay_tick(self.delay_bus);
        let reverb_out = self.reverb_tick(self.reverb_bus);
        out[0] = (self.dry_l + delay_out + reverb_out).tanh();
        out[1] = (self.dry_r + delay_out + reverb_out).tanh();
    }

    fn delay_tick(&mut self, input: f32) -> f32 {
        let len = self.delay.len();
        let tap = (0.75 * self.bar_samples as f64) as usize;
        let tap_pos = (self.delay_pos + len - tap) % len;
        let out = self.delay[tap_pos];
        self.delay[self.delay_pos] = input + out * self.delay_feedback;
        self.delay_pos = (self.delay_pos + 1) % len;
        out
    }

    fn reverb_tick(&mut self, input: f32) -> f32 {
        let mut acc = 0.0;
        for c in &mut self.comb {
            acc += c.tick(input);
        }
        let a1 = self.allpass[0].tick(acc);
        let a2 = self.allpass[1].tick(a1);
        a2 * self.reverb_gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(master: &mut Master, voices: &[VoiceOutput]) -> [f32; 2] {
        master.begin_frame();
        for v in voices {
            master.add_voice(*v);
        }
        let mut out = [0.0f32; 2];
        master.end_frame(&mut out);
        out
    }

    fn mid(pan: f32) -> VoiceOutput {
        VoiceOutput {
            sample: 1.0,
            velocity: 1.0,
            pan,
            delay_send: 0.0,
            reverb_send: 0.0,
        }
    }

    #[test]
    fn pan_extremes() {
        let mut m = Master::new(48000, 24000);
        let l = frame(&mut m, &[mid(-1.0)]);
        assert!(l[1].abs() < 1e-6, "pan=-1 must be silent on right: {l:?}");
        let mut m = Master::new(48000, 24000);
        let r = frame(&mut m, &[mid(1.0)]);
        assert!(r[0].abs() < 1e-6, "pan=1 must be silent on left: {r:?}");
    }

    #[test]
    fn center_pan_has_equal_power() {
        let mut m = Master::new(48000, 24000);
        let c = frame(&mut m, &[mid(0.0)]);
        assert!((c[0] - c[1]).abs() < 1e-6);
        assert!(c[0] > 0.5, "center pan should keep level: {c:?}");
    }

    #[test]
    fn velocity_scales_amplitude() {
        let mut m = Master::new(48000, 24000);
        let quiet = frame(
            &mut m,
            &[VoiceOutput {
                sample: 1.0,
                velocity: 0.5,
                pan: 0.0,
                delay_send: 0.0,
                reverb_send: 0.0,
            }],
        );
        let mut m2 = Master::new(48000, 24000);
        let loud = frame(&mut m2, &[mid(0.0)]);
        assert!(quiet[0] < loud[0], "velocity 0.5 must be quieter");
    }

    #[test]
    fn silence_in_silence_out() {
        let mut m = Master::new(48000, 24000);
        let mut out = [1.0f32; 2];
        m.begin_frame();
        m.end_frame(&mut out);
        assert_eq!(out, [0.0, 0.0]);
    }

    #[test]
    fn delay_taps_at_dotted_eighth() {
        // 120 bpm, 48k: bar = 24000 samples, dotted-eighth = 18000
        let mut m = Master::new(48000, 24000);
        let mut taps = Vec::new();
        for i in 0..20000 {
            let v = if i == 0 {
                VoiceOutput {
                    sample: 1.0,
                    velocity: 1.0,
                    pan: 0.0,
                    delay_send: 1.0,
                    reverb_send: 0.0,
                }
            } else {
                VoiceOutput {
                    sample: 0.0,
                    velocity: 1.0,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                }
            };
            let out = frame(&mut m, &[v]);
            if i > 0 && out[0].abs() > 0.05 {
                taps.push(i);
            }
        }
        assert_eq!(
            taps.first().copied(),
            Some(18000),
            "first echo at dotted-eighth"
        );
    }

    #[test]
    fn reverb_impulse_decays_and_stays_finite() {
        let mut m = Master::new(48000, 24000);
        let mut peak = 0.0f32;
        for i in 0..48000 {
            let v = if i == 0 {
                VoiceOutput {
                    sample: 1.0,
                    velocity: 1.0,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 1.0,
                }
            } else {
                VoiceOutput {
                    sample: 0.0,
                    velocity: 1.0,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                }
            };
            let out = frame(&mut m, &[v]);
            assert!(out[0].is_finite() && out[1].is_finite());
            peak = peak.max(out[0].abs());
        }
        assert!(peak > 0.001, "impulse must produce reverb energy");
        let mut m2 = Master::new(48000, 24000);
        let tail: Vec<f32> = (48000..96000)
            .map(|_| {
                frame(
                    &mut m2,
                    &[VoiceOutput {
                        sample: 0.0,
                        velocity: 1.0,
                        pan: 0.0,
                        delay_send: 0.0,
                        reverb_send: 0.0,
                    }],
                )[0]
            })
            .collect();
        assert!(
            tail.iter().skip(40000).all(|s| s.abs() < 0.001),
            "reverb must decay below threshold"
        );
    }
}
