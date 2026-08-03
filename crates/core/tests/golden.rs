use cymbal_core::render::render_offline;
use cymbal_core::wav::encode_wav;

#[test]
fn beat_cym_renders_byte_identical_to_golden() {
    let src = include_str!("../../../examples/beat.cym");
    let samples = render_offline(src, 384000, 48000).unwrap();
    let wav = encode_wav(&samples, 48000);
    let golden = include_bytes!("data/beat.golden.wav");
    assert_eq!(wav.as_slice(), golden);
}

#[test]
fn beat_cym_full_renders_byte_identical_to_golden() {
    let src = include_str!("../../../examples/beat.cym");
    let samples = render_offline(src, 768000, 48000).unwrap();
    let wav = encode_wav(&samples, 48000);
    let golden = include_bytes!("data/beat_full.golden.wav");
    assert_eq!(wav.as_slice(), golden);
}
