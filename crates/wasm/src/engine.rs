//! Worklet engine: full native DSP — `Master` (delay/reverb sends) and
//! per-voice `bass`/`treble`/`comp`. Sample voices land in D3; sample_id is
//! carried on the wire but not applied yet.

use cymbal_core::ast::VoiceKind;
use cymbal_core::dsp::{Voice, VoiceParams};
use cymbal_core::mixer::{Master, VoiceOutput};
use cymbal_core::scheduler::Timeline;

/// Fixed 64-byte record (LE): offset u64, voice u8, pitch i16 (-1 = none),
/// semitone i16, velocity f32, duration u64, pan f32, delay f32, reverb f32,
/// bass f32, treble f32, comp f32, sample_id u16, start f32 (0..1), end f32
/// (0..1), cycle u8, pad 4.
pub fn serialize(tl: &Timeline) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + tl.events.len() * 64);
    out.extend_from_slice(&tl.bar_samples.to_le_bytes());
    out.extend_from_slice(&(tl.events.len() as u64).to_le_bytes());
    for ev in &tl.events {
        out.extend_from_slice(&ev.sample_offset.to_le_bytes());
        out.push(ev.voice as u8);
        out.extend_from_slice(&(ev.pitch.map(|p| p as i16).unwrap_or(-1)).to_le_bytes());
        out.extend_from_slice(&(ev.semitone as i16).to_le_bytes());
        out.extend_from_slice(&ev.velocity.to_le_bytes());
        out.extend_from_slice(&ev.duration.to_le_bytes());
        out.extend_from_slice(&ev.pan.to_le_bytes());
        out.extend_from_slice(&ev.delay_send.to_le_bytes());
        out.extend_from_slice(&ev.reverb_send.to_le_bytes());
        out.extend_from_slice(&ev.bass.to_le_bytes());
        out.extend_from_slice(&ev.treble.to_le_bytes());
        out.extend_from_slice(&ev.comp.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(ev.sample_start as f32).to_le_bytes());
        out.extend_from_slice(&(ev.sample_end as f32).to_le_bytes());
        out.push(ev.sample_loop as u8);
        out.extend_from_slice(&[0u8; 4]);
    }
    out
}

#[derive(Debug)]
pub struct WireEvent {
    pub sample_offset: u64,
    pub voice: u8,
    pub pitch: Option<u8>,
    pub semitone: i16,
    pub velocity: f32,
    pub duration: u64,
    pub pan: f32,
    pub delay_send: f32,
    pub reverb_send: f32,
    pub bass: f32,
    pub treble: f32,
    pub comp: f32,
    pub sample_id: u16,
    pub start: f32,
    pub end: f32,
    pub cycle: u8,
}

pub fn deserialize_events(bytes: &[u8]) -> Result<Vec<WireEvent>, &'static str> {
    if bytes.len() < 16 || !(bytes.len() - 16).is_multiple_of(64) {
        return Err("wire data: payload not a multiple of the 64-byte record stride");
    }
    let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    let Some(records_len) = count.checked_mul(64) else {
        return Err("wire data: record count overflow");
    };
    if records_len != bytes.len() - 16 {
        return Err("wire data: record count does not match payload length");
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let rec = &bytes[16 + i * 64..16 + (i + 1) * 64];
        let pitch = {
            let p = i16::from_le_bytes(rec[9..11].try_into().unwrap());
            if p >= 0 { Some(p as u8) } else { None }
        };
        let velocity = f32::from_le_bytes(rec[13..17].try_into().unwrap());
        let pan = f32::from_le_bytes(rec[25..29].try_into().unwrap());
        out.push(WireEvent {
            sample_offset: u64::from_le_bytes(rec[0..8].try_into().unwrap()),
            voice: rec[8],
            pitch,
            semitone: i16::from_le_bytes(rec[11..13].try_into().unwrap()),
            velocity: if velocity.is_finite() { velocity } else { 0.0 },
            duration: u64::from_le_bytes(rec[17..25].try_into().unwrap()),
            pan: if pan.is_finite() { pan } else { 0.0 },
            delay_send: f32::from_le_bytes(rec[29..33].try_into().unwrap()),
            reverb_send: f32::from_le_bytes(rec[33..37].try_into().unwrap()),
            bass: f32::from_le_bytes(rec[37..41].try_into().unwrap()),
            treble: f32::from_le_bytes(rec[41..45].try_into().unwrap()),
            comp: f32::from_le_bytes(rec[45..49].try_into().unwrap()),
            sample_id: u16::from_le_bytes(rec[49..51].try_into().unwrap()),
            start: f32::from_le_bytes(rec[51..55].try_into().unwrap()),
            end: f32::from_le_bytes(rec[55..59].try_into().unwrap()),
            cycle: rec[59],
        });
    }
    Ok(out)
}

