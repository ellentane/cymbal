use std::collections::HashMap;
use std::sync::Arc;

use cymbal_core::dsp::Voice;
use cymbal_core::mixer::{Master, VoiceOutput};
use cymbal_core::scheduler::{Event, Timeline};
use cymbal_core::transport::Transport;

use crate::recorder::Recorder;

struct Scheduled {
    at: u64,
    event: Event,
}

struct RecState {
    rec: Arc<Recorder>,
    current: Box<[f32]>,
    pos: usize,
}

struct Active {
    until: u64,
    loop_name: String,
    generation: u64,
    voice: Voice,
    velocity: f32,
    pan: f32,
    delay_send: f32,
    reverb_send: f32,
}

pub struct Engine {
    sample_rate: u32,
    bar_samples: u64,
    position: u64,
    next_bar_abs: u64,
    timeline_origin: u64,
    last_swap_seq: u64,
    pending: Option<(Arc<Timeline>, u64)>,
    future: Vec<Scheduled>,
    active: Vec<Active>,
    master: Master,
    rec_state: Option<RecState>,
}

impl Engine {
    pub fn new(tempo: f64, sample_rate: u32) -> Self {
        let transport = Transport::new(tempo, sample_rate);
        let bar_samples = transport.bar_samples();
        Self {
            sample_rate,
            bar_samples,
            position: 0,
            next_bar_abs: 0,
            timeline_origin: 0,
            last_swap_seq: 0,
            pending: None,
            future: Vec::with_capacity(256),
            active: Vec::with_capacity(32),
            master: Master::new(sample_rate, bar_samples),
            rec_state: None,
        }
    }

    pub fn submit_swap(&mut self, timeline: Arc<Timeline>, seq: u64) {
        self.pending = Some((timeline, seq));
    }

    pub fn process(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        for frame in 0..frames {
            let now = self.position + frame as u64;
            // while, not if: keeps the boundary invariant for any buffer/frame
            // alignment; a pending swap applies at the first boundary reached.
            while now >= self.next_bar_abs {
                self.apply_swap_at_boundary(now);
                self.next_bar_abs += self.bar_samples;
            }
            while let Some(at) = self.future.first().map(|s| s.at) {
                if at <= now {
                    let s = self.future.remove(0);
                    let e = &s.event;
                    self.active.push(Active {
                        until: s.at + e.duration,
                        loop_name: e.loop_name.clone(),
                        generation: e.generation,
                        voice: Voice::new(e.voice, e.pitch, e.sample.clone(), e.semitone),
                        velocity: e.velocity,
                        pan: e.pan,
                        delay_send: e.delay_send,
                        reverb_send: e.reverb_send,
                    });
                } else {
                    break;
                }
            }
            self.active.retain(|a| a.until > now);
            self.master.begin_frame();
            for a in &mut self.active {
                if let Some(s) = a.voice.next_sample(self.sample_rate) {
                    self.master.add_voice(VoiceOutput {
                        sample: s,
                        velocity: a.velocity,
                        pan: a.pan,
                        delay_send: a.delay_send,
                        reverb_send: a.reverb_send,
                    });
                }
            }
            let mut frame_out = [0.0f32; 2];
            self.master.end_frame(&mut frame_out);
            out[frame * 2] = frame_out[0];
            out[frame * 2 + 1] = frame_out[1];
            self.push_rec_frame(frame_out[0], frame_out[1]);
        }
        self.position += frames as u64;
    }

    pub fn start_recording(&mut self, rec: Arc<Recorder>) {
        if self.rec_state.is_some() {
            self.stop_recording();
        }
        let block_frames = rec.block_frames();
        self.rec_state = Some(RecState {
            rec,
            current: vec![0.0f32; block_frames * 2].into_boxed_slice(),
            pos: 0,
        });
    }

    pub fn stop_recording(&mut self) {
        if let Some(state) = self.rec_state.take() {
            let pos = state.pos;
            if pos > 0 {
                let mut cur = state.current;
                for s in &mut cur[pos * 2..] {
                    *s = 0.0;
                }
                state.rec.push_filled(cur);
            }
            state.rec.stop();
        }
    }

