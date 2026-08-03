use cymbal_core::render::render_offline;
use cymbal_core::wav::encode_wav;

fn decode_pcm16(wav: &[u8]) -> Vec<i16> {
    assert_eq!(&wav[0..4], b"RIFF", "bad header");
    let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
    assert_eq!(data_len, wav.len() - 44, "bad data chunk length");
    let mut out = Vec::with_capacity(data_len / 2);
    for chunk in wav[44..].chunks_exact(2) {
        out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    out
}

fn assert_audio_matches(rendered: &[f32], golden: &[u8]) {
    let rendered = encode_wav(rendered, 48000);
    let rendered = decode_pcm16(&rendered);
    let golden = decode_pcm16(golden);
    assert_eq!(rendered.len(), golden.len(), "sample count mismatch");
    let bad: Vec<_> = rendered
        .iter()
        .zip(&golden)
        .enumerate()
        .filter(|(_, (a, b))| (**a as i32 - **b as i32).abs() > 1)
        .collect();
    assert!(
        bad.is_empty(),
        "{} samples differ by more than 1 lsb (first: {:?})",
        bad.len(),
        bad.first().map(|(i, (a, b))| (i, a, b))
    );
}

#[test]
fn beat_cym_matches_golden() {
    let src = include_str!("../../../examples/beat.cym");
    let samples = render_offline(src, 384000, 48000).unwrap();
    assert_audio_matches(&samples, include_bytes!("data/beat.golden.wav"));
}

#[test]
fn beat_cym_full_matches_golden() {
    let src = include_str!("../../../examples/beat.cym");
    let samples = render_offline(src, 768000, 48000).unwrap();
    assert_audio_matches(&samples, include_bytes!("data/beat_full.golden.wav"));
}