struct Ev {
    kind: VoiceKind,
    pitch: Option<u8>,
    semitone: i32,
    velocity: f32,
    duration: u64,
    pan: f32,
    delay_send: f32,
    reverb_send: f32,
    bass: f32,
    treble: f32,
    comp: f32,
}

pub struct Eng {
    position: u64,
    bar_samples: u64,
    next_bar: u64,
    future: Vec<(u64, Ev)>,
    active: Vec<(u64, Ev, Voice)>,
    sample_rate: u32,
    master: Master,
}

#[unsafe(no_mangle)]
pub extern "C" fn eng_alloc(bar_samples: u64, sample_rate: u32) -> *mut Eng {
    Box::into_raw(Box::new(Eng {
        position: 0,
        bar_samples: bar_samples.max(1),
        next_bar: 0,
        future: Vec::new(),
        active: Vec::new(),
        sample_rate,
        master: Master::new(sample_rate, bar_samples.max(1)),
    }))
}

/// # Safety
/// `e` must be a pointer from `eng_alloc` (or null). Single-call only:
/// a second call on the same pointer is UB and cannot be detected safely —
/// callers must not re-enter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eng_free(e: *mut Eng) {
    if !e.is_null() {
        drop(unsafe { Box::from_raw(e) });
    }
}

/// # Safety
/// `e` must be a valid pointer from `eng_alloc`; `data` must point to `len`
/// readable bytes (a serialized timeline, see `serialize`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eng_submit(e: *mut Eng, data: *const u8, len: usize) {
    if e.is_null() || data.is_null() || len < 16 {
        return;
    }
    let eng = unsafe { &mut *e };
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    let bar_samples = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    if bar_samples == 0 {
        return;
    }
    let Ok(events) = deserialize_events(bytes) else {
        return;
    };
    eng.bar_samples = bar_samples;
    eng.master.set_bar_samples(bar_samples);
    eng.future.clear();
    eng.active.clear();
    for w in events {
        let kind = match w.voice {
            0 => VoiceKind::Kick,
            1 => VoiceKind::Snare,
            2 => VoiceKind::Hat,
            3 => VoiceKind::Bass,
            4 => VoiceKind::Lead,
            _ => continue,
        };
        eng.future.push((
            w.sample_offset,
            Ev {
                kind,
                pitch: w.pitch,
                semitone: w.semitone as i32,
                velocity: w.velocity,
                duration: w.duration,
                pan: w.pan,
                delay_send: w.delay_send,
                reverb_send: w.reverb_send,
                bass: w.bass,
                treble: w.treble,
                comp: w.comp,
            },
        ));
    }
    eng.future.sort_by_key(|(o, _)| *o);
    eng.position = 0;
    eng.next_bar = 0;
}

/// # Safety
/// `e` must be a valid pointer from `eng_alloc`; `out` must point to
/// `frames * 2` writable floats (interleaved stereo).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eng_process(e: *mut Eng, out: *mut f32, frames: u32) {
    let eng = unsafe { &mut *e };
    if eng.bar_samples == 0 {
        return;
    }
    let frames = frames.min(128);
    let out = unsafe { std::slice::from_raw_parts_mut(out, frames as usize * 2) };
    for frame in 0..frames as usize {
        let now = eng.position + frame as u64;
        while now >= eng.next_bar {
            eng.next_bar += eng.bar_samples;
        }
        while let Some((at, _)) = eng.future.first() {
            if *at <= now {
                let (_, ev) = eng.future.remove(0);
                let voice = Voice::new(
                    ev.kind,
                    VoiceParams {
                        semitone: ev.semitone,
                        bass: ev.bass,
                        treble: ev.treble,
                        comp: ev.comp,
                        ..VoiceParams::default_for(ev.kind, ev.pitch)
                    },
                    eng.sample_rate,
                );
                eng.active.push((now + ev.duration, ev, voice));
            } else {
                break;
            }
        }
        eng.active.retain(|(until, _, _)| *until > now);
        eng.master.begin_frame();
        for (_, ev, voice) in &mut eng.active {
            if let Some(s) = voice.next_sample(eng.sample_rate) {
                eng.master.add_voice(VoiceOutput {
                    sample: s,
                    velocity: ev.velocity,
                    pan: ev.pan,
                    delay_send: ev.delay_send,
                    reverb_send: ev.reverb_send,
                });
            }
        }
        let mut frame_out = [0.0f32; 2];
        eng.master.end_frame(&mut frame_out);
        out[frame * 2] = frame_out[0];
        out[frame * 2 + 1] = frame_out[1];
    }
    eng.position += frames as u64;
}

