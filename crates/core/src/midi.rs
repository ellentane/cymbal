use crate::ast::VoiceKind;
use crate::scheduler::Timeline;

#[derive(Debug, Clone)]
pub struct MidiEvent {
    pub sample_offset: u64,
    pub bytes: [u8; 3],
}

pub fn encode_note_on(channel: u8, note: u8, velocity: u8) -> [u8; 3] {
    [0x90 | (channel & 0x0F), note, velocity.clamp(1, 127)]
}

pub fn encode_note_off(channel: u8, note: u8) -> [u8; 3] {
    [0x80 | (channel & 0x0F), note, 0]
}

fn drum_note(kind: VoiceKind) -> Option<u8> {
    match kind {
        VoiceKind::Kick => Some(35),
        VoiceKind::Snare => Some(38),
        VoiceKind::Hat => Some(42),
        _ => None,
    }
}

pub fn build_timeline_midi(tl: &Timeline) -> Vec<MidiEvent> {
    let mut out = Vec::new();
    for ev in &tl.events {
        let (channel, note) = match ev.voice {
            VoiceKind::Kick | VoiceKind::Snare | VoiceKind::Hat => (9, drum_note(ev.voice)),
            VoiceKind::Bass | VoiceKind::Lead => (0, ev.pitch),
            VoiceKind::Sample => (0, None),
        };
        let Some(note) = note else { continue };
        let velocity = (ev.velocity * 127.0).round().clamp(1.0, 127.0) as u8;
        out.push(MidiEvent {
            sample_offset: ev.sample_offset,
            bytes: encode_note_on(channel, note, velocity),
        });
        out.push(MidiEvent {
            sample_offset: ev.sample_offset + ev.duration,
            bytes: encode_note_off(channel, note),
        });
    }
    out.sort_by_key(|e| e.sample_offset);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_bytes() {
        assert_eq!(encode_note_on(9, 35, 100), [0x99, 35, 100]);
        assert_eq!(encode_note_off(0, 60), [0x80, 60, 0]);
    }

    #[test]
    fn channel_masked_to_four_bits() {
        assert_eq!(encode_note_on(16, 60, 1)[0], 0x90);
        assert_eq!(encode_note_off(16, 60)[0], 0x80);
    }

    #[test]
    fn velocity_clamped() {
        assert_eq!(encode_note_on(0, 60, 200)[2], 127);
        assert_eq!(
            encode_note_on(0, 60, 0)[2],
            1,
            "note-on vel 0 must not be silent"
        );
    }

    fn tl_for(events: Vec<(VoiceKind, Option<u8>, f32, u64, u64)>) -> Timeline {
        // (kind, pitch, velocity, offset, duration)
        Timeline {
            events: events
                .into_iter()
                .map(
                    |(voice, pitch, velocity, sample_offset, duration)| crate::scheduler::Event {
                        sample_offset,
                        loop_name: "b".into(),
                        voice,
                        pitch,
                        semitone: 0,
                        velocity,
                        duration,
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
                    },
                )
                .collect(),
            generation: 0,
            tempo: 120.0,
            bar_samples: 96000,
            sample_rate: 48000,
            loops: vec!["b".into()],
            loop_generations: vec![("b".into(), 0)],
            midi: vec![],
        }
    }

    #[test]
    fn drums_map_to_percussion_channel() {
        let tl = tl_for(vec![
            (VoiceKind::Kick, None, 1.0, 0, 14400),
            (VoiceKind::Snare, None, 0.5, 24000, 7200),
            (VoiceKind::Hat, None, 1.0, 48000, 2400),
        ]);
        let midi = build_timeline_midi(&tl);
        assert_eq!(midi[0].bytes, [0x99, 35, 127]);
        assert_eq!(midi[1].bytes, [0x89, 35, 0]);
        assert_eq!(midi[1].sample_offset, 14400, "note off at offset+duration");
        assert_eq!(midi[2].bytes, [0x99, 38, 64]);
        assert_eq!(midi[4].bytes, [0x99, 42, 127]);
    }

    #[test]
    fn pitched_voices_use_pitch_on_channel_1() {
        let tl = tl_for(vec![(VoiceKind::Bass, Some(36), 1.0, 0, 14400)]);
        let midi = build_timeline_midi(&tl);
        assert_eq!(midi[0].bytes, [0x90, 36, 127]);
        assert_eq!(midi[1].bytes, [0x80, 36, 0]);
    }

    #[test]
    fn events_sorted_by_offset() {
        let tl = tl_for(vec![
            (VoiceKind::Kick, None, 1.0, 48000, 14400),
            (VoiceKind::Bass, Some(36), 1.0, 0, 14400),
        ]);
        let midi = build_timeline_midi(&tl);
        assert!(
            midi.windows(2)
                .all(|w| w[0].sample_offset <= w[1].sample_offset)
        );
    }
}
