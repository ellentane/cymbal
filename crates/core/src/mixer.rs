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
    delay_bus_l: f32,
    delay_bus_r: f32,
    reverb_bus_l: f32,
    reverb_bus_r: f32,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    delay_pos_l: usize,
    delay_pos_r: usize,
    delay_feedback: f32,
    comb_l: [Comb; 4],
    comb_r: [Comb; 4],
    allpass_l: [Allpass; 2],
    allpass_r: [Allpass; 2],
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
        // slowest tempo 20 bpm -> bar 576000; delay tap 0.75 bar = 432000 + margin
        let max_bar = Transport::new(20.0, sample_rate).bar_samples();
        let delay_len = (0.75 * max_bar as f64) as usize + 1024;
        Self {
            bar_samples,
            dry_l: 0.0,
            dry_r: 0.0,
            delay_bus_l: 0.0,
            delay_bus_r: 0.0,
            reverb_bus_l: 0.0,
            reverb_bus_r: 0.0,
            delay_l: vec![0.0; delay_len],
            delay_r: vec![0.0; delay_len],
            delay_pos_l: 0,
            delay_pos_r: 0,
            delay_feedback: 0.35,
            comb_l: [
                Comb::new(1557, 0.8),
                Comb::new(1617, 0.8),
                Comb::new(1491, 0.8),
                Comb::new(1422, 0.8),
            ],
            comb_r: [
                Comb::new(1557, 0.8),
                Comb::new(1617, 0.8),
                Comb::new(1491, 0.8),
                Comb::new(1422, 0.8),
            ],
            allpass_l: [Allpass::new(225, 0.5), Allpass::new(556, 0.5)],
            allpass_r: [Allpass::new(225, 0.5), Allpass::new(556, 0.5)],
            reverb_gain: 0.06,
        }
    }

    pub fn set_bar_samples(&mut self, bar_samples: u64) {
        self.bar_samples = bar_samples;
    }

    pub fn begin_frame(&mut self) {
        self.dry_l = 0.0;
        self.dry_r = 0.0;
        self.delay_bus_l = 0.0;
        self.delay_bus_r = 0.0;
        self.reverb_bus_l = 0.0;
        self.reverb_bus_r = 0.0;
    }

    pub fn add_voice(&mut self, v: VoiceOutput) {
        let s = v.sample * v.velocity;
        let angle = (v.pan + 1.0) * std::f32::consts::PI / 4.0;
        let dl = angle.cos();
        let dr = angle.sin();
        self.dry_l += s * dl;
        self.dry_r += s * dr;
        self.delay_bus_l += s * v.delay_send * dl;
        self.delay_bus_r += s * v.delay_send * dr;
        self.reverb_bus_l += s * v.reverb_send * dl;
        self.reverb_bus_r += s * v.reverb_send * dr;
    }

    pub fn end_frame(&mut self, out: &mut [f32; 2]) {
        let delay_out_l = self.delay_tick(self.delay_bus_l, true);
        let delay_out_r = self.delay_tick(self.delay_bus_r, false);
        let reverb_out_l = self.reverb_tick(self.reverb_bus_l, true);
        let reverb_out_r = self.reverb_tick(self.reverb_bus_r, false);
        out[0] = (self.dry_l + delay_out_l + reverb_out_l).tanh();
        out[1] = (self.dry_r + delay_out_r + reverb_out_r).tanh();
    }

    fn delay_tick(&mut self, input: f32, is_l: bool) -> f32 {
        let (buf, pos) = if is_l {
            (&mut self.delay_l, &mut self.delay_pos_l)
        } else {
            (&mut self.delay_r, &mut self.delay_pos_r)
        };
        let len = buf.len();
        let tap = ((0.75 * self.bar_samples as f64) as usize).min(len);
        let tap_pos = (*pos + len - tap) % len;
        let out = buf[tap_pos];
        buf[*pos] = input + out * self.delay_feedback;
        *pos = (*pos + 1) % len;
        out
    }

    fn reverb_tick(&mut self, input: f32, is_l: bool) -> f32 {
        let (combs, allpasses) = if is_l {
            (&mut self.comb_l, &mut self.allpass_l)
        } else {
            (&mut self.comb_r, &mut self.allpass_r)
        };
        let mut acc = 0.0;
        for c in combs {
            acc += c.tick(input);
        }
        let a1 = allpasses[0].tick(acc);
        let a2 = allpasses[1].tick(a1);
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
    fn set_bar_samples_changes_delay_timing() {
        let mut m = Master::new(48000, 24000);
        m.set_bar_samples(12000);
        let mut taps = Vec::new();
        for i in 0..12000 {
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
            Some(9000),
            "first echo at 0.75 * 12000"
        );
    }

    #[test]
    fn delay_tick_survives_pathological_bar_samples() {
        for bar_samples in [2_304_000, u64::MAX] {
            let mut m = Master::new(48000, 24000);
            m.set_bar_samples(bar_samples);
            let v = VoiceOutput {
                sample: 1.0,
                velocity: 1.0,
                pan: 0.0,
                delay_send: 1.0,
                reverb_send: 0.0,
            };
            for _ in 0..200 {
                let out = frame(&mut m, &[v]);
                assert!(out[0].is_finite() && out[1].is_finite());
            }
        }
    }

    #[test]
    fn delay_taps_at_dotted_half() {
        // 120 bpm, 48k: bar = 24000 samples, dotted-half (3-beat) tap = 18000
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
            "first echo at the dotted-half (3-beat) tap"
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

    #[test]
    fn delay_echo_follows_source_pan() {
        let mut m = Master::new(48000, 12000); // bar 12000 -> tap 9000
        let mut taps_r = Vec::new();
        let mut taps_l = Vec::new();
        for i in 0..12000 {
            let v = if i == 0 {
                VoiceOutput {
                    sample: 1.0,
                    velocity: 1.0,
                    pan: -1.0,
                    delay_send: 1.0,
                    reverb_send: 0.0,
                }
            } else {
                VoiceOutput {
                    sample: 0.0,
                    velocity: 1.0,
                    pan: -1.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                }
            };
            let out = frame(&mut m, &[v]);
            if i > 0 && out[0].abs() > 0.05 {
                taps_l.push(i);
            }
            if i > 0 && out[1].abs() > 0.05 {
                taps_r.push(i);
            }
        }
        assert_eq!(
            taps_l.first().copied(),
            Some(9000),
            "echo on the left at 0.75 bar"
        );
        assert!(taps_r.is_empty(), "hard-left echo must not reach the right");
    }

    #[test]
    fn reverb_tail_follows_source_pan() {
        let mut m = Master::new(48000, 24000);
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for i in 0..48000 {
            let v = if i == 0 {
                VoiceOutput {
                    sample: 1.0,
                    velocity: 1.0,
                    pan: 1.0,
                    delay_send: 0.0,
                    reverb_send: 1.0,
                }
            } else {
                VoiceOutput {
                    sample: 0.0,
                    velocity: 1.0,
                    pan: 1.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                }
            };
            let out = frame(&mut m, &[v]);
            peak_l = peak_l.max(out[0].abs());
            peak_r = peak_r.max(out[1].abs());
        }
        assert!(peak_r > 0.001, "tail must be audible on the right");
        assert!(peak_l < 0.001, "hard-right reverb must not leak left");
    }

    #[test]
    fn center_pan_sends_split_equally() {
        let mut m = Master::new(48000, 24000);
        let v = VoiceOutput {
            sample: 1.0,
            velocity: 1.0,
            pan: 0.0,
            delay_send: 1.0,
            reverb_send: 0.0,
        };
        let mut first = 0usize;
        let mut at_echo = [0.0f32; 2];
        for i in 0..20000 {
            let out = frame(
                &mut m,
                &[if i == 0 {
                    v
                } else {
                    VoiceOutput {
                        sample: 0.0,
                        velocity: 1.0,
                        pan: 0.0,
                        delay_send: 0.0,
                        reverb_send: 0.0,
                    }
                }],
            );
            if i > 0 && out[0].abs() > 0.05 && first == 0 {
                first = i;
            }
            if i == 18000 {
                at_echo = out;
            }
        }
        assert_eq!(first, 18000, "center echo at dotted-half tap");
        assert!(at_echo[0].abs() > 0.05);
        assert!(
            (at_echo[0] - at_echo[1]).abs() < 1e-6,
            "center stays center in FX"
        );
    }
}