#[unsafe(no_mangle)]
pub extern "C" fn eng_out_ptr() -> *mut f32 {
    static mut OUT: *mut f32 = std::ptr::null_mut();
    unsafe {
        if OUT.is_null() {
            OUT = Box::into_raw(vec![0.0f32; 128 * 2].into_boxed_slice()) as *mut f32;
        }
        OUT
    }
}

/// Single worklet thread: JS copies the buffer before the next call.
static mut IN_BUF: *mut Vec<u8> = std::ptr::null_mut();

#[unsafe(no_mangle)]
pub extern "C" fn eng_in_ptr(len: usize) -> *mut u8 {
    unsafe {
        if IN_BUF.is_null() {
            IN_BUF = Box::into_raw(Box::new(Vec::with_capacity(1024)));
        }
        let v = &mut *IN_BUF;
        if v.len() < len {
            v.resize(len, 0);
        }
        v.as_mut_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cymbal_core::scheduler::Event;

    fn timeline_with(ev: Event) -> Timeline {
        Timeline {
            events: vec![ev],
            generation: 0,
            tempo: 120.0,
            bar_samples: 48000,
            sample_rate: 48000,
            loops: Vec::new(),
            loop_generations: Vec::new(),
            midi: Vec::new(),
            window_start: 0,
            window_len: u64::MAX,
        }
    }

    #[test]
    fn master_bus_matches_native_mix() {
        use cymbal_core::lexer::lex;
        use cymbal_core::parser::parse;
        use cymbal_core::render::render_offline;
        use cymbal_core::scheduler::schedule;
        use std::collections::HashMap;

        let src = "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x\" delay=0.8 reverb=0.6 bass=0.4 treble=0.3 comp=0.5 pan=0.2\n";
        let tl = schedule(
            &parse(&lex(src).unwrap()).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            48000,
            48000,
        )
        .unwrap();
        let native = render_offline(src, 48000, 48000, &HashMap::new()).unwrap();
        let wire = serialize(&tl);

        unsafe {
            let e = eng_alloc(tl.bar_samples, 48000);
            eng_submit(e, wire.as_ptr(), wire.len());
            let mut out = vec![0.0f32; 128 * 2];
            let mut worklet = Vec::with_capacity(native.len());
            for _ in 0..48000 / 128 {
                eng_process(e, out.as_mut_ptr(), 128);
                worklet.extend_from_slice(&out);
            }
            eng_free(e);
            assert_eq!(
                worklet, native,
                "worklet master bus must match the native mix"
            );
        }
    }

    #[test]
    fn wire_v2_round_trips_fx_and_sample_fields() {
        let ev = Event {
            sample_offset: 123456,
            loop_name: String::new(),
            loop_index: 0,
            voice: VoiceKind::Bass,
            pitch: Some(60),
            semitone: 2,
            velocity: 0.8,
            duration: 14400,
            generation: 0,
            pan: 0.1,
            delay_send: 0.3,
            reverb_send: 0.4,
            bass: 0.5,
            treble: 0.6,
            comp: 0.7,
            sample: None,
            sample_start: 0.25,
            sample_end: 0.75,
            sample_loop: true,
        };
        let bytes = serialize(&timeline_with(ev));
        let events = deserialize_events(&bytes).expect("well-formed wire data");
        assert_eq!(events.len(), 1);
        let w = &events[0];
        assert_eq!(w.sample_offset, 123456);
        assert_eq!(w.voice, VoiceKind::Bass as u8);
        assert_eq!(w.pitch, Some(60));
        assert_eq!(w.semitone, 2);
        assert_eq!(w.velocity, 0.8_f32);
        assert_eq!(w.duration, 14400);
        assert_eq!(w.pan, 0.1_f32);
        assert_eq!(w.delay_send, 0.3_f32);
        assert_eq!(w.reverb_send, 0.4_f32);
        assert_eq!(w.bass, 0.5_f32);
        assert_eq!(w.treble, 0.6_f32);
        assert_eq!(w.comp, 0.7_f32);
        assert_eq!(w.sample_id, 0, "registry ids land in D3; 0 for now");
        assert_eq!(w.start, 0.25_f32);
        assert_eq!(w.end, 0.75_f32);
        assert_eq!(w.cycle, 1);
        assert!(
            deserialize_events(&bytes[..bytes.len() - 1]).is_err(),
            "wrong-size records must be rejected"
        );
    }
}
