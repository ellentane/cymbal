use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use cymbal_core::ast::Program;
use cymbal_core::error::{Error, ErrorKind, Span};
use cymbal_core::scheduler::{SampleData, sample_paths};

pub fn load_samples(
    program: &Program,
    base_dir: &Path,
) -> Result<HashMap<String, Arc<SampleData>>, Error> {
    let mut out = HashMap::new();
    for path in sample_paths(program) {
        let full = base_dir.join(&path);
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(read_err) => match cymbal_core::builtin_samples::builtin(&path) {
                Some(b) => b.to_vec(),
                None => {
                    return Err(Error::new(
                        Span { line: 0, col: 0 },
                        ErrorKind::Io,
                        format!("cannot read {}: {}", full.display(), read_err),
                    ));
                }
            },
        };
        let data = cymbal_core::wav::decode_wav(&bytes)
            .map_err(|e| Error::new(e.span, e.kind, format!("{}: {e}", full.display())))?;
        out.insert(path, Arc::new(data));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cymbal_core::lexer::lex;
    use cymbal_core::parser::parse;
    use cymbal_core::wav::encode_wav;

    #[test]
    fn loads_samples_relative_to_base_dir() {
        let dir = std::env::temp_dir();
        let wav_path = dir.join(format!("cymbal_smp_{}.wav", std::process::id()));
        std::fs::write(&wav_path, encode_wav(&[0.0, 1.0, -1.0], 48000, 1)).unwrap();
        let src = format!(
            "loop \"b\":\n    sample \"{}\" << \"x\"\n",
            wav_path.file_name().unwrap().to_str().unwrap()
        );
        let program = parse(&lex(&src).unwrap()).unwrap();
        let samples = load_samples(&program, &dir).unwrap();
        assert_eq!(samples.len(), 1);
        let data = samples.values().next().unwrap();
        assert_eq!(data.frames.as_slice(), &[0.0, 32767.0 / 32768.0, -1.0]);
        let _ = std::fs::remove_file(&wav_path);
    }

    #[test]
    fn builtin_sample_resolves_without_local_file() {
        let dir = std::env::temp_dir().join(format!("cymbal_builtin_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = "loop \"b\":\n    sample \"kick\" << \"x\"\n";
        let program = parse(&lex(src).unwrap()).unwrap();
        let samples = load_samples(&program, &dir).unwrap();
        assert_eq!(samples.len(), 1);
        let data = samples.get("kick").unwrap();
        assert!(data.frames.len() > 100, "builtin kick must decode");
        assert!(
            data.frames.iter().any(|s| s.abs() > 0.001),
            "builtin kick must not be silent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_sample_reports_error() {
        let program =
            parse(&lex("loop \"b\":\n    sample \"nope.wav\" << \"x\"\n").unwrap()).unwrap();
        let err = load_samples(&program, std::path::Path::new("/nonexistent")).unwrap_err();
        assert!(err.message.contains("nope.wav"));
    }

    #[test]
    fn corrupted_sample_names_file_in_error() {
        let dir = std::env::temp_dir();
        let wav_path = dir.join(format!("cymbal_bad_{}.wav", std::process::id()));
        std::fs::write(&wav_path, b"this is not a wav").unwrap();
        let src = format!(
            "loop \"b\":\n    sample \"{}\" << \"x\"\n",
            wav_path.file_name().unwrap().to_str().unwrap()
        );
        let program = parse(&lex(&src).unwrap()).unwrap();
        let err = load_samples(&program, &dir).unwrap_err();
        assert!(err.message.contains("RIFF"));
        assert!(
            err.message
                .contains(&wav_path.file_name().unwrap().to_str().unwrap().to_string())
        );
        let _ = std::fs::remove_file(&wav_path);
    }
}
