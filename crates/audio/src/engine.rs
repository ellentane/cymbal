use std::sync::Arc;

use cymbal_core::dsp::Voice;
use cymbal_core::scheduler::{Event, Timeline};
use cymbal_core::transport::Transport;

struct Scheduled {
    at: u64,
    event: Event,
}

struct Active {
    until: u64,
    voice: Voice,
}

pub struct Engine {
    sample_rate: u32,
    bar_samples: u64,
    position: u64,
    next_bar_abs: u64,
    timeline_origin: u64,
    generation: u64,
    timeline: Option<Arc<Timeline>>,
    pending: Option<Arc<Timeline>>,
    future: Vec<Scheduled>,
    active: Vec<Active>,
}

impl Engine {
    pub fn new(tempo: f64, sample_rate: u32) -> Self {
        let transport = Transport::new(tempo, sample_rate);
        Self {
            sample_rate,
            bar_samples: transport.bar_samples(),
            position: 0,
            next_bar_abs: 0,
            timeline_origin: 0,
            generation: 0,
            timeline: None,
            pending: None,
            future: Vec::with_capacity(256),
            active: Vec::with_capacity(32),
        }
    }

    pub fn submit_swap(&mut self, timeline: Arc<Timeline>) {
        self.pending = Some(timeline);
    }

    pub fn process(&mut self, out: &mut [f32]) {
        let n = out.len() as u64;
        for frame in 0..n {
            let now = self.position + frame;
            if now >= self.next_bar_abs {
                self.apply_swap_at_boundary(now);
                self.next_bar_abs += self.bar_samples;
            }
            while let Some(at) = self.future.first().map(|s| s.at) {
                if at <= now {
                    let s = self.future.remove(0);
                    self.active.push(Active {
                        until: s.at + s.event.duration,
                        voice: Voice::new(s.event.voice, s.event.pitch),
                    });
                } else {
                    break;
                }
            }
            let mut sample = 0.0f32;
            self.active.retain(|a| a.until > now);
            for a in &mut self.active {
                if let Some(s) = a.voice.next_sample(self.sample_rate) {
                    sample += s;
                }
            }
            out[frame as usize] = sample.tanh();
        }
        self.position += n;
    }

    fn apply_swap_at_boundary(&mut self, now: u64) {
        if let Some(tl) = self.pending.take() {
            self.generation = tl.generation;
            self.timeline_origin = now;
            self.timeline = Some(tl.clone());
            self.bar_samples = tl.bar_samples;
            self.future.clear();
            self.active.clear();
            for ev in &tl.events {
                if ev.generation == self.generation {
                    self.future.push(Scheduled {
                        at: self.timeline_origin + ev.sample_offset,
                        event: ev.clone(),
                    });
                }
            }
            self.future.sort_by_key(|s| s.at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cymbal_core::ast::VoiceKind;

    fn tl(events: Vec<Event>, generation: u64) -> Arc<Timeline> {
        Arc::new(Timeline {
            events,
            generation,
            tempo: 120.0,
            bar_samples: 96000,
            sample_rate: 48000,
            loops: vec![],
            loop_generations: vec![("b".to_string(), generation)],
        })
    }

    fn ev(offset: u64, voice: VoiceKind, generation: u64) -> Event {
        Event {
            sample_offset: offset,
            loop_name: "b".to_string(),
            voice,
            pitch: None,
            semitone: 0,
            velocity: 1.0,
            duration: 2400,
            generation,
            pan: 0.0,
            delay_send: 0.0,
            reverb_send: 0.0,
            sample: None,
        }
    }

    #[test]
    fn renders_events_into_buffer() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(
            vec![ev(0, VoiceKind::Hat, 0), ev(48000, VoiceKind::Hat, 0)],
            0,
        ));
        let mut out = vec![0.0f32; 96000];
        engine.process(&mut out);
        assert!(
            out[0..8].iter().any(|s| *s != 0.0),
            "first frames should contain the hat hit"
        );
        assert!(out[47999] == 0.0, "gap before second hit should be silent");
        assert!(
            out[48000..48008].iter().any(|s| *s != 0.0),
            "second hit should sound at offset 48000"
        );
        assert!(
            out[50400..].iter().all(|s| *s == 0.0),
            "hat decays in 2400 samples; tail must be silent"
        );
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn swap_applies_at_bar_boundary() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(
            vec![ev(0, VoiceKind::Kick, 0), ev(60000, VoiceKind::Kick, 0)],
            0,
        ));
        engine.process(&mut vec![0.0f32; 48000]);
        engine.submit_swap(tl(vec![ev(0, VoiceKind::Snare, 1)], 1));
        let mut out = vec![0.0f32; 96000];
        engine.process(&mut out);
        assert!(
            out[12000..12008].iter().any(|s| *s != 0.0),
            "gen0 mid-bar kick must still play"
        );
        assert!(
            out[26400] == 0.0,
            "gen0 kick (14400 samples) must have decayed"
        );
        assert!(out[47999] == 0.0, "silence right before the boundary");
        assert!(
            out[48000..48008].iter().any(|s| *s != 0.0),
            "gen1 snare must start at the bar boundary"
        );
        assert!(out[50400] == 0.0, "snare (2400 samples) must have decayed");
    }

    #[test]
    fn generation_cutover_drops_stale_events() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![ev(0, VoiceKind::Kick, 0)], 0));
        engine.submit_swap(tl(vec![ev(0, VoiceKind::Hat, 1)], 1));
        let mut out = vec![0.0f32; 48000];
        engine.process(&mut out);
        let mut reference = Engine::new(120.0, 48000);
        reference.submit_swap(tl(vec![ev(0, VoiceKind::Hat, 1)], 1));
        let mut expect = vec![0.0f32; 48000];
        reference.process(&mut expect);
        assert_eq!(out, expect);
    }

    #[test]
    fn empty_timeline_is_silent() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0));
        let mut out = vec![0.5f32; 96000];
        engine.process(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }
}
