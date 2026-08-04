//! The worklet engine intentionally skips `Master` (no delay/reverb in wasm v1)
//! and skips FX sends: `l.tanh()`/`r.tanh()` only.

//! `semitone` is applied; `bass`/`treble`/`comp`/sample params are ignored
//! (not serialized).

use cymbal_core::ast::VoiceKind;
use cymbal_core::dsp::{Voice, VoiceParams};
use cymbal_core::scheduler::Timeline;

/// Fixed 32-byte record: offset u64, voice u8, pitch i16 (-1 = none),
/// semitone i16, velocity f32, duration u64, pan f32, pad 3.
/// Delay/reverb sends are not serialized (the worklet engine has no sends).
pub fn serialize(tl: &Timeline) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + tl.events.len() * 32);
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
        out.extend_from_slice(&[0u8; 3]);
    }
    out
}

pub fn deserialize_events(bytes: &[u8]) -> Vec<(u64, u8)> {
    let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let rec = &bytes[16 + i * 32..16 + (i + 1) * 32];
        out.push((u64::from_le_bytes(rec[0..8].try_into().unwrap()), rec[8]));
    }
    out
}

struct Ev {
    kind: VoiceKind,
    pitch: Option<u8>,
    semitone: i32,
    velocity: f32,
    duration: u64,
    pan: f32,
}

pub struct Eng {
    position: u64,
    bar_samples: u64,
    next_bar: u64,
    future: Vec<(u64, Ev)>,
    active: Vec<(u64, Ev, Voice)>,
    sample_rate: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn eng_alloc(bar_samples: u64, sample_rate: u32) -> *mut Eng {
    Box::into_raw(Box::new(Eng {
        position: 0,
        bar_samples,
        next_bar: 0,
        future: Vec::new(),
        active: Vec::new(),
        sample_rate,
    }))
}

/// # Safety
/// `e` must be a pointer from `eng_alloc` (or null).
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
    eng.bar_samples = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    if 16 + count * 32 > len {
        return;
    }
    eng.future.clear();
    eng.active.clear();
    for i in 0..count {
        let rec = &bytes[16 + i * 32..16 + (i + 1) * 32];
        let offset = u64::from_le_bytes(rec[0..8].try_into().unwrap());
        let kind = match rec[8] {
            0 => VoiceKind::Kick,
            1 => VoiceKind::Snare,
            2 => VoiceKind::Hat,
            3 => VoiceKind::Bass,
            4 => VoiceKind::Lead,
            _ => continue,
        };
        let pitch = {
            let p = i16::from_le_bytes(rec[9..11].try_into().unwrap());
            if p >= 0 { Some(p as u8) } else { None }
        };
        let semitone = i16::from_le_bytes(rec[11..13].try_into().unwrap()) as i32;
        let velocity = f32::from_le_bytes(rec[13..17].try_into().unwrap());
        let duration = u64::from_le_bytes(rec[17..25].try_into().unwrap());
        let pan = f32::from_le_bytes(rec[25..29].try_into().unwrap());
        eng.future.push((
            offset,
            Ev {
                kind,
                pitch,
                semitone,
                velocity,
                duration,
                pan,
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
        let mut l = 0.0f32;
        let mut r = 0.0f32;
        for (_, ev, voice) in &mut eng.active {
            if let Some(s) = voice.next_sample(eng.sample_rate) {
                let angle = (ev.pan + 1.0) * std::f32::consts::PI / 4.0;
                l += s * ev.velocity * angle.cos();
                r += s * ev.velocity * angle.sin();
            }
        }
        out[frame * 2] = l.tanh();
        out[frame * 2 + 1] = r.tanh();
    }
    eng.position += frames as u64;
}
