const FILES: &[(&str, &[u8])] = &[
    (
        "kick.wav",
        include_bytes!("../../../assets/samples/kick.wav"),
    ),
    (
        "snare.wav",
        include_bytes!("../../../assets/samples/snare.wav"),
    ),
    ("hat.wav", include_bytes!("../../../assets/samples/hat.wav")),
    (
        "clap.wav",
        include_bytes!("../../../assets/samples/clap.wav"),
    ),
    (
        "loop.wav",
        include_bytes!("../../../assets/samples/loop.wav"),
    ),
];

pub fn builtin(name: &str) -> Option<&'static [u8]> {
    let stem = name.strip_suffix(".wav").unwrap_or(name);
    let key = format!("{stem}.wav");
    FILES.iter().find(|(n, _)| *n == key).map(|(_, b)| *b)
}

pub fn builtin_names() -> &'static [&'static str] {
    &["kick.wav", "snare.wav", "hat.wav", "clap.wav", "loop.wav"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_lookup_by_name_and_stem() {
        assert!(builtin("kick").is_some());
        assert!(builtin("kick.wav").is_some());
        assert_eq!(builtin("kick"), builtin("kick.wav"));
        assert!(builtin("nope").is_none());
        assert!(builtin("nope.wav").is_none());
    }

    #[test]
    fn builtins_decode_as_wav() {
        for name in builtin_names() {
            let data = crate::wav::decode_wav(builtin(name).unwrap()).unwrap();
            assert!(data.frames.len() > 100, "{name} must have audio");
            assert!(
                data.frames.iter().any(|s| s.abs() > 0.001),
                "{name} must not be silent"
            );
        }
    }
}
