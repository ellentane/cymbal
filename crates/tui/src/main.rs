use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["render", input, output] => match render_to_wav(Path::new(input), Path::new(output)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cymbal render <in.cym> <out.wav>");
            ExitCode::from(2)
        }
    }
}

fn render_to_wav(input: &Path, output: &Path) -> Result<(), String> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    let samples = cymbal_core::render::render_offline(&src, 384000, 48000)
        .map_err(|e| format!("render failed: {e}"))?;
    cymbal_core::wav::write_wav(output, &samples, 48000)
        .map_err(|e| format!("cannot write {}: {e}", output.display()))
}