    fn push_rec_frame(&mut self, l: f32, r: f32) {
        let Some(state) = &mut self.rec_state else {
            return;
        };
        state.current[state.pos * 2] = l;
        state.current[state.pos * 2 + 1] = r;
        state.pos += 1;
        if state.pos == state.rec.block_frames() {
            let full = std::mem::replace(&mut state.current, state.rec.take_pool_block());
            state.rec.push_filled(full);
            state.pos = 0;
        }
    }

    fn apply_swap_at_boundary(&mut self, now: u64) {
        if let Some((tl, seq)) = self.pending.take() {
            if seq <= self.last_swap_seq {
                return;
            }
            self.last_swap_seq = seq;
            self.timeline_origin = now;
            self.bar_samples = tl.bar_samples;
            self.master.set_bar_samples(tl.bar_samples);
            let kept: HashMap<&str, u64> = tl
                .loop_generations
                .iter()
                .map(|(n, g)| (n.as_str(), *g))
                .collect();
            self.active
                .retain(|a| kept.get(a.loop_name.as_str()) == Some(&a.generation));
            self.future.clear();
            for ev in &tl.events {
                self.future.push(Scheduled {
                    at: self.timeline_origin + ev.sample_offset,
                    event: ev.clone(),
                });
            }
            self.future.sort_by_key(|s| s.at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cymbal_core::ast::VoiceKind;
    use cymbal_core::scheduler::{Event, Timeline};

    fn tl(
        events: Vec<Event>,
        generation: u64,
        loop_generations: Vec<(String, u64)>,
        bar_samples: u64,
    ) -> Arc<Timeline> {
        Arc::new(Timeline {
            events,
            generation,
            tempo: 120.0,
            bar_samples,
            sample_rate: 48000,
            loops: loop_generations.iter().map(|(n, _)| n.clone()).collect(),
            loop_generations,
        })
    }

    fn ev(offset: u64, voice: VoiceKind, generation: u64, duration: u64) -> Event {
        Event {
            sample_offset: offset,
            loop_name: "b".into(),
            voice,
            pitch: None,
            semitone: 0,
            velocity: 1.0,
            duration,
            generation,
            pan: 0.0,
            delay_send: 0.0,
            reverb_send: 0.0,
            sample: None,
        }
    }

    fn engine_step(engine: &mut Engine, n: u64) -> Vec<f32> {
        let mut out = vec![0.0f32; n as usize * 2];
        engine.process(&mut out);
        out
    }

    fn lr(out: &[f32], frame: usize) -> (f32, f32) {
        (out[frame * 2], out[frame * 2 + 1])
    }

    #[test]
    fn renders_events_into_stereo_buffer() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![
                    ev(0, VoiceKind::Hat, 0, 2400),
                    ev(48000, VoiceKind::Hat, 0, 2400),
                ],
                0,
                vec![("b".into(), 0)],
                96000,
            ),
            1,
        );
        let out = engine_step(&mut engine, 96000);
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "first frames should contain the hat hit"
        );
        let (l1, r1) = lr(&out, 1);
        assert_eq!(l1, r1, "center-panned voice is equal-power");
        assert_eq!(lr(&out, 47999), (0.0, 0.0), "gap before second hit");
        assert!(
            out[48000 * 2..48000 * 2 + 16].iter().any(|s| *s != 0.0),
            "second hit at 48000"
        );
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn swap_applies_at_bar_boundary() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![
                    ev(0, VoiceKind::Kick, 0, 14400),
                    ev(60000, VoiceKind::Kick, 0, 14400),
                ],
                0,
                vec![("b".into(), 0)],
                96000,
            ),
            1,
        );
        engine_step(&mut engine, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Snare, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                96000,
            ),
            2,
        );
        let out = engine_step(&mut engine, 96000);
        assert!(
            out[12000 * 2..12000 * 2 + 16].iter().any(|s| *s != 0.0),
            "gen0 mid-bar kick must still play"
        );
        assert_eq!(lr(&out, 26400), (0.0, 0.0), "gen0 kick must have decayed");
        assert_eq!(lr(&out, 47999), (0.0, 0.0), "silence before the boundary");
        assert!(
            out[48000 * 2..48000 * 2 + 16].iter().any(|s| *s != 0.0),
            "gen1 snare starts at the boundary"
        );
    }

    #[test]
    fn unchanged_loop_notes_survive_swap() {
        let mut engine = Engine::new(120.0, 48000);
        // bass loop "b" gen 0, long note from frame 0
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Bass, 0, 50000)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        engine_step(&mut engine, 24000);
        // swap at bar 24000: "b" unchanged (gen 0), new "h" gen 1
        engine.submit_swap(
            tl(
                vec![
                    ev(0, VoiceKind::Bass, 0, 50000),
                    Event {
                        sample_offset: 0,
                        loop_name: "h".into(),
                        voice: VoiceKind::Hat,
                        pitch: None,
                        semitone: 0,
                        velocity: 1.0,
                        duration: 2400,
                        generation: 1,
                        pan: 0.0,
                        delay_send: 0.0,
                        reverb_send: 0.0,
                        sample: None,
                    },
                ],
                1,
                vec![("b".into(), 0), ("h".into(), 1)],
                24000,
            ),
            2,
        );
        let out = engine_step(&mut engine, 24000);
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "bass still ringing after swap (frames 0-8)"
        );
        assert!(
            out[4800 * 2..4800 * 2 + 8].iter().any(|s| *s != 0.0),
            "bass still ringing at frame 4800 (t=28800)"
        );
    }

    #[test]
    fn changed_loop_notes_are_cut_at_swap() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Bass, 0, 50000)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![Event {
                    sample_offset: 0,
                    loop_name: "b".into(),
                    voice: VoiceKind::Bass,
                    pitch: None,
                    semitone: 0,
                    velocity: 1.0,
                    duration: 50000,
                    generation: 1,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                    sample: None,
                }],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            2,
        );
        let out = engine_step(&mut engine, 4800);
        let mut reference = Engine::new(120.0, 48000);
        reference.submit_swap(
            tl(
                vec![Event {
                    sample_offset: 0,
                    loop_name: "b".into(),
                    voice: VoiceKind::Bass,
                    pitch: None,
                    semitone: 0,
                    velocity: 1.0,
                    duration: 50000,
                    generation: 1,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                    sample: None,
                }],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            3,
        );
        let expect = engine_step(&mut reference, 4800);
        assert_eq!(
            out, expect,
            "changed loop's note must be cut at the boundary"
        );
    }

    #[test]
    fn removed_loop_notes_are_cut_at_swap() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Bass, 0, 50000)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        engine_step(&mut engine, 24000);
        engine.submit_swap(tl(vec![], 1, vec![], 24000), 2);
        let out = engine_step(&mut engine, 4800);
        assert!(out[0..16].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn generation_cutover_drops_stale_events() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Kick, 0, 14400)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            2,
        );
        let out = engine_step(&mut engine, 24000);
        let mut reference = Engine::new(120.0, 48000);
        reference.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            3,
        );
        let expect = engine_step(&mut reference, 24000);
        assert_eq!(out, expect);
    }

    #[test]
    fn empty_timeline_is_silent() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![], 96000), 1);
        let mut out = vec![0.5f32; 96000 * 2];
        engine.process(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn swap_rebases_grid_on_tempo_change() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![], 24000), 1);
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![
                    ev(0, VoiceKind::Kick, 1, 2400),
                    ev(12000, VoiceKind::Kick, 1, 2400),
                ],
                1,
                vec![("b".into(), 1)],
                12000,
            ),
            2,
        );
        let out = engine_step(&mut engine, 24000);
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "kick at origin + 0 on the new grid"
        );
        assert!(
            out[4800 * 2..4800 * 2 + 8].iter().all(|s| *s == 0.0),
            "silence between the two kicks"
        );
        assert!(
            out[12000 * 2..12000 * 2 + 16].iter().any(|s| *s != 0.0),
            "kick at origin + 12000 (new bar_samples)"
        );
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn recording_taps_post_master_stereo() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 0, 2400)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        let rec = Recorder::new(4, 4);
        engine.start_recording(rec.clone());
        engine_step(&mut engine, 4);
        engine.stop_recording();
        let blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec.take_filled()).collect();
        assert_eq!(blocks.len(), 1, "4 frames = one full block");
        assert!(
            blocks[0][..8].iter().any(|s| *s != 0.0),
            "captured post-master signal"
        );
        assert!(rec.is_stopped());
    }

    #[test]
    fn record_stop_flushes_partial_block() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 0, 2400)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        let rec = Recorder::new(4, 4);
        engine.start_recording(rec.clone());
        engine_step(&mut engine, 3);
        engine.stop_recording();
        let b = rec.take_filled().unwrap();
        assert!(
            b[..6].iter().any(|s| *s != 0.0),
            "partial block contains audio"
        );
        assert_eq!(&b[6..], &[0.0, 0.0], "unfilled tail is zeroed");
        assert!(rec.take_filled().is_none());
    }

    #[test]
    fn recording_restarts_after_stop() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 0, 2400)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        let rec1 = Recorder::new(4, 4);
        engine.start_recording(rec1.clone());
        engine_step(&mut engine, 3);
        engine.stop_recording();
        let b1 = rec1.take_filled().unwrap();
        assert!(
            b1[..6].iter().any(|s| *s != 0.0),
            "first recording's partial block contains audio"
        );
        assert_eq!(&b1[6..], &[0.0, 0.0], "unfilled tail is zeroed");
        let rec2 = Recorder::new(4, 4);
        engine.start_recording(rec2.clone());
        engine_step(&mut engine, 4);
        engine.stop_recording();
        let blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec2.take_filled()).collect();
        assert_eq!(blocks.len(), 1, "second recording = one full block");
        assert!(
            blocks[0][..8].iter().any(|s| *s != 0.0),
            "second recording captures post-master signal"
        );
        assert!(rec1.is_stopped());
        assert!(rec2.is_stopped());
    }

    #[test]
    fn recording_continues_across_swap() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 0, 2400)],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            1,
        );
        let rec = Recorder::new(64, 4800);
        engine.start_recording(rec.clone());
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![
                    ev(0, VoiceKind::Hat, 0, 2400),
                    ev(19200, VoiceKind::Hat, 0, 2400),
                ],
                0,
                vec![("b".into(), 0)],
                24000,
            ),
            2,
        );
        engine_step(&mut engine, 24000);
        engine.stop_recording();
        let blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec.take_filled()).collect();
        assert_eq!(
            blocks.len(),
            10,
            "two 24000-frame steps at 4800-frame blocks"
        );
        let total_frames: usize = blocks.iter().map(|b| b.len() / 2).sum();
        assert_eq!(
            total_frames, 48000,
            "recording is continuous across the swap"
        );
        assert!(
            blocks[0][..16].iter().any(|s| *s != 0.0),
            "audio present before the swap"
        );
        assert!(
            blocks[9][..16].iter().any(|s| *s != 0.0),
            "audio continues after the swap (late event at frame 43200)"
        );
        assert!(rec.is_stopped());
    }

    #[test]
    fn large_buffer_spans_multiple_boundaries() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![], 24000), 1);
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            2,
        );
        let out = engine_step(&mut engine, 72000);
        assert_eq!(out.len(), 72000 * 2, "three bars of stereo");
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "swap applied at the first boundary (buffer frame 0)"
        );
        assert!(
            out[24000 * 2..24000 * 2 + 16].iter().all(|s| *s == 0.0),
            "second boundary is a no-op: nothing scheduled at origin + 24000"
        );
        assert!(
            out[48000 * 2..48000 * 2 + 16].iter().all(|s| *s == 0.0),
            "third boundary is a no-op"
        );
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn stale_swap_is_ignored() {
        let mut engine = Engine::new(120.0, 48000);
        let kick_tl = tl(
            vec![
                ev(0, VoiceKind::Kick, 0, 14400),
                ev(24000, VoiceKind::Kick, 0, 14400),
            ],
            0,
            vec![("b".into(), 0)],
            24000,
        );
        engine.submit_swap(kick_tl.clone(), 1);
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Snare, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            1,
        );
        let out = engine_step(&mut engine, 48000);
        let mut reference = Engine::new(120.0, 48000);
        reference.submit_swap(kick_tl, 1);
        engine_step(&mut reference, 24000);
        let expect = engine_step(&mut reference, 48000);
        assert_eq!(
            out, expect,
            "stale seq-1 swap must not replace the active timeline"
        );
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "kick must keep playing across the boundary the stale swap would have taken"
        );
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 2, 2400)],
                2,
                vec![("b".into(), 2)],
                24000,
            ),
            2,
        );
        let out = engine_step(&mut engine, 48000);
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "newer seq-2 swap applies at the next boundary"
        );
    }
}
