use std::sync::{Arc, Mutex};

use cymbal_core::dsp::Voice;
use cymbal_core::mixer::{Master, VoiceOutput};
use cymbal_core::scheduler::Timeline;
use cymbal_core::transport::Transport;

use crate::midi_out::{MidiItem, MidiOut};
use crate::recorder::Recorder;
use crate::ui_queue::{UiEvent, UiQueue};

pub const SPARE_WATERMARK: usize = 32;

struct RecState {
    rec: Arc<Recorder>,
    current: Box<[f32]>,
    pos: usize,
}

#[derive(Clone)]
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
    window_end_abs: u64,
    last_bar_in_window: u64,
    segment_requested: bool,
    pending: Option<(Arc<Timeline>, u64, Vec<Arc<Recorder>>)>,
    timeline: Option<Arc<Timeline>>,
    active: Vec<Active>,
    master: Master,
    rec_state: Option<RecState>,
    spares: Vec<Arc<Recorder>>,
    tracks: Vec<Option<TrackState>>,
    track_acc: Vec<Option<(f32, f32)>>,
    generations: Vec<u64>,
    tracks_scratch: Vec<Option<TrackState>>,
    track_acc_scratch: Vec<Option<(f32, f32)>>,
    midi: Option<Arc<MidiOut>>,
    ui: Option<Arc<UiQueue>>,
    bar_count: u64,
    event_cursor: usize,
    midi_cursor: usize,
    next_pulse_abs: f64,
    pulse_period: f64,
    midi_dropped: u64,
    midi_dropped_reported: u64,
    retired: Mutex<Vec<Arc<Timeline>>>,
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
            window_end_abs: u64::MAX,
            last_bar_in_window: 0,
            segment_requested: false,
            pending: None,
            timeline: None,
            active: Vec::with_capacity(512),
            master: Master::new(sample_rate, bar_samples),
            rec_state: None,
            spares: Vec::with_capacity(512),
            tracks: Vec::with_capacity(512),
            track_acc: Vec::with_capacity(512),
            generations: Vec::with_capacity(512),
            tracks_scratch: Vec::with_capacity(512),
            track_acc_scratch: Vec::with_capacity(512),
            midi: None,
            ui: None,
            bar_count: 0,
            event_cursor: 0,
            midi_cursor: 0,
            next_pulse_abs: f64::MAX,
            pulse_period: 1.0,
            midi_dropped: 0,
            midi_dropped_reported: 0,
            retired: Mutex::new(Vec::with_capacity(8)),
        }
    }

    pub fn submit_swap(&mut self, timeline: Arc<Timeline>, seq: u64, spares: Vec<Arc<Recorder>>) {
        if let Some((old, _, _)) = self.pending.replace((timeline, seq, spares)) {
            self.retired.lock().unwrap().push(old);
        }
    }

    pub fn take_retired(&self) -> Vec<Arc<Timeline>> {
        self.retired
            .lock()
            .map(|mut v| v.drain(..).collect())
            .unwrap()
    }

    pub(crate) fn take_retired_into(
        &self,
        n: usize,
        mut push: impl FnMut(Arc<Timeline>) -> Result<(), Arc<Timeline>>,
    ) -> usize {
        let mut v = self.retired.lock().unwrap();
        let n = n.min(v.len());
        let mut pushed = 0;
        for _ in 0..n {
            let tl = v.remove(0);
            match push(tl) {
                Ok(()) => pushed += 1,
                Err(tl) => v.push(tl),
            }
        }
        pushed
    }

    pub fn set_midi(&mut self, midi: Option<Arc<MidiOut>>) {
        self.midi = midi;
    }

    pub fn set_ui(&mut self, ui: Option<Arc<UiQueue>>) {
        self.ui = ui;
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
                if let Some(ui) = &self.ui {
                    ui.try_push(UiEvent::Bar(self.bar_count));
                }
                if self.midi_dropped > self.midi_dropped_reported {
                    self.midi_dropped_reported = self.midi_dropped;
                    if let Some(ui) = &self.ui {
                        ui.try_push(UiEvent::MidiDropped(self.midi_dropped));
                    }
                }
                if !self.segment_requested
                    && self.window_end_abs != u64::MAX
                    && self.bar_count >= self.last_bar_in_window
                {
                    self.segment_requested = true;
                    if let Some(ui) = &self.ui {
                        ui.try_push(UiEvent::NeedSegment(self.window_end_abs));
                    }
                }
                self.bar_count += 1;
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
                    if let Some(out) = &self.midi
                        && !out.try_send(MidiItem::Note {
                            offset: self.timeline_origin + m.sample_offset,
                            bytes: m.bytes,
                        })
                    {
                        self.midi_dropped = self.midi_dropped.saturating_add(1);
                    }
                    self.midi_cursor += 1;
                }
            }
            while self.next_pulse_abs <= now as f64 {
                if let Some(m) = &self.midi
                    && !m.try_send(MidiItem::Clock {
                        offset: self.next_pulse_abs as u64,
                    })
                {
                    self.midi_dropped = self.midi_dropped.saturating_add(1);
                }
                self.next_pulse_abs += self.pulse_period;
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
                    if let Some(acc) = self
                        .track_acc
                        .get_mut(a.loop_index as usize)
                        .and_then(|o| o.as_mut())
                    {
                        let angle = (a.pan + 1.0) * std::f32::consts::PI / 4.0;
                        acc.0 += s * a.velocity * angle.cos();
                        acc.1 += s * a.velocity * angle.sin();
                    }
                }
            }
            let mut frame_out = [0.0f32; 2];
            self.master.end_frame(&mut frame_out);
            out[frame * 2] = frame_out[0];
            out[frame * 2 + 1] = frame_out[1];
            self.push_rec_frame(frame_out[0], frame_out[1]);
            for (i, acc) in self.track_acc.iter().enumerate() {
                let Some((l, r)) = acc else { continue };
                let Some(st) = self.tracks.get_mut(i).and_then(|o| o.as_mut()) else {
                    continue;
                };
                st.current[st.pos * 2] = *l;
                st.current[st.pos * 2 + 1] = *r;
                st.pos += 1;
                if st.pos == st.rec.block_frames() {
                    let full = std::mem::replace(&mut st.current, st.rec.take_pool_block());
                    st.rec.push_filled(full);
                    st.pos = 0;
                }
            }
            for acc in self.track_acc.iter_mut().flatten() {
                *acc = (0.0, 0.0);
            }
        }
        self.position += frames as u64;
        if let Some(ui) = &self.ui {
            ui.store_position(self.position);
        }
    }

    pub fn start_recording(
        &mut self,
        rec: Arc<Recorder>,
        tracks: Vec<(String, Arc<Recorder>)>,
        spares: Vec<Arc<Recorder>>,
    ) {
        if self.rec_state.is_some() {
            self.stop_recording();
        }
        for s in spares {
            if self.spares.len() < SPARE_WATERMARK {
                self.spares.push(s);
            }
        }
        if let Some(m) = &self.midi {
            m.try_send(MidiItem::Sys {
                bytes: [0xFA, 0, 0],
                len: 1,
            });
        }
        self.rec_state = Some(RecState {
            rec: rec.clone(),
            current: rec.take_pool_block(),
            pos: 0,
        });
        let source = self
            .timeline
            .as_ref()
            .or(self.pending.as_ref().map(|(tl, _, _)| tl));
        let n = source.map_or(0, |t| t.loops.len());
        self.tracks.clear();
        self.tracks.resize(n, None);
        self.track_acc.clear();
        self.track_acc.resize(n, None);
        for (name, rec) in tracks {
            match source.and_then(|t| t.loops.iter().position(|n| n == &name)) {
                Some(idx) => {
                    self.tracks[idx] = Some(TrackState {
                        current: rec.take_pool_block(),
                        rec,
                        pos: 0,
                    });
                    self.track_acc[idx] = Some((0.0, 0.0));
                }
                None => rec.stop(),
            }
        }
    }

    pub fn stop_recording(&mut self) {
        if let Some(m) = &self.midi {
            m.try_send(MidiItem::Sys {
                bytes: [0xFC, 0, 0],
                len: 1,
            });
        }
        if let Some(state) = self.rec_state.take() {
            let pos = state.pos;
            let mut cur = state.current;
            if pos > 0 {
                for s in &mut cur[pos * 2..] {
                    *s = 0.0;
                }
                state.rec.push_filled(cur);
            } else {
                state.rec.return_block(cur);
            }
            state.rec.stop();
        }
        for st in self.tracks.iter_mut().flatten() {
            let pos = st.pos;
            let mut cur = std::mem::take(&mut st.current);
            if pos > 0 {
                for s in &mut cur[pos * 2..] {
                    *s = 0.0;
                }
                st.rec.push_filled(cur);
            } else {
                st.rec.return_block(cur);
            }
            st.rec.stop();
        }
        self.tracks.clear();
        self.track_acc.clear();
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
        let Some((tl, seq, spares)) = self.pending.take() else {
            return;
        };
        if seq <= self.last_swap_seq {
            self.retired.lock().unwrap().push(tl);
            // Accepted bounded dealloc on the audio thread: a stale swap's
            // spares drop here, bounded to stale-seq anomalies (a newer swap
            // already applied). Plan-accepted; the pin test counts
            // allocations only, so the dealloc keeps ALLOCS == 0.
            return;
        }
        self.last_swap_seq = seq;
        // Accepted bounded dealloc on the audio thread: spares beyond the
        // watermark drop here, bounded to a single reload carrying more
        // recorder Arcs than SPARE_WATERMARK. Plan-accepted; the pin test
        // counts allocations only, so the dealloc keeps ALLOCS == 0.
        for s in spares {
            if self.spares.len() < SPARE_WATERMARK {
                self.spares.push(s);
            }
        }

        let old_tl = self.timeline.replace(tl.clone());
        self.timeline_origin = now;
        self.bar_samples = tl.bar_samples;
        self.window_end_abs = tl.window_start.saturating_add(tl.window_len);
        // Relative latch: the window end is absolute, but the bar grid
        // restarts at the apply boundary — count bars from `now`, not from
        // absolute bar numbers. A window end already in the past saturates
        // to zero bars and fires at the applying boundary itself.
        self.last_bar_in_window = self
            .bar_count
            .saturating_add(
                (self
                    .window_end_abs
                    .saturating_sub(now)
                    .saturating_add(tl.bar_samples - 1))
                    / tl.bar_samples,
            )
            .saturating_sub(1);
        self.segment_requested = false;
        self.master.set_bar_samples(tl.bar_samples);
        self.event_cursor = 0;
        self.midi_cursor = 0;

        let max_index = tl.loops.len().min(512);
        self.generations.clear();
        self.generations.resize(max_index, 0);
        for (i, (_, g)) in tl.loop_generations.iter().take(max_index).enumerate() {
            self.generations[i] = *g;
        }

        if let Some(old) = old_tl.as_ref() {
            self.active.retain_mut(|a| {
                let name = old
                    .loops
                    .get(a.loop_index as usize)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                match tl.loops.iter().position(|n| n == name) {
                    Some(new_idx)
                        if self.generations.get(new_idx).copied() == Some(a.generation) =>
                    {
                        a.loop_index = new_idx as u32;
                        true
                    }
                    _ => false,
                }
            });
        } else {
            self.active
                .retain(|a| tl.loops.get(a.loop_index as usize).is_some());
        }

        let mut old_tracks = std::mem::take(&mut self.tracks);
        let old_acc = std::mem::take(&mut self.track_acc);
        self.tracks_scratch.clear();
        self.tracks_scratch.resize(tl.loops.len(), None);
        self.track_acc_scratch.clear();
        self.track_acc_scratch.resize(tl.loops.len(), None);
        for (old_idx, track) in old_tracks.drain(..).enumerate() {
            let name = match old_tl.as_ref() {
                Some(t) => t.loops.get(old_idx).map(|s| s.as_str()).unwrap_or(""),
                None => tl.loops.get(old_idx).map(|s| s.as_str()).unwrap_or(""),
            };
            match tl.loops.iter().position(|n| n == name) {
                Some(new_idx) => {
                    self.tracks_scratch[new_idx] = track;
                    self.track_acc_scratch[new_idx] = old_acc.get(old_idx).copied().flatten();
                }
                None => {
                    if let Some(mut st) = track {
                        let pos = st.pos;
                        if pos > 0 {
                            for s in &mut st.current[pos * 2..] {
                                *s = 0.0;
                            }
                            st.rec.push_filled(st.current);
                        } else {
                            st.rec.return_block(st.current);
                        }
                        st.rec.stop();
                    }
                }
            }
        }
        std::mem::swap(&mut self.tracks, &mut self.tracks_scratch);
        std::mem::swap(&mut self.track_acc, &mut self.track_acc_scratch);
        self.tracks_scratch = old_tracks;
        self.track_acc_scratch = old_acc;

        let seq = self.last_swap_seq;
        for (idx, slot) in self.tracks.iter_mut().enumerate() {
            if slot.is_none() {
                let is_new = old_tl.as_ref().is_some_and(|old| {
                    tl.loops
                        .get(idx)
                        .is_some_and(|name| !old.loops.contains(name))
                });
                if is_new && let Some(spare) = self.spares.pop() {
                    *slot = Some(TrackState {
                        rec: spare.clone(),
                        current: spare.take_pool_block(),
                        pos: 0,
                    });
                    self.track_acc[idx] = Some((0.0, 0.0));
                    if let Some(ui) = &self.ui {
                        ui.try_push(UiEvent::TrackClaimed {
                            rec: spare,
                            seq,
                            loop_index: idx as u32,
                        });
                    }
                }
            }
        }

        if let Some(m) = &self.midi {
            m.try_send(MidiItem::Rebase {
                offset: now,
                tempo: tl.tempo,
            });
        }

        self.pulse_period = tl.bar_samples as f64 / 96.0;
        self.next_pulse_abs = now as f64 + self.pulse_period;

        if let Some(old) = old_tl {
            self.retired.lock().unwrap().push(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::AudioQueue;
    use cymbal_core::ast::VoiceKind;
    use cymbal_core::scheduler::{Event, Timeline};
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting;
    static ALLOCS: AtomicUsize = AtomicUsize::new(0);
    static FIRST_ALLOC_BT: Mutex<Option<std::backtrace::Backtrace>> = Mutex::new(None);

    // only the measuring test thread counts: parallel tests cannot pollute
    thread_local! {
        static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if COUNTING.with(|c| c.get()) {
                if ALLOCS.fetch_add(1, Ordering::SeqCst) == 0 {
                    *FIRST_ALLOC_BT.lock().unwrap() =
                        Some(std::backtrace::Backtrace::force_capture());
                }
            }
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

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
            window_start: 0,
            window_len: u64::MAX,
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
        );
        engine_step(&mut engine, 24000);
        engine.submit_swap(tl(vec![], 1, vec![], 24000), 2, vec![]);
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
            vec![],
        );
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            2,
            vec![],
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
            vec![],
        );
        let expect = engine_step(&mut reference, 24000);
        assert_eq!(out, expect);
    }

    #[test]
    fn empty_timeline_is_silent() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![], 96000), 1, vec![]);
        let mut out = vec![0.5f32; 96000 * 2];
        engine.process(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn swap_rebases_grid_on_tempo_change() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![], 24000), 1, vec![]);
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
            vec![],
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
            vec![],
        );
        let rec = Recorder::new(4, 4);
        engine.start_recording(rec.clone(), vec![], vec![]);
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
    fn record_start_and_stop_send_transport_messages() {
        use crate::midi_out::MidiOut;
        let mut engine = Engine::new(120.0, 48000);
        let midi = MidiOut::new(64);
        engine.set_midi(Some(midi.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine.start_recording(Recorder::new(4, 4), vec![], vec![]);
        engine_step(&mut engine, 4);
        assert_eq!(midi.take_sys(), Some(vec![0xFA]), "start on record");
        engine.stop_recording();
        assert_eq!(midi.take_sys(), Some(vec![0xFC]), "stop on record end");
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
            vec![],
        );
        let rec = Recorder::new(4, 4);
        engine.start_recording(rec.clone(), vec![], vec![]);
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
            vec![],
        );
        let rec1 = Recorder::new(4, 4);
        engine.start_recording(rec1.clone(), vec![], vec![]);
        engine_step(&mut engine, 3);
        engine.stop_recording();
        let b1 = rec1.take_filled().unwrap();
        assert!(
            b1[..6].iter().any(|s| *s != 0.0),
            "first recording's partial block contains audio"
        );
        assert_eq!(&b1[6..], &[0.0, 0.0], "unfilled tail is zeroed");
        let rec2 = Recorder::new(4, 4);
        engine.start_recording(rec2.clone(), vec![], vec![]);
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
            vec![],
        );
        let rec1 = Recorder::new(4, 4);
        engine.start_recording(rec1.clone(), vec![], vec![]);
        engine_step(&mut engine, 3);
        let rec2 = Recorder::new(4, 4);
        engine.start_recording(rec2.clone(), vec![], vec![]);
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
            vec![],
        );
        let rec = Recorder::new(64, 4800);
        engine.start_recording(rec.clone(), vec![], vec![]);
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
            vec![],
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
        engine.submit_swap(tl(vec![], 0, vec![], 24000), 1, vec![]);
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            2,
            vec![],
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
        engine.submit_swap(kick_tl.clone(), 1, vec![]);
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Snare, 1, 2400)],
                1,
                vec![("b".into(), 1)],
                24000,
            ),
            1,
            vec![],
        );
        let out = engine_step(&mut engine, 48000);
        let mut reference = Engine::new(120.0, 48000);
        reference.submit_swap(kick_tl, 1, vec![]);
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
            vec![],
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
            vec![],
        );
        let master = Recorder::new(4, 4);
        let rec_k = Recorder::new(4, 4);
        let rec_h = Recorder::new(4, 4);
        engine.start_recording(
            master.clone(),
            vec![("k".into(), rec_k.clone()), ("h".into(), rec_h.clone())],
            vec![],
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
        engine.submit_swap(tl(vec![hat], 0, vec![("h".into(), 0)], 3), 1, vec![]);
        let rec_h = Recorder::new(4, 4);
        engine.start_recording(
            Recorder::new(4, 4),
            vec![("h".into(), rec_h.clone())],
            vec![],
        );
        engine_step(&mut engine, 3);
        // swap drops loop "h"
        engine.submit_swap(tl(vec![], 1, vec![], 3), 2, vec![]);
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
    fn stop_recording_returns_inflight_blocks_to_pool() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![("h".into(), 0)], 3), 1, vec![]);
        let master = Recorder::new(4, 4);
        let rec_h = Recorder::new(4, 4);
        let master_before = master.pool_len();
        let track_before = rec_h.pool_len();
        engine.start_recording(master.clone(), vec![("h".into(), rec_h.clone())], vec![]);
        engine.stop_recording();
        assert_eq!(
            master.pool_len(),
            master_before,
            "master in-flight block returned to the pool"
        );
        assert_eq!(
            rec_h.pool_len(),
            track_before,
            "track in-flight block returned to the pool"
        );
    }

    #[test]
    fn removed_loop_recorder_returns_empty_inflight_block() {
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![("h".into(), 0)], 3), 1, vec![]);
        let rec_h = Recorder::new(4, 4);
        let before = rec_h.pool_len();
        engine.start_recording(
            Recorder::new(4, 4),
            vec![("h".into(), rec_h.clone())],
            vec![],
        );
        engine.submit_swap(tl(vec![], 1, vec![], 3), 2, vec![]);
        engine_step(&mut engine, 1);
        assert_eq!(
            rec_h.pool_len(),
            before,
            "empty in-flight block returned on loop removal"
        );
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
        engine.submit_swap(Arc::new(tl), 1, vec![]);
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
        let midi = MidiOut::new(8192);
        engine.set_midi(Some(midi.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
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
        engine.submit_swap(Arc::new(tl2), 2, vec![]);
        let out = engine_step(&mut engine, 24000);
        assert_eq!(
            midi.take_rebase_offset(),
            Some(0),
            "rebase at the first swap"
        );
        assert_eq!(
            midi.take_rebase_offset(),
            Some(24000),
            "rebase at the boundary"
        );
        assert_eq!(midi.take_note(), Some([0x99, 42, 100]));
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn midi_rebases_for_sample_only_songs() {
        use crate::midi_out::MidiOut;
        let mut engine = Engine::new(120.0, 48000);
        let midi = MidiOut::new(8192);
        engine.set_midi(Some(midi.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine_step(&mut engine, 24000);
        assert_eq!(
            midi.take_rebase_offset(),
            Some(0),
            "the rebase anchors the clock even without midi events"
        );
    }

    #[test]
    fn triggers_via_cursor_across_tempo_change() {
        // same behavior pin as swap_rebases_grid_on_tempo_change, but exercises
        // the cursor path: events fire from the swapped timeline at origin + offset
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
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
            vec![],
        );
        let out = engine_step(&mut engine, 24000);
        assert!(out[0..16].iter().any(|s| *s != 0.0), "kick at origin + 0");
        assert!(out[4800 * 2..4800 * 2 + 8].iter().all(|s| *s == 0.0));
        assert!(
            out[12000 * 2..12000 * 2 + 16].iter().any(|s| *s != 0.0),
            "kick at origin + 12000 on the new grid"
        );
    }

    #[test]
    fn retained_voice_survives_shrinking_loop_list() {
        let mut engine = Engine::new(120.0, 48000);
        let mut hat = ev(0, VoiceKind::Hat, 0, 50000);
        hat.loop_name = "h".into();
        hat.loop_index = 1;
        engine.submit_swap(
            tl(vec![hat], 0, vec![("b".into(), 0), ("h".into(), 0)], 2000),
            1,
            vec![],
        );
        engine_step(&mut engine, 2000);
        engine.submit_swap(tl(vec![], 1, vec![("h".into(), 0)], 2000), 2, vec![]);
        let out = engine_step(&mut engine, 2000);
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "ringing h voice must survive when the loop list shrinks"
        );
    }

    #[test]
    fn reordered_loops_reindex_retained_voices() {
        // loop "a" (bass) is sounding; a swap reorders loops to ["c", "a"] with
        // "a" unchanged (gen 0) and no events — the retained bass is the only
        // signal source, so audio after the swap proves the voice survived.
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![Event {
                    sample_offset: 0,
                    loop_name: "a".into(),
                    loop_index: 0,
                    voice: VoiceKind::Bass,
                    pitch: Some(60),
                    semitone: 0,
                    velocity: 1.0,
                    duration: 50000,
                    generation: 0,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                    sample: None,
                    bass: 0.0,
                    treble: 0.0,
                    comp: 0.0,
                    sample_start: 0.0,
                    sample_end: 1.0,
                    sample_loop: false,
                }],
                0,
                vec![("a".into(), 0)],
                2000,
            ),
            1,
            vec![],
        );
        engine_step(&mut engine, 2000);
        engine.submit_swap(
            tl(vec![], 0, vec![("c".into(), 0), ("a".into(), 0)], 2000),
            2,
            vec![],
        );
        let out = engine_step(&mut engine, 2000);
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "bass must survive the reorder (reindexed, not cut)"
        );
    }

    #[test]
    fn changed_loop_is_cut_after_reorder() {
        // loop "a" gen 0 sounding; swap reorders to ["c", "a"] with "a" gen 1
        // and no events — the boundary lands inside the voice's audible tail,
        // so silence can only mean the gen-0 voice was actually cut.
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Bass, 0, 50000)],
                0,
                vec![("a".into(), 0)],
                2000,
            ),
            1,
            vec![],
        );
        engine_step(&mut engine, 2000);
        engine.submit_swap(
            tl(vec![], 1, vec![("c".into(), 0), ("a".into(), 1)], 2000),
            2,
            vec![],
        );
        let out = engine_step(&mut engine, 4800);
        assert!(
            out[0..16].iter().all(|s| *s == 0.0),
            "reindexed gen-0 voice must be cut when its loop changed"
        );
    }

    #[test]
    fn retained_voice_routes_to_reindexed_track() {
        // loops ["a", "b"] with bass on "a" (index 0) ringing; a swap reorders
        // to ["b", "a"] with no events — the retained bass must be reindexed to
        // position 1 so its stem keeps landing in loop "a"'s track, not "b"'s.
        let mut engine = Engine::new(120.0, 48000);
        let mut bass = ev(0, VoiceKind::Bass, 0, 50000);
        bass.loop_name = "a".into();
        engine.submit_swap(
            tl(vec![bass], 0, vec![("a".into(), 0), ("b".into(), 0)], 2000),
            1,
            vec![],
        );
        engine_step(&mut engine, 2000);
        let rec_a = Recorder::new(4, 2000);
        let rec_b = Recorder::new(4, 2000);
        engine.start_recording(
            Recorder::new(4, 2000),
            vec![("a".into(), rec_a.clone()), ("b".into(), rec_b.clone())],
            vec![],
        );
        engine_step(&mut engine, 2000);
        engine.submit_swap(
            tl(vec![], 1, vec![("b".into(), 0), ("a".into(), 0)], 2000),
            2,
            vec![],
        );
        let out = engine_step(&mut engine, 2000);
        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "retained bass keeps ringing after the reorder"
        );
        engine_step(&mut engine, 2000);
        engine_step(&mut engine, 2000);
        engine.stop_recording();
        let a_blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec_a.take_filled()).collect();
        let b_blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec_b.take_filled()).collect();
        assert_eq!(a_blocks.len(), 4, "one block per recorded bar for loop a");
        assert_eq!(b_blocks.len(), 4, "one block per recorded bar for loop b");
        assert!(
            a_blocks[1][..16].iter().any(|s| *s != 0.0),
            "retained bass must route to loop a's track after the reorder"
        );
        assert!(
            b_blocks.iter().all(|b| b.iter().all(|s| *s == 0.0)),
            "loop b's track must stay silent"
        );
    }

    #[test]
    fn swap_and_steady_state_allocate_nothing() {
        // build everything first, then snapshot: harness allocations (Arc,
        // Vecs, out buffer) must not be counted
        let tl_a = tl(
            vec![ev(0, VoiceKind::Hat, 0, 2400)],
            0,
            vec![("b".into(), 0)],
            24000,
        );
        let tl_b = tl(
            vec![ev(0, VoiceKind::Hat, 1, 2400)],
            1,
            vec![("b".into(), 1), ("c".into(), 1)],
            24000,
        );
        let tl_c = tl(
            vec![ev(0, VoiceKind::Hat, 2, 2400)],
            2,
            vec![("b".into(), 2), ("c".into(), 2), ("d".into(), 2)],
            24000,
        );
        // windowed segment advance: tl_b covers [48000, 96000), tl_c
        // [96000, 144000) — the engine fires NeedSegment one bar before
        // each window end, inside the counted region
        let mut tl_b = (*tl_b).clone();
        tl_b.window_start = 48000;
        tl_b.window_len = 48000;
        let mut tl_c = (*tl_c).clone();
        tl_c.window_start = 96000;
        tl_c.window_len = 48000;
        let tl_b = Arc::new(tl_b);
        let tl_c = Arc::new(tl_c);
        let mut engine = Engine::new(120.0, 48000);
        let mut out = vec![0.0f32; 48000 * 2];
        let midi = crate::midi_out::MidiOut::new(8192);
        engine.set_midi(Some(midi.clone()));
        let ui = UiQueue::new(64);
        engine.set_ui(Some(ui.clone()));
        let retired_q = AudioQueue::new(4);
        engine.submit_swap(tl_a, 1, vec![]);
        engine.process(&mut out);
        let rec = Recorder::new(4, 4);
        let rec2 = Recorder::new(4, 4);
        let spare = Recorder::new(4, 4);
        let spare2 = Recorder::new(4, 4);
        let spare3 = Recorder::new(4, 4);
        let tracks = vec![("b".into(), rec2.clone())];
        let spares = vec![spare.clone()];
        let refill = vec![spare2.clone(), spare3.clone()];
        engine.stop_recording();
        COUNTING.with(|c| c.set(true));
        ALLOCS.store(0, Ordering::SeqCst);
        engine.start_recording(rec.clone(), tracks, spares);
        engine.submit_swap(tl_b, 2, vec![]);
        engine.process(&mut out);
        engine.take_retired_into(retired_q.retired_available(), |tl| {
            retired_q.push_retired(tl)
        });
        engine.submit_swap(tl_c, 3, refill);
        engine.process(&mut out);
        assert_eq!(
            engine.spares.len(),
            1,
            "the swap-carried spares must land in the pool, one claimed by \
             the new loop"
        );
        engine.stop_recording();
        let count = ALLOCS.load(Ordering::SeqCst);
        COUNTING.with(|c| c.set(false));
        if count != 0 {
            eprintln!(
                "FIRST COUNTED ALLOCATION:\n{}",
                FIRST_ALLOC_BT
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|b| format!("{b}"))
                    .unwrap_or_default()
            );
        }
        let mut segments = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::NeedSegment(end) = ev {
                segments.push(end);
            }
        }
        assert_eq!(
            segments,
            vec![96000, 144000],
            "one segment request per window, fired in its last bar"
        );
        assert_eq!(
            engine.timeline.as_ref().map(|t| t.generation),
            Some(2),
            "both counted swaps must have applied"
        );
        let drained = retired_q.take_retired();
        assert_eq!(
            drained.len(),
            1,
            "the first retirement was pushed through the bridge drain"
        );
        assert_eq!(
            drained[0].generation, 0,
            "the drained timeline is the first one"
        );
        assert_eq!(
            engine
                .take_retired()
                .iter()
                .map(|t| t.generation)
                .collect::<Vec<_>>(),
            vec![1],
            "the retirement pushed after the in-window drain is still delivered"
        );
        let b = rec2.take_filled().unwrap();
        assert!(
            b[..8].iter().any(|s| *s != 0.0),
            "the per-track tap must capture the hat"
        );
        assert_eq!(
            count, 0,
            "swap, retirement drain, spare refill and claim, triggers, clock \
             pulses, bar, claimed-track and segment-request events, and \
             recording taps must not allocate"
        );
    }

    #[test]
    fn same_offset_burst_uses_the_cursor() {
        // 100k events at offset 0: the cursor path fires them all in the first
        // frame without memmoving the remainder (remove(0) would be O(n^2))
        let mut events = Vec::with_capacity(100_000);
        for _ in 0..100_000u64 {
            events.push(Event {
                sample_offset: 0,
                loop_name: "b".into(),
                loop_index: 0,
                voice: VoiceKind::Hat,
                pitch: None,
                semitone: 0,
                velocity: 1.0,
                duration: 2400,
                generation: 0,
                pan: 0.0,
                delay_send: 0.0,
                reverb_send: 0.0,
                sample: None,
                bass: 0.0,
                treble: 0.0,
                comp: 0.0,
                sample_start: 0.0,
                sample_end: 1.0,
                sample_loop: false,
            });
        }
        let tl = Arc::new(Timeline {
            events,
            generation: 0,
            tempo: 120.0,
            bar_samples: 24000,
            sample_rate: 48000,
            loops: vec!["b".into()],
            loop_generations: vec![("b".into(), 0)],
            midi: vec![],
            window_start: 0,
            window_len: u64::MAX,
        });
        let mut engine = Engine::new(120.0, 48000);
        engine.submit_swap(tl, 1, vec![]);
        let mut out = vec![0.0f32; 100 * 2];
        engine.process(&mut out);
        assert_eq!(
            engine.event_cursor, 100_000,
            "all events consumed by the cursor"
        );
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn clock_pulses_fire_on_the_24ppqn_grid() {
        let mut engine = Engine::new(120.0, 48000);
        let midi = crate::midi_out::MidiOut::new(8192);
        engine.set_midi(Some(midi.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine_step(&mut engine, 2500);
        let mut pulses = Vec::new();
        while let Some(item) = midi.take_clock() {
            pulses.push(item);
        }
        assert_eq!(
            pulses.len(),
            9,
            "one pulse per 250 samples from 250 to 2250"
        );
        assert_eq!(pulses.first(), Some(&250), "boundary pulse skipped");
        assert_eq!(pulses.last(), Some(&2250));
    }

    #[test]
    fn pulse_grid_restarts_at_swap_boundary() {
        let mut engine = Engine::new(120.0, 48000);
        let midi = crate::midi_out::MidiOut::new(8192);
        engine.set_midi(Some(midi.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine_step(&mut engine, 24000);
        engine.submit_swap(tl(vec![], 1, vec![("b".into(), 1)], 12000), 2, vec![]);
        engine_step(&mut engine, 12000);
        let mut pulses = Vec::new();
        while let Some(p) = midi.take_clock() {
            pulses.push(p);
        }
        assert!(!pulses.contains(&24000), "boundary pulse is skipped");
        assert!(pulses.contains(&24125));
    }

    #[test]
    fn segment_request_fires_in_last_bar() {
        // windowed timeline: [0, 2 bars); process a bar so we sit in bar 1,
        // the last bar of the window; the boundary entering bar 1 must push
        // exactly one NeedSegment carrying the window end.
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        let mut tl = (*tl(vec![], 0, vec![("b".into(), 0)], 24000)).clone();
        tl.window_start = 0;
        tl.window_len = 24000 * 2;
        engine.submit_swap(Arc::new(tl), 1, vec![]);
        engine_step(&mut engine, 24000);
        engine_step(&mut engine, 24000);
        let mut segments = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::NeedSegment(end) = ev {
                segments.push(end);
            }
        }
        assert_eq!(
            segments,
            vec![48000],
            "one request with the window end, fired in the last bar"
        );
    }

    #[test]
    fn single_shot_timeline_never_requests_segments() {
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine_step(&mut engine, 24000 * 4);
        let mut segments = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::NeedSegment(end) = ev {
                segments.push(end);
            }
        }
        assert!(segments.is_empty(), "u64::MAX window = no segments");
    }

    #[test]
    fn anchored_reload_window_requests_segment_at_anchored_end() {
        // A reload anchored at the boundary where its swap applies (past the
        // previous window's end) must fire NeedSegment with the anchored end
        // in the last bar of the anchored window. The old floor math
        // (window_end / bar_samples - 1) would wait until the absolute bar
        // of the window end, and a 0-anchored window would schedule its
        // events ~300s behind the playhead.
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        let mut first = (*tl(vec![], 0, vec![("b".into(), 0)], 96000)).clone();
        first.window_start = 0;
        first.window_len = 96000 * 2;
        engine.submit_swap(Arc::new(first), 1, vec![]);
        engine_step(&mut engine, 96000 * 2);
        // cross the first window's end; the engine now sits in silence past
        // it, with the next boundary at 288000
        let quiet = engine_step(&mut engine, 96000);
        // 240-bpm reload: window [288000, 336000) = two 24000-sample bars,
        // anchored at the boundary where its swap applies
        let mut reload = (*tl(
            vec![
                ev(0, VoiceKind::Hat, 1, 2400),
                ev(24000, VoiceKind::Hat, 1, 2400),
            ],
            1,
            vec![("b".into(), 1)],
            24000,
        ))
        .clone();
        reload.window_start = 288000;
        reload.window_len = 24000 * 2;
        engine.submit_swap(Arc::new(reload), 2, vec![]);
        // the swap applies at the first frame (288000); then four anchored bars
        let out = engine_step(&mut engine, 24000 * 4);
        assert!(
            quiet.iter().all(|s| *s == 0.0),
            "silence while the engine sits past the first window's end"
        );
        assert!(
            out[..16].iter().any(|s| *s != 0.0),
            "the first hit rings from the anchor (288000), not from 0"
        );
        assert!(
            out[24000 * 2..24000 * 2 + 16].iter().any(|s| *s != 0.0),
            "the second hit fires one anchored bar later (312000)"
        );
        assert!(
            out[2400 * 2..24000 * 2].iter().all(|s| *s == 0.0),
            "nothing between the anchored hits"
        );
        let mut segments = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::NeedSegment(end) = ev {
                segments.push(end);
            }
        }
        assert_eq!(
            segments,
            vec![192000, 336000],
            "the first window requests its end; the anchored reload requests \
             the anchored end in the anchored window's last bar (bar 4, not \
             absolute bar 13)"
        );
    }

    #[test]
    fn tempo_change_latch_fires_relative_to_the_anchor() {
        // 120→240 bpm reload with a two-bar anchored window at small scale:
        // the latch counts bars from the apply boundary. The old floor math
        // fires at absolute bar 7 (480000); the relative latch fires in the
        // window's last bar (336000), long before.
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 96000), 1, vec![]);
        engine_step(&mut engine, 96000 * 2);
        // cross the 192000 boundary so the reload applies at 288000
        engine_step(&mut engine, 96000);
        let mut reload = (*tl(vec![], 1, vec![("b".into(), 1)], 48000)).clone();
        reload.window_start = 288000;
        reload.window_len = 48000 * 2;
        engine.submit_swap(Arc::new(reload), 2, vec![]);
        // three anchored bars from the applying boundary: the request fires
        // at bar 4 (position 336000), the last window bar
        engine_step(&mut engine, 48000 * 3);
        let mut segments = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::NeedSegment(end) = ev {
                segments.push(end);
            }
        }
        assert_eq!(
            segments,
            vec![384000],
            "the request fires in the last bar of the anchored window"
        );
    }

    #[test]
    fn window_entirely_in_the_past_requests_segment_immediately() {
        // The reload lands after its window already elapsed: the saturating
        // latch fires NeedSegment at the applying boundary instead of
        // waiting for a bar count the window never reaches.
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 96000), 1, vec![]);
        engine_step(&mut engine, 96000 * 2);
        // cross the 192000 boundary so the reload applies at 288000, where
        // its window [192000, 240000) is already entirely in the past
        engine_step(&mut engine, 96000);
        let mut reload = (*tl(vec![], 1, vec![("b".into(), 1)], 24000)).clone();
        reload.window_start = 192000;
        reload.window_len = 24000 * 2;
        engine.submit_swap(Arc::new(reload), 2, vec![]);
        // the applying boundary is the first frame: the saturating latch must
        // fire NeedSegment right there
        engine_step(&mut engine, 1);
        let mut segments = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::NeedSegment(end) = ev {
                segments.push(end);
            }
        }
        assert_eq!(
            segments,
            vec![240000],
            "a window that already ended requests its end at the applying boundary"
        );
    }

    #[test]
    fn bar_ticks_are_pushed_at_boundaries() {
        use crate::ui_queue::{UiEvent, UiQueue};
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine_step(&mut engine, 72000);
        let mut bars = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::Bar(n) = ev {
                bars.push(n);
            }
        }
        assert_eq!(bars, vec![0, 1, 2], "a bar tick at each boundary");
    }

    #[test]
    fn bar_counter_stays_monotonic_across_tempo_change() {
        use crate::ui_queue::{UiEvent, UiQueue};
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 12000), 1, vec![]);
        engine_step(&mut engine, 12000);
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 2, vec![]);
        engine_step(&mut engine, 72000);
        let mut bars = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::Bar(n) = ev {
                bars.push(n);
            }
        }
        assert_eq!(bars, vec![0, 1, 2, 3], "bars counted across the tempo swap");
        assert!(
            bars.windows(2).all(|w| w[0] < w[1]),
            "bar ticks must never go backward"
        );
    }

    #[test]
    fn midi_overflow_is_reported_once_per_change() {
        use crate::ui_queue::{UiEvent, UiQueue};
        let ui = UiQueue::new(64);
        let midi = crate::midi_out::MidiOut::new(1);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_midi(Some(midi.clone()));
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine_step(&mut engine, 24001);
        let mut dropped = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::MidiDropped(n) = ev {
                dropped.push(n);
            }
        }
        assert_eq!(
            dropped,
            vec![95],
            "one report per bar with the cumulative dropped count"
        );
    }

    #[test]
    fn new_loop_at_swap_claims_a_spare() {
        use crate::ui_queue::{UiEvent, UiQueue};
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        let spares = vec![Recorder::new(4, 4)];
        engine.start_recording(Recorder::new(4, 4), vec![], spares);
        engine_step(&mut engine, 24000);
        // swap adds loop "h" (index 0 in the new timeline)
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 1, 2400)],
                1,
                vec![("h".into(), 1)],
                24000,
            ),
            2,
            vec![],
        );
        engine_step(&mut engine, 24000);
        let claim = loop {
            match ui.try_pop() {
                Some(UiEvent::TrackClaimed {
                    rec,
                    seq,
                    loop_index,
                }) => break Some((rec, seq, loop_index)),
                Some(_) => continue,
                None => break None,
            }
        };
        let (rec, seq, loop_index) = claim.expect("a claim must be pushed");
        assert_eq!(seq, 2);
        assert_eq!(loop_index, 0);
        let blocks: Vec<Box<[f32]>> = std::iter::from_fn(|| rec.take_filled()).collect();
        assert!(
            blocks.iter().flatten().any(|s| *s != 0.0),
            "the claimed recorder captures the hat's dry audio"
        );
    }

    #[test]
    fn spare_exhaustion_joins_master_only() {
        use crate::ui_queue::{UiEvent, UiQueue};
        let ui = UiQueue::new(64);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![("b".into(), 0)], 24000), 1, vec![]);
        engine.start_recording(Recorder::new(4, 4), vec![], vec![]); // no spares
        engine_step(&mut engine, 24000);
        engine.submit_swap(
            tl(
                vec![ev(0, VoiceKind::Hat, 1, 2400)],
                1,
                vec![("h".into(), 1)],
                24000,
            ),
            2,
            vec![],
        );
        engine_step(&mut engine, 24000);
        let mut claims = 0;
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::TrackClaimed { .. } = ev {
                claims += 1;
            }
        }
        assert_eq!(claims, 0, "no spares -> no claims");
    }

    #[test]
    fn swap_refills_spares_beyond_old_cap() {
        use crate::ui_queue::{UiEvent, UiQueue};
        let ui = UiQueue::new(128);
        let mut engine = Engine::new(120.0, 48000);
        engine.set_ui(Some(ui.clone()));
        engine.submit_swap(tl(vec![], 0, vec![], 24000), 1, vec![]);
        engine_step(&mut engine, 24000);
        engine.start_recording(Recorder::new(4, 4), vec![], vec![]);
        // swap adds 20 new loops, carrying 20 spares
        let first: Vec<(String, u64)> = (0..20).map(|i| (format!("l{i}"), 1)).collect();
        let spares: Vec<_> = (0..20).map(|_| Recorder::new(4, 4)).collect();
        engine.submit_swap(tl(vec![], 1, first.clone(), 24000), 2, spares);
        engine_step(&mut engine, 24000);
        // next swap adds 5 more loops, carrying 5 spares
        let mut second = first;
        second.extend((20..25).map(|i| (format!("l{i}"), 1)));
        let spares: Vec<_> = (0..5).map(|_| Recorder::new(4, 4)).collect();
        engine.submit_swap(tl(vec![], 2, second, 24000), 3, spares);
        engine_step(&mut engine, 24000);
        let mut claimed: Vec<u32> = Vec::new();
        while let Some(ev) = ui.try_pop() {
            if let UiEvent::TrackClaimed { loop_index, .. } = ev {
                claimed.push(loop_index);
            }
        }
        claimed.sort_unstable();
        assert_eq!(
            claimed,
            (0..25).collect::<Vec<u32>>(),
            "every new loop claims a track, beyond the old 8-spare cap"
        );
    }

    #[test]
    fn retired_timeline_is_returned_not_dropped() {
        let mut engine = Engine::new(120.0, 48000);
        let a = tl(vec![], 0, vec![], 24000);
        let b = tl(vec![], 1, vec![], 24000);
        engine.submit_swap(a.clone(), 1, vec![]);
        engine_step(&mut engine, 24000);
        engine.submit_swap(b.clone(), 2, vec![]);
        engine_step(&mut engine, 24000);
        assert_eq!(
            Arc::strong_count(&a),
            2,
            "the old timeline must survive the swap in the retirement slot"
        );
        let retired = engine.take_retired();
        assert_eq!(retired.len(), 1, "exactly one timeline retired");
        assert!(
            Arc::ptr_eq(&retired[0], &a),
            "the retired Arc is the previous timeline, not a copy"
        );
        assert_eq!(
            engine.timeline.as_ref().map(|t| t.generation),
            Some(1),
            "the second timeline stays active"
        );
    }

    #[test]
    fn retired_overflow_stays_in_the_engine() {
        let mut engine = Engine::new(120.0, 48000);
        let mut out = vec![0.0f32; 24000 * 2];
        let timelines: Vec<_> = (0..6).map(|g| tl(vec![], g, vec![], 24000)).collect();
        for (i, t) in timelines.iter().enumerate() {
            engine.submit_swap(t.clone(), i as u64 + 1, vec![]);
            engine.process(&mut out);
        }
        let q = AudioQueue::new(4);
        assert_eq!(q.retired_available(), 4, "empty queue has every slot free");
        let n = engine.take_retired_into(q.retired_available(), |tl| {
            let r = q.push_retired(tl);
            assert!(r.is_ok(), "the drain stays within the free slots");
            r
        });
        assert_eq!(n, 4, "only the free queue slots are drained");
        assert_eq!(
            q.take_retired()
                .iter()
                .map(|t| t.generation)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
            "the queue fills in retirement order"
        );
        assert_eq!(
            engine
                .take_retired()
                .iter()
                .map(|t| t.generation)
                .collect::<Vec<_>>(),
            vec![4],
            "the overflow stays in the engine slot, in order"
        );
    }

    #[test]
    fn retired_bridge_never_drops_on_a_full_queue() {
        let mut engine = Engine::new(120.0, 48000);
        let mut out = vec![0.0f32; 24000 * 2];
        let timelines: Vec<_> = (0..6).map(|g| tl(vec![], g, vec![], 24000)).collect();
        for (i, t) in timelines.iter().enumerate() {
            engine.submit_swap(t.clone(), i as u64 + 1, vec![]);
            engine.process(&mut out);
        }
        let q = AudioQueue::new(4);
        for g in 0..4 {
            assert!(
                q.push_retired(tl(vec![], g, vec![], 24000)).is_ok(),
                "prefill the queue up to capacity"
            );
        }
        assert_eq!(q.retired_available(), 0, "the queue is full");
        let mut failures = 0;
        let pushed =
            engine.take_retired_into(q.retired_available(), |tl| match q.push_retired(tl) {
                Ok(()) => Ok(()),
                Err(tl) => {
                    failures += 1;
                    Err(tl)
                }
            });
        assert_eq!(pushed, 0, "a full queue must drain nothing");
        assert_eq!(
            failures, 0,
            "the bridge must not attempt an impossible push"
        );
        let drained = q.take_retired();
        assert_eq!(drained.len(), 4, "the consumer drains the queue");
        let pushed = engine.take_retired_into(q.retired_available(), |tl| q.push_retired(tl));
        assert_eq!(
            pushed, 4,
            "exactly the free slots are drained and pushed back"
        );
        assert_eq!(q.take_retired().len(), 4, "every pushed timeline made it");
        assert_eq!(
            engine.take_retired().len(),
            1,
            "the remainder stays in the engine for the next drain"
        );
    }

    #[test]
    fn failed_retired_push_returns_to_the_engine() {
        let mut engine = Engine::new(120.0, 48000);
        let mut out = vec![0.0f32; 24000 * 2];
        let timelines: Vec<_> = (0..3).map(|g| tl(vec![], g, vec![], 24000)).collect();
        for (i, t) in timelines.iter().enumerate() {
            engine.submit_swap(t.clone(), i as u64 + 1, vec![]);
            engine.process(&mut out);
        }
        let pushed = engine.take_retired_into(usize::MAX, Err);
        assert_eq!(pushed, 0, "every push failed");
        let retired = engine.take_retired();
        assert_eq!(
            retired.iter().map(|t| t.generation).collect::<Vec<_>>(),
            vec![0, 1],
            "failed pushes return to the engine slot in order, never dropped"
        );
    }
}
