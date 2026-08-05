use std::collections::HashMap;
use std::sync::Arc;

use cymbal_core::dsp::Voice;
use cymbal_core::mixer::{Master, VoiceOutput};
use cymbal_core::scheduler::Timeline;
use cymbal_core::transport::Transport;

use crate::midi_out::{MidiItem, MidiOut};
use crate::recorder::Recorder;

struct RecState {
    rec: Arc<Recorder>,
    current: Box<[f32]>,
    pos: usize,
}

struct TrackState {
    rec: Arc<Recorder>,
    current: Box<[f32]>,
    pos: usize,
}

struct Active {
    until: u64,
    loop_index: u32,
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
    timeline: Option<Arc<Timeline>>,
    active: Vec<Active>,
    master: Master,
    rec_state: Option<RecState>,
    tracks: Vec<(String, TrackState)>,
    track_acc: Vec<(String, f32, f32)>,
    midi: Option<Arc<MidiOut>>,
    event_cursor: usize,
    midi_cursor: usize,
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
            timeline: None,
            active: Vec::with_capacity(512),
            master: Master::new(sample_rate, bar_samples),
            rec_state: None,
            tracks: Vec::new(),
            track_acc: Vec::new(),
            midi: None,
            event_cursor: 0,
            midi_cursor: 0,
        }
    }

    pub fn submit_swap(&mut self, timeline: Arc<Timeline>, seq: u64) {
        self.pending = Some((timeline, seq));
    }

    pub fn set_midi(&mut self, midi: Option<Arc<MidiOut>>) {
        self.midi = midi;
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
            if let Some(tl) = &self.timeline {
                while self.event_cursor < tl.events.len()
                    && tl.events[self.event_cursor].sample_offset + self.timeline_origin <= now
                {
                    let e = &tl.events[self.event_cursor];
                    self.active.push(Active {
                        until: self.timeline_origin + e.sample_offset + e.duration,
                        loop_index: e.loop_index,
                        generation: e.generation,
                        voice: Voice::new(
                            e.voice,
                            cymbal_core::dsp::VoiceParams::from_event(e),
                            self.sample_rate,
                        ),
                        velocity: e.velocity,
                        pan: e.pan,
                        delay_send: e.delay_send,
                        reverb_send: e.reverb_send,
                    });
                    self.event_cursor += 1;
                }
            }
            self.active.retain(|a| a.until > now);
            if let Some(tl) = &self.timeline {
                while self.midi_cursor < tl.midi.len()
                    && tl.midi[self.midi_cursor].sample_offset + self.timeline_origin <= now
                {
                    let m = &tl.midi[self.midi_cursor];
                    if let Some(out) = &self.midi {
                        out.try_send(MidiItem::Note {
                            offset: self.timeline_origin + m.sample_offset,
                            bytes: m.bytes,
                        });
                    }
                    self.midi_cursor += 1;
                }
            }
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
                    if let Some(tl) = &self.timeline {
                        let name = tl.loops.get(a.loop_index as usize);
                        if let Some((_, l, r)) = name
                            .and_then(|name| self.track_acc.iter_mut().find(|(n, _, _)| n == name))
                        {
                            let angle = (a.pan + 1.0) * std::f32::consts::PI / 4.0;
                            *l += s * a.velocity * angle.cos();
                            *r += s * a.velocity * angle.sin();
                        }
                    }
                }
            }
            let mut frame_out = [0.0f32; 2];
            self.master.end_frame(&mut frame_out);
            out[frame * 2] = frame_out[0];
            out[frame * 2 + 1] = frame_out[1];
            self.push_rec_frame(frame_out[0], frame_out[1]);
            for (name, l, r) in &self.track_acc {
                if let Some((_, st)) = self.tracks.iter_mut().find(|(n, _)| n == name) {
                    st.current[st.pos * 2] = *l;
                    st.current[st.pos * 2 + 1] = *r;
                    st.pos += 1;
                    if st.pos == st.rec.block_frames() {
                        let full = std::mem::replace(&mut st.current, st.rec.take_pool_block());
                        st.rec.push_filled(full);
                        st.pos = 0;
                    }
                }
            }
            for (_, l, r) in &mut self.track_acc {
                *l = 0.0;
                *r = 0.0;
            }
        }
        self.position += frames as u64;
    }

    pub fn start_recording(&mut self, rec: Arc<Recorder>, tracks: Vec<(String, Arc<Recorder>)>) {
        if self.rec_state.is_some() {
            self.stop_recording();
        }
        self.rec_state = Some(RecState {
            rec: rec.clone(),
            current: rec.take_pool_block(),
            pos: 0,
        });
        self.tracks = tracks
            .into_iter()
            .map(|(name, rec)| {
                (
                    name.clone(),
                    TrackState {
                        current: rec.take_pool_block(),
                        rec,
                        pos: 0,
                    },
                )
            })
            .collect();
        self.track_acc = self
            .tracks
            .iter()
            .map(|(n, _)| (n.clone(), 0.0, 0.0))
            .collect();
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
        let names: Vec<String> = self.tracks.iter().map(|(n, _)| n.clone()).collect();
        for name in names {
            self.flush_track(&name);
        }
        self.track_acc.clear();
    }

    fn flush_track(&mut self, name: &str) {
        if let Some(idx) = self.tracks.iter().position(|(n, _)| n == name) {
            let (_, mut st) = self.tracks.remove(idx);
            let pos = st.pos;
            if pos > 0 {
                for s in &mut st.current[pos * 2..] {
                    *s = 0.0;
                }
                st.rec.push_filled(st.current);
            }
            st.rec.stop();
        }
        self.track_acc.retain(|(n, _, _)| n != name);
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
            self.timeline = Some(tl.clone());
            let kept: HashMap<&str, u64> = tl
                .loop_generations
                .iter()
                .map(|(n, g)| (n.as_str(), *g))
                .collect();
            self.active.retain(|a| {
                tl.loops
                    .get(a.loop_index as usize)
                    .and_then(|n| kept.get(n.as_str()))
                    == Some(&a.generation)
            });
            self.event_cursor = 0;
            self.midi_cursor = 0;
            if !tl.midi.is_empty()
                && let Some(m) = &self.midi
            {
                m.try_send(MidiItem::Rebase {
                    offset: now,
                    tempo: tl.tempo,
                });
            }
            let mut i = 0;
            while i < self.tracks.len() {
                if tl.loops.contains(&self.tracks[i].0) {
                    i += 1;
                } else {
                    let name = self.tracks[i].0.clone();
                    self.flush_track(&name);
                }
            }
            self.track_acc = self
                .tracks
                .iter()
                .map(|(n, _)| (n.clone(), 0.0, 0.0))
                .collect();
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
            midi: vec![],
        })
    }

    fn ev(offset: u64, voice: VoiceKind, generation: u64, duration: u64) -> Event {
        Event {
            sample_offset: offset,
            loop_name: "b".into(),
            loop_index: 0,
            voice,
            pitch: None,
            semitone: 0,
            velocity: 1.0,
            duration,
            generation,
            pan: 0.0,
            delay_send: 0.0,
            reverb_send: 0.0,
            bass: 0.0,
            treble: 0.0,
            comp: 0.0,
            sample: None,
            sample_start: 0.0,
            sample_end: 1.0,
            sample_loop: false,
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
                        loop_index: 1,
                        voice: VoiceKind::Hat,
                        pitch: None,
                        semitone: 0,
                        velocity: 1.0,
                        duration: 2400,
                        generation: 1,
                        pan: 0.0,
                        delay_send: 0.0,
                        reverb_send: 0.0,
                        bass: 0.0,
                        treble: 0.0,
                        comp: 0.0,
                        sample: None,
                        sample_start: 0.0,
                        sample_end: 1.0,
                        sample_loop: false,
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
                    loop_index: 0,
                    voice: VoiceKind::Bass,
                    pitch: None,
                    semitone: 0,
                    velocity: 1.0,
                    duration: 50000,
                    generation: 1,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                    bass: 0.0,
                    treble: 0.0,
                    comp: 0.0,
                    sample: None,
                    sample_start: 0.0,
                    sample_end: 1.0,
                    sample_loop: false,
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
                    loop_index: 0,
                    voice: VoiceKind::Bass,
                    pitch: None,
                    semitone: 0,
                    velocity: 1.0,
                    duration: 50000,
                    generation: 1,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                    bass: 0.0,
                    treble: 0.0,
                    comp: 0.0,
                    sample: None,
                    sample_start: 0.0,
                    sample_end: 1.0,
                    sample_loop: false,
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
        engine.start_recording(rec.clone(), vec![]);
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
        engine.start_recording(rec.clone(), vec![]);
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
        engine.start_recording(rec1.clone(), vec![]);
        engine_step(&mut engine, 3);
        engine.stop_recording();
        let b1 = rec1.take_filled().unwrap();
        assert!(
            b1[..6].iter().any(|s| *s != 0.0),
            "first recording's partial block contains audio"
        );
        assert_eq!(&b1[6..], &[0.0, 0.0], "unfilled tail is zeroed");
        let rec2 = Recorder::new(4, 4);
        engine.start_recording(rec2.clone(), vec![]);
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
    fn recording_restart_guard_flushes_previous() {
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
        engine.start_recording(rec1.clone(), vec![]);
        engine_step(&mut engine, 3);
        let rec2 = Recorder::new(4, 4);
        engine.start_recording(rec2.clone(), vec![]);
        assert!(
            rec1.is_stopped(),
            "restart must stop the previous recording"
        );
        let b1 = rec1.take_filled().unwrap();
        assert!(
            b1[..6].iter().any(|s| *s != 0.0),
            "previous recording's partial block contains audio"
        );
        assert_eq!(&b1[6..], &[0.0, 0.0], "unfilled tail is zeroed");
        assert!(rec1.take_filled().is_none());
        engine_step(&mut engine, 4);
        engine.stop_recording();
        let blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec2.take_filled()).collect();
        assert_eq!(blocks.len(), 1, "second recording = one full block");
        assert!(
            blocks[0][..8].iter().any(|s| *s != 0.0),
            "second recording captures post-master signal"
        );
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
        engine.start_recording(rec.clone(), vec![]);
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

    #[test]
    fn tracks_record_only_their_own_loop() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![
                    Event {
                        sample_offset: 0,
                        loop_name: "k".into(),
                        loop_index: 0,
                        voice: VoiceKind::Kick,
                        pitch: None,
                        semitone: 0,
                        velocity: 1.0,
                        duration: 14400,
                        generation: 0,
                        pan: -1.0,
                        delay_send: 0.0,
                        reverb_send: 0.0,
                        sample: None,
                        bass: 0.0,
                        treble: 0.0,
                        comp: 0.0,
                        sample_start: 0.0,
                        sample_end: 1.0,
                        sample_loop: false,
                    },
                    Event {
                        sample_offset: 0,
                        loop_name: "h".into(),
                        loop_index: 1,
                        voice: VoiceKind::Hat,
                        pitch: None,
                        semitone: 0,
                        velocity: 1.0,
                        duration: 2400,
                        generation: 0,
                        pan: 1.0,
                        delay_send: 0.0,
                        reverb_send: 0.0,
                        sample: None,
                        bass: 0.0,
                        treble: 0.0,
                        comp: 0.0,
                        sample_start: 0.0,
                        sample_end: 1.0,
                        sample_loop: false,
                    },
                ],
                0,
                vec![("k".into(), 0), ("h".into(), 0)],
                24000,
            ),
            1,
        );
        let master = Recorder::new(4, 4);
        let rec_k = Recorder::new(4, 4);
        let rec_h = Recorder::new(4, 4);
        engine.start_recording(
            master.clone(),
            vec![("k".into(), rec_k.clone()), ("h".into(), rec_h.clone())],
        );
        engine_step(&mut engine, 4);
        engine.stop_recording();
        let k_blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec_k.take_filled()).collect();
        let h_blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec_h.take_filled()).collect();
        assert_eq!(k_blocks.len(), 1);
        assert_eq!(h_blocks.len(), 1);
        assert!(
            k_blocks[0][..8].iter().any(|s| *s != 0.0),
            "kick track has audio"
        );
        // frame 1, not frame 0: every voice starts at exactly 0 (attack ramp)
        let k_l = k_blocks[0][2];
        let k_r = k_blocks[0][3];
        assert!(k_l.abs() > k_r.abs() * 10.0, "kick pan=-1 tracks left");
        let h_l = h_blocks[0][2];
        let h_r = h_blocks[0][3];
        assert!(h_r.abs() > h_l.abs() * 10.0, "hat pan=1 tracks right");
    }

    #[test]
    fn removed_loop_track_is_flushed_on_swap() {
        let mut engine = Engine::new(120.0, 48000);
        let mut hat = ev(0, VoiceKind::Hat, 0, 2400);
        hat.loop_name = "h".into();
        engine.submit_swap(tl(vec![hat], 0, vec![("h".into(), 0)], 3), 1);
        let rec_h = Recorder::new(4, 4);
        engine.start_recording(Recorder::new(4, 4), vec![("h".into(), rec_h.clone())]);
        engine_step(&mut engine, 3);
        // swap drops loop "h"
        engine.submit_swap(tl(vec![], 1, vec![], 3), 2);
        engine_step(&mut engine, 4);
        assert!(
            rec_h.is_stopped(),
            "removed loop's recorder must be stopped"
        );
        let b = rec_h.take_filled().unwrap();
        assert!(b[..6].iter().any(|s| *s != 0.0), "partial track flushed");
        assert_eq!(&b[6..], &[0.0, 0.0], "unfilled tail zeroed");
    }

    #[test]
    fn midi_events_fire_at_offsets() {
        use crate::midi_out::MidiOut;
        let mut engine = Engine::new(120.0, 48000);
        let midi = MidiOut::new(64);
        engine.set_midi(Some(midi.clone()));
        let mut tl = (*tl(
            vec![ev(0, VoiceKind::Hat, 0, 2400)],
            0,
            vec![("b".into(), 0)],
            24000,
        ))
        .clone();
        tl.midi = vec![
            cymbal_core::midi::MidiEvent {
                sample_offset: 0,
                bytes: [0x99, 42, 127],
            },
            cymbal_core::midi::MidiEvent {
                sample_offset: 2400,
                bytes: [0x80, 42, 0],
            },
        ];
        engine.submit_swap(Arc::new(tl), 1);
        engine_step(&mut engine, 4800);
        let a = midi.take_note();
        let b = midi.take_note();
        assert_eq!(a, Some([0x99, 42, 127]));
        assert_eq!(b, Some([0x80, 42, 0]));
        assert!(midi.take_note().is_none());
    }

    #[test]
    fn midi_rebases_on_swap() {
        use crate::midi_out::MidiOut;
        let mut engine = Engine::new(120.0, 48000);
        let midi = MidiOut::new(64);
        engine.set_midi(Some(midi.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1);
        engine_step(&mut engine, 24000);
        let mut tl2 = (*tl(
            vec![ev(0, VoiceKind::Hat, 1, 2400)],
            1,
            vec![("b".into(), 1)],
            24000,
        ))
        .clone();
        tl2.midi = vec![cymbal_core::midi::MidiEvent {
            sample_offset: 0,
            bytes: [0x99, 42, 100],
        }];
        engine.submit_swap(Arc::new(tl2), 2);
        let out = engine_step(&mut engine, 24000);
        assert_eq!(
            midi.take_rebase_offset(),
            Some(24000),
            "rebase at the boundary"
        );
        assert_eq!(midi.take_note(), Some([0x99, 42, 100]));
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn triggers_via_cursor_across_tempo_change() {
        // same behavior pin as swap_rebases_grid_on_tempo_change, but exercises
        // the cursor path: events fire from the swapped timeline at origin + offset
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1);
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
        assert!(out[0..16].iter().any(|s| *s != 0.0), "kick at origin + 0");
        assert!(out[4800 * 2..4800 * 2 + 8].iter().all(|s| *s == 0.0));
        assert!(
            out[12000 * 2..12000 * 2 + 16].iter().any(|s| *s != 0.0),
            "kick at origin + 12000 on the new grid"
        );
    }
}
