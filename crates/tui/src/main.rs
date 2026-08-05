mod editor;
mod highlight;
mod samples;
mod status;

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

use cymbal_audio::ring::{AudioQueue, Msg};
use cymbal_core::error::Error;
use cymbal_core::lexer::lex;
use cymbal_core::parser::parse;
use cymbal_core::scheduler::Timeline;

use editor::Editor;
use highlight::highlight_line;
use status::Status;

const MAX_SAMPLES: u64 = 3600 * 48000;
const SAMPLE_RATE: u32 = 48000;
const RENDER_DEFAULT_SECONDS: u64 = 120;

enum UiMsg {
    Err(Error),
    Info(String),
    RecordError(String),
    Reloaded {
        generations: HashMap<String, u64>,
        loops: Vec<String>,
        seq: u64,
    },
}

fn next_loop_generations(
    current: &HashMap<String, u64>,
    loop_names: &[String],
) -> HashMap<String, u64> {
    let max = current.values().copied().max().unwrap_or(0);
    loop_names
        .iter()
        .map(|name| {
            let g = current.get(name).copied().unwrap_or(max + 1);
            (name.clone(), g)
        })
        .collect()
}

fn apply_ui_msg(status: &mut Status, msg: UiMsg) {
    match msg {
        UiMsg::Err(e) => status.set_error(e.to_string()),
        UiMsg::Info(s) => {
            status.clear_error();
            status.message = s;
        }
        _ => {}
    }
}

fn handle_reloaded(
    status: &mut Status,
    latest: &mut HashMap<String, u64>,
    latest_seq: &mut u64,
    generations: HashMap<String, u64>,
    loops: Vec<String>,
    seq: u64,
) {
    if seq <= *latest_seq {
        return;
    }
    *latest_seq = seq;
    let changed: Vec<String> = loops
        .iter()
        .filter(|n| latest.get(*n) != generations.get(*n))
        .cloned()
        .collect();
    *latest = generations;
    status.loops = loops;
    status.clear_error();
    status.message = if changed.is_empty() {
        "reloaded: nothing changed".into()
    } else {
        format!("reloaded: {}", changed.join(", "))
    };
}

fn loop_names(src: &str) -> Result<Vec<String>, Error> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    Ok(program
        .statements
        .iter()
        .filter_map(|s| match s {
            cymbal_core::ast::Stmt::Loop(l) => Some(l.name.clone()),
            _ => None,
        })
        .collect())
}

pub fn build_timeline_with(
    src: &str,
    sample_rate: u32,
    base_dir: &std::path::Path,
    loop_generations: &HashMap<String, u64>,
) -> Result<Timeline, Error> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    let samples = samples::load_samples(&program, base_dir)?;
    cymbal_core::scheduler::schedule(
        &program,
        loop_generations,
        &samples,
        MAX_SAMPLES,
        sample_rate,
    )
}

fn render_src(src: &str, base_dir: &std::path::Path, max_samples: u64) -> Result<Vec<f32>, Error> {
    let program = parse(&lex(src)?)?;
    let samples = samples::load_samples(&program, base_dir)?;
    cymbal_core::render::render_offline(src, max_samples, SAMPLE_RATE, &samples)
}

fn render_to_wav_with(
    input: &std::path::Path,
    output: &std::path::Path,
    seconds: u64,
    format: cymbal_core::wav::WavFormat,
) -> Result<(), String> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    let max_samples = seconds.saturating_mul(SAMPLE_RATE as u64).min(MAX_SAMPLES);
    let base = input.parent().unwrap_or_else(|| std::path::Path::new("."));
    let samples_out =
        render_src(&src, base, max_samples).map_err(|e| format!("render failed: {e}"))?;
    let mut w = cymbal_core::wav::WavWriter::create_with_format(output, SAMPLE_RATE, 2, format)
        .map_err(|e| format!("cannot write {}: {e}", output.display()))?;
    w.write_interleaved(&samples_out)
        .map_err(|e| format!("cannot write {}: {e}", output.display()))?;
    w.finalize()
        .map_err(|e| format!("cannot finalize {}: {e}", output.display()))
}

fn render_to_wav(
    input: &std::path::Path,
    output: &std::path::Path,
    seconds: u64,
) -> Result<(), String> {
    render_to_wav_with(input, output, seconds, cymbal_core::wav::WavFormat::Pcm16)
}

fn render_to_wav_f32(
    input: &std::path::Path,
    output: &std::path::Path,
    seconds: u64,
) -> Result<(), String> {
    render_to_wav_with(input, output, seconds, cymbal_core::wav::WavFormat::F32)
}

fn render_tracks_to_dir(
    input: &std::path::Path,
    out_dir: &std::path::Path,
    seconds: u64,
) -> Result<(), String> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    let max_samples = seconds.saturating_mul(SAMPLE_RATE as u64).min(MAX_SAMPLES);
    let base = input.parent().unwrap_or_else(|| std::path::Path::new("."));
    let program = parse(&lex(&src).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let samples = samples::load_samples(&program, base).map_err(|e| e.to_string())?;
    let tracks =
        cymbal_core::render::render_offline_tracks(&src, max_samples, SAMPLE_RATE, &samples)
            .map_err(|e| format!("render failed: {e}"))?;
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    for (name, samples) in tracks {
        let path = free_path(out_dir, &cymbal_core::wav::sanitize_name(&name));
        cymbal_core::wav::write_wav(&path, &samples, SAMPLE_RATE)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn apply_tempo_override(src: &str, tempo: f64) -> String {
    let mut lines: Vec<String> = src.lines().map(|s| s.to_string()).collect();
    let new_line = format!("tempo {tempo}");
    if let Some(line) = lines.iter_mut().find(|l| l.starts_with("tempo ")) {
        *line = new_line;
    } else {
        lines.insert(0, new_line);
    }
    lines.join("\n") + "\n"
}

fn free_path(dir: &std::path::Path, stem: &str) -> std::path::PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem}.wav")
        } else {
            format!("{stem}-{n}.wav")
        };
        let path = dir.join(name);
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

fn recording_path(dir: &std::path::Path, ts: &str) -> std::path::PathBuf {
    free_path(dir, &format!("recording-{ts}"))
}

fn record_loop(
    rec: &Arc<cymbal_audio::recorder::Recorder>,
    path: &std::path::Path,
    tx: &mpsc::Sender<UiMsg>,
    start: Instant,
) {
    let mut w = match cymbal_core::wav::WavWriter::create(path, SAMPLE_RATE, 2) {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.send(UiMsg::RecordError(format!(
                "cannot create {}: {e}",
                path.display()
            )));
            return;
        }
    };
    loop {
        if let Some(block) = rec.take_filled() {
            if let Err(e) = w.write_interleaved(&block) {
                let _ = tx.send(UiMsg::RecordError(format!("write failed: {e}")));
                return;
            }
            rec.return_block(block);
        } else if rec.is_stopped() {
            break;
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let _ = w.finalize();
    let secs = start.elapsed().as_secs_f32();
    let _ = tx.send(UiMsg::Info(format!(
        "recorded {} ({secs:.1}s)",
        path.display()
    )));
}

fn spawn_reload(
    src: String,
    base: std::path::PathBuf,
    latest: HashMap<String, u64>,
    seq: u64,
    msg_tx: mpsc::Sender<UiMsg>,
    queue: Arc<AudioQueue>,
) {
    std::thread::spawn(move || match loop_names(&src) {
        Ok(names) => {
            let gens = next_loop_generations(&latest, &names);
            match build_timeline_with(&src, SAMPLE_RATE, &base, &gens) {
                Ok(tl) => {
                    let _ = msg_tx.send(UiMsg::Reloaded {
                        generations: gens,
                        loops: names,
                        seq,
                    });
                    let _ = queue.send(Msg::Swap(Arc::new(tl), seq));
                }
                Err(e) => {
                    let _ = msg_tx.send(UiMsg::Err(e));
                }
            }
        }
        Err(e) => {
            let _ = msg_tx.send(UiMsg::Err(e));
        }
    });
}

fn alt_transform_kind(code: KeyCode) -> Option<cymbal_core::transform::TransformKind> {
    use cymbal_core::transform::TransformKind;
    match code {
        KeyCode::Char('r') => Some(TransformKind::Reverse),
        KeyCode::Char('h') => Some(TransformKind::HalfSpeed),
        KeyCode::Char('[') => Some(TransformKind::RotateLeft),
        KeyCode::Char(']') => Some(TransformKind::RotateRight),
        _ => None,
    }
}

type ParsedArgs<'a> = (
    Option<String>,
    Option<(&'a str, &'a str, Option<&'a str>, bool)>,
    Option<(&'a str, &'a str)>,
    Option<&'a str>,
);

fn parse_args(args: &[String]) -> ParsedArgs<'_> {
    // returns (midi_port, render args (input, output, seconds), tracks args (input, outdir), tui file)
    let mut it = args.iter().map(String::as_str);
    let first = it.next();
    match first {
        Some("--midi") => {
            let rest: Vec<&str> = it.collect();
            // "" means "first available port". An explicit port must be
            // followed by "render" or a file; a lone argument is the file.
            // (A port literally named "render" is misparsed — documented.)
            let (port, rest) = match rest.first() {
                Some(&"render") | None => (Some(String::new()), rest),
                Some(_) if rest.len() == 1 => (Some(String::new()), rest),
                Some(&p) => (Some(p.to_string()), rest[1..].to_vec()),
            };
            match rest.as_slice() {
                ["render", input, output] => (port, Some((input, output, None, false)), None, None),
                ["render", "--tracks", input, outdir] => (port, None, Some((input, outdir)), None),
                ["render", "--f32", input, output] => {
                    (port, Some((input, output, None, true)), None, None)
                }
                ["render", "--f32", input, output, seconds] => {
                    (port, Some((input, output, Some(seconds), true)), None, None)
                }
                [file] => (port, None, None, Some(file)),
                _ => (port, None, None, None),
            }
        }
        Some("render") => {
            let rest: Vec<&str> = it.collect();
            match rest.as_slice() {
                [input, output] => (None, Some((input, output, None, false)), None, None),
                ["--tracks", input, outdir] => (None, None, Some((input, outdir)), None),
                ["--f32", input, output] => (None, Some((input, output, None, true)), None, None),
                ["--f32", input, output, seconds] => {
                    (None, Some((input, output, Some(seconds), true)), None, None)
                }
                [input, output, seconds] => (
                    None,
                    Some((input, output, Some(seconds), false)),
                    None,
                    None,
                ),
                _ => (None, None, None, None),
            }
        }
        Some(file) => (None, None, None, Some(file)),
        None => (None, None, None, None),
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (midi_port, render, tracks, tui_file) = parse_args(&args);
    if let Some((input, output, seconds, f32)) = render {
        let seconds = match seconds {
            None => RENDER_DEFAULT_SECONDS,
            Some(s) => match s.parse::<u64>() {
                Ok(s) if s > 0 => s,
                _ => {
                    eprintln!("invalid seconds: {s}");
                    return std::process::ExitCode::FAILURE;
                }
            },
        };
        let result = if f32 {
            render_to_wav_f32(
                std::path::Path::new(input),
                std::path::Path::new(output),
                seconds,
            )
        } else {
            render_to_wav(
                std::path::Path::new(input),
                std::path::Path::new(output),
                seconds,
            )
        };
        return match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("{msg}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some((input, outdir)) = tracks {
        return match render_tracks_to_dir(
            std::path::Path::new(input),
            std::path::Path::new(outdir),
            RENDER_DEFAULT_SECONDS,
        ) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("{msg}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(file) = tui_file {
        return match run_tui(std::path::Path::new(file), midi_port) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("{msg}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    eprintln!(
        "usage: cymbal [--midi [port]] <file.cym>\n       cymbal render <in.cym> <out.wav> [seconds]\n       cymbal render --f32 <in.cym> <out.wav> [seconds]\n       cymbal render --tracks <in.cym> <outdir>"
    );
    std::process::ExitCode::from(2)
}

fn run_tui(file: &std::path::Path, midi_port: Option<String>) -> Result<(), String> {
    let src = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;

    let queue = Arc::new(AudioQueue::new(16));
    let base = file.parent().unwrap_or_else(|| std::path::Path::new("."));
    let initial =
        build_timeline_with(&src, SAMPLE_RATE, base, &HashMap::new()).map_err(|e| e.to_string())?;
    let mut latest_loops: HashMap<String, u64> = initial.loop_generations.iter().cloned().collect();
    let mut status = Status::new();
    status.loops = initial.loops.clone();
    let midi_out = match &midi_port {
        Some(port) => {
            let name = port.as_str();
            if cymbal_audio::midi_out::MidiOut::port_available(name) {
                let out = cymbal_audio::midi_out::MidiOut::new(8192);
                out.clone().spawn_writer(name);
                status.midi_port = midi_port.clone();
                Some(out)
            } else {
                status.set_error(if name.is_empty() {
                    "midi unavailable: no MIDI port available".into()
                } else {
                    format!("midi unavailable: no port named '{port}'")
                });
                None
            }
        }
        None => None,
    };
    let handle = match cymbal_audio::stream::start_audio(
        queue.clone(),
        Arc::new(initial),
        |_e| {},
        midi_out,
    ) {
        Ok(h) => Some(h),
        Err(e) => {
            status.set_error(e.into_error().to_string());
            None
        }
    };
    status.latency_ms = handle.as_ref().and_then(|h| h.latency_ms);
    status.device_rate = handle.as_ref().map(|h| h.device_rate);

    let (msg_tx, msg_rx) = mpsc::channel::<UiMsg>();

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let mut editor = Editor::new(src);

    let mut recording = false;
    let mut record_start: Option<Instant> = None;
    let mut record_writers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut reload_seq: u64 = 1;
    let mut latest_seq: u64 = 0;

    let result = (|| -> io::Result<()> {
        loop {
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(k) = event::read()?
            {
                if k.modifiers.contains(KeyModifiers::CONTROL) {
                    match k.code {
                        KeyCode::Char('s') => {
                            let src = editor.content();
                            let base = file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let latest = latest_loops.clone();
                            reload_seq += 1;
                            let seq = reload_seq;
                            spawn_reload(src, base, latest, seq, msg_tx.clone(), queue.clone());
                            status.clear_error();
                            status.message = "reloading...".into();
                        }
                        KeyCode::Char('q') => {
                            if recording {
                                recording = false;
                                status.recording = false;
                                if queue.send(Msg::RecordStop).is_err() {
                                    status.set_error("recording queue full: stop failed".into());
                                }
                                let deadline = Instant::now() + Duration::from_secs(2);
                                for w in record_writers.drain(..) {
                                    while !w.is_finished() && Instant::now() < deadline {
                                        std::thread::sleep(Duration::from_millis(10));
                                    }
                                }
                            }
                            break;
                        }
                        KeyCode::Char('r') => {
                            if !recording {
                                recording = true;
                                record_start = Some(Instant::now());
                                let ts = cymbal_core::timefmt::format_timestamp(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0),
                                );
                                let dir =
                                    file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                                let master_path = recording_path(&dir, &ts);
                                let master = cymbal_audio::recorder::Recorder::new(32, 4096);
                                let mut tracks = Vec::new();
                                for name in &status.loops {
                                    tracks.push((
                                        name.clone(),
                                        cymbal_audio::recorder::Recorder::new(32, 4096),
                                    ));
                                }
                                let master_for_queue = master.clone();
                                if queue
                                    .send(Msg::RecordStart {
                                        master: master_for_queue,
                                        tracks: tracks
                                            .iter()
                                            .map(|(n, r)| (n.clone(), r.clone()))
                                            .collect(),
                                    })
                                    .is_err()
                                {
                                    master.stop();
                                    for (_, r) in &tracks {
                                        r.stop();
                                    }
                                    recording = false;
                                    record_start = None;
                                    status.set_error(
                                        "recording queue full: recording aborted".into(),
                                    );
                                } else {
                                    let tx = msg_tx.clone();
                                    let start = Instant::now();
                                    let mut writers = Vec::new();
                                    let m2 = master.clone();
                                    let p2 = master_path.clone();
                                    writers.push(std::thread::spawn(move || {
                                        record_loop(&m2, &p2, &tx, start)
                                    }));
                                    for (name, rec) in &tracks {
                                        let rec = rec.clone();
                                        let path = free_path(
                                            &dir,
                                            &format!(
                                                "recording-{ts}-{}",
                                                cymbal_core::wav::sanitize_name(name)
                                            ),
                                        );
                                        let tx = msg_tx.clone();
                                        let start = Instant::now();
                                        writers.push(std::thread::spawn(move || {
                                            record_loop(&rec, &path, &tx, start)
                                        }));
                                    }
                                    record_writers = writers;
                                    status.recording = true;
                                    status.clear_error();
                                    status.message = "recording...".into();
                                }
                            } else {
                                recording = false;
                                if queue.send(Msg::RecordStop).is_err() {
                                    status.set_error("recording queue full: stop failed".into());
                                }
                                status.recording = false;
                                status.message = "stopping...".into();
                            }
                        }
                        KeyCode::Char('e') => {
                            let src = editor.content();
                            let out_path = file
                                .parent()
                                .map(|p| p.join("out.wav"))
                                .unwrap_or_else(|| std::path::PathBuf::from("out.wav"));
                            let base = file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let tx = msg_tx.clone();
                            std::thread::spawn(move || {
                                match render_src(&src, &base, MAX_SAMPLES) {
                                    Ok(samples) => {
                                        match cymbal_core::wav::write_wav(
                                            &out_path,
                                            &samples,
                                            SAMPLE_RATE,
                                        ) {
                                            Ok(()) => {
                                                let _ = tx.send(UiMsg::Info(format!(
                                                    "exported {}",
                                                    out_path.display()
                                                )));
                                            }
                                            Err(e) => {
                                                let _ = tx.send(UiMsg::Err(Error::new(
                                                    cymbal_core::error::Span { line: 0, col: 0 },
                                                    cymbal_core::error::ErrorKind::Io,
                                                    format!("export failed: {e}"),
                                                )));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(UiMsg::Err(e));
                                    }
                                }
                            });
                        }
                        KeyCode::Char('=') => {
                            status.raise_tempo();
                            let src = apply_tempo_override(&editor.content(), status.tempo);
                            let base = file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let latest = latest_loops
                                .iter()
                                .map(|(k, v)| (k.clone(), v + 1))
                                .collect();
                            reload_seq += 1;
                            let seq = reload_seq;
                            spawn_reload(src, base, latest, seq, msg_tx.clone(), queue.clone());
                        }
                        KeyCode::Char('-') => {
                            status.lower_tempo();
                            let src = apply_tempo_override(&editor.content(), status.tempo);
                            let base = file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let latest = latest_loops
                                .iter()
                                .map(|(k, v)| (k.clone(), v + 1))
                                .collect();
                            reload_seq += 1;
                            let seq = reload_seq;
                            spawn_reload(src, base, latest, seq, msg_tx.clone(), queue.clone());
                        }
                        _ => {}
                    }
                } else if k.modifiers.contains(KeyModifiers::ALT) {
                    if let Some(kind) = alt_transform_kind(k.code) {
                        let src = editor.content();
                        match cymbal_core::transform::transform_src(&src, editor.cursor().1, kind) {
                            Ok(new_src) => {
                                editor.set_content(new_src);
                                let base =
                                    file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                                let latest = latest_loops.clone();
                                reload_seq += 1;
                                let seq = reload_seq;
                                spawn_reload(
                                    editor.content(),
                                    base,
                                    latest,
                                    seq,
                                    msg_tx.clone(),
                                    queue.clone(),
                                );
                                status.clear_error();
                                status.message = "transforming...".into();
                            }
                            Err(e) => status.set_error(e.to_string()),
                        }
                    }
                } else {
                    match k.code {
                        KeyCode::Char(c) => editor.insert_char(c),
                        KeyCode::Backspace => editor.backspace(),
                        KeyCode::Enter => editor.newline(),
                        KeyCode::Left => editor.move_left(),
                        KeyCode::Right => editor.move_right(),
                        KeyCode::Up => editor.move_up(),
                        KeyCode::Down => editor.move_down(),
                        KeyCode::Home => editor.move_home(),
                        KeyCode::End => editor.move_end(),
                        _ => {}
                    }
                }
            }
            if let Ok(msg) = msg_rx.try_recv() {
                match msg {
                    UiMsg::Reloaded {
                        generations,
                        loops,
                        seq,
                    } => handle_reloaded(
                        &mut status,
                        &mut latest_loops,
                        &mut latest_seq,
                        generations,
                        loops,
                        seq,
                    ),
                    UiMsg::RecordError(s) => {
                        recording = false;
                        record_start = None;
                        record_writers = Vec::new();
                        if queue.send(Msg::RecordStop).is_err() {
                            status.set_error(format!("{s} (recording queue full: stop failed)"));
                        } else {
                            status.set_error(s);
                        }
                        status.recording = false;
                    }
                    m => apply_ui_msg(&mut status, m),
                }
            }
            if recording && let Some(start) = record_start {
                status.record_elapsed_secs = start.elapsed().as_secs();
            }
            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([Constraint::Percentage(90), Constraint::Percentage(10)])
                    .split(f.area());
                let mut styled_lines: Vec<ratatui::text::Line> = Vec::new();
                for (i, line) in editor.lines().iter().enumerate() {
                    let spans: Vec<ratatui::text::Span> = if i == editor.cursor().1 {
                        highlight_line(line)
                            .into_iter()
                            .map(|(s, c)| {
                                let color = match c {
                                    highlight::Class::Keyword => Color::Cyan,
                                    highlight::Class::Pattern => Color::Yellow,
                                    highlight::Class::Note => Color::Magenta,
                                    highlight::Class::Number => Color::Green,
                                    highlight::Class::Comment => Color::DarkGray,
                                    highlight::Class::Plain => Color::White,
                                };
                                ratatui::text::Span::styled(s, Style::default().fg(color))
                            })
                            .collect()
                    } else {
                        line.split(' ')
                            .map(|w| {
                                ratatui::text::Span::styled(
                                    w.to_string(),
                                    Style::default().fg(Color::White),
                                )
                            })
                            .collect()
                    };
                    styled_lines.push(ratatui::text::Line::from(spans));
                }
                f.render_widget(
                    Paragraph::new(styled_lines)
                        .block(Block::default().borders(Borders::ALL).title("cymbal")),
                    chunks[0],
                );
                f.render_widget(
                    Paragraph::new(status.render()).block(Block::default().borders(Borders::ALL)),
                    chunks[1],
                );
            })?;
        }
        Ok(())
    })();

    disable_raw_mode().map_err(|e| e.to_string())?;
    std::io::stdout()
        .execute(LeaveAlternateScreen)
        .map_err(|e| e.to_string())?;
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cymbal_core::ast::VoiceKind;

    #[test]
    fn build_timeline_from_source() {
        let src = "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x . . x\"\n";
        let tl =
            build_timeline_with(src, 48000, std::path::Path::new("."), &HashMap::new()).unwrap();
        assert!(!tl.events.is_empty());
        assert!(tl.events.iter().all(|e| e.voice == VoiceKind::Kick));
    }

    #[test]
    fn invalid_source_reports_error() {
        let err = build_timeline_with(
            "loop \"b\":\n    kick << \"x y\"\n",
            48000,
            std::path::Path::new("."),
            &HashMap::new(),
        )
        .unwrap_err();
        assert!(err.message.contains("pattern"));
    }

    #[test]
    fn render_seconds_sets_wav_length() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("cymbal_test_src_{}.cym", std::process::id()));
        let out_path = dir.join(format!("cymbal_test_out_{}.wav", std::process::id()));
        {
            let mut f = std::fs::File::create(&src_path).unwrap();
            write!(
                f,
                "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x\"\n"
            )
            .unwrap();
        }
        render_to_wav(&src_path, &out_path, 1).unwrap();
        assert_eq!(std::fs::metadata(&out_path).unwrap().len(), 48000 * 4 + 44);
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn render_f32_writes_float_wav() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("cymbal_f32_src_{}.cym", std::process::id()));
        let out_path = dir.join(format!("cymbal_f32_out_{}.wav", std::process::id()));
        {
            let mut f = std::fs::File::create(&src_path).unwrap();
            write!(f, "let kick = kick()\nloop \"b\":\n    kick << \"x\"\n").unwrap();
        }
        render_to_wav_f32(&src_path, &out_path, 1).unwrap();
        let bytes = std::fs::read(&out_path).unwrap();
        assert_eq!(&bytes[20..22], &3u16.to_le_bytes(), "fmt tag 3");
        assert_eq!(bytes.len(), 48000 * 2 * 4 + 44, "1s stereo f32 wav length");
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn render_tracks_writes_master_and_loop_files() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("cymbal_tracks_src_{}.cym", std::process::id()));
        let out_dir = dir.join(format!("cymbal_tracks_out_{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&src_path).unwrap();
            write!(
                f,
                "let kick = kick()\nlet hat = hat()\nloop \"b\":\n    kick << \"x\"\nloop \"h\":\n    hat << \"x\"\n"
            )
            .unwrap();
        }
        render_tracks_to_dir(&src_path, &out_dir, 1).unwrap();
        assert!(out_dir.join("master.wav").exists());
        assert!(out_dir.join("b.wav").exists());
        assert!(out_dir.join("h.wav").exists());
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn render_tracks_dedupes_colliding_stems_and_keeps_master() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("cymbal_tracks2_src_{}.cym", std::process::id()));
        let out_dir = dir.join(format!("cymbal_tracks2_out_{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&src_path).unwrap();
            write!(
                f,
                "let kick = kick()\nloop \"master\":\n    kick << \"x\"\nloop \"a b\":\n    kick << \"x\"\nloop \"a-b\":\n    kick << \"x\"\n"
            )
            .unwrap();
        }
        render_tracks_to_dir(&src_path, &out_dir, 1).unwrap();
        assert!(out_dir.join("master.wav").exists(), "mix keeps master.wav");
        assert!(
            out_dir.join("master-2.wav").exists(),
            "loop named master must not overwrite the mix"
        );
        assert!(out_dir.join("a-b.wav").exists());
        assert!(out_dir.join("a-b-2.wav").exists());
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn render_src_loads_samples_from_base_dir() {
        let dir = std::env::temp_dir();
        let sample_path = dir.join(format!("cymbal_rs_smp_{}.wav", std::process::id()));
        std::fs::write(
            &sample_path,
            cymbal_core::wav::encode_wav(&[0.5, -0.5, 0.25], 48000, 1),
        )
        .unwrap();
        let src = format!(
            "loop \"b\":\n    sample \"{}\" << \"x\"\n",
            sample_path.file_name().unwrap().to_str().unwrap()
        );
        let samples = render_src(&src, &dir, 48000).unwrap();
        assert!(!samples.is_empty());
        let _ = std::fs::remove_file(&sample_path);
    }

    #[test]
    fn tempo_override_inserts_and_replaces() {
        let src = "let kick = kick()\nloop \"b\":\n    kick << \"x\"\n";
        assert_eq!(
            apply_tempo_override(src, 130.0),
            "tempo 130\nlet kick = kick()\nloop \"b\":\n    kick << \"x\"\n"
        );
        let src2 = "tempo 90\nlet kick = kick()\n";
        assert_eq!(
            apply_tempo_override(src2, 130.0),
            "tempo 130\nlet kick = kick()\n"
        );
    }

    #[test]
    fn apply_ui_msg_info_clears_error() {
        let mut status = Status::new();
        status.set_error("boom".into());
        apply_ui_msg(&mut status, UiMsg::Info("exported out.wav".into()));
        assert_eq!(status.error, None);
        assert_eq!(status.message, "exported out.wav");
    }

    #[test]
    fn build_timeline_with_loop_generations() {
        let mut gens = HashMap::new();
        gens.insert("b".to_string(), 5u64);
        let tl = build_timeline_with(
            "let kick = kick()\nloop \"b\":\n    kick << \"x .\"\n",
            48000,
            std::path::Path::new("."),
            &gens,
        )
        .unwrap();
        assert_eq!(tl.generation, 5);
    }

    #[test]
    fn handle_reloaded_sets_ordered_loops_and_noop_message() {
        let mut status = Status::new();
        let mut latest = HashMap::new();
        let mut latest_seq = 0;
        let mut gens = HashMap::new();
        gens.insert("b".to_string(), 1u64);
        latest.insert("b".to_string(), 1u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            gens.clone(),
            vec!["b".to_string()],
            1,
        );
        assert_eq!(status.loops, vec!["b".to_string()]);
        assert_eq!(status.message, "reloaded: nothing changed");
        assert_eq!(latest, gens);
        assert_eq!(latest_seq, 1);
    }

    #[test]
    fn handle_reloaded_reports_changed_loops() {
        let mut status = Status::new();
        let mut latest = HashMap::new();
        latest.insert("a".to_string(), 1u64);
        latest.insert("b".to_string(), 1u64);
        let mut latest_seq = 0;
        let mut gens = HashMap::new();
        gens.insert("a".to_string(), 1u64);
        gens.insert("b".to_string(), 2u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            gens,
            vec!["a".to_string(), "b".to_string()],
            1,
        );
        assert_eq!(status.message, "reloaded: b");
    }

    #[test]
    fn handle_reloaded_includes_new_loops() {
        let mut status = Status::new();
        let mut latest = HashMap::new();
        latest.insert("b".to_string(), 2u64);
        let mut latest_seq = 0;
        let mut gens = HashMap::new();
        gens.insert("b".to_string(), 2u64);
        gens.insert("k".to_string(), 3u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            gens,
            vec!["b".to_string(), "k".to_string()],
            1,
        );
        assert_eq!(status.message, "reloaded: k");
        assert_eq!(status.loops, vec!["b".to_string(), "k".to_string()]);
    }

    #[test]
    fn handle_reloaded_ignores_stale_seq() {
        let mut status = Status::new();
        status.loops = vec!["a".to_string()];
        status.message = "reloaded: a".into();
        let mut latest = HashMap::new();
        latest.insert("a".to_string(), 5u64);
        let mut latest_seq = 3;
        let mut gens = HashMap::new();
        gens.insert("z".to_string(), 9u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            gens,
            vec!["z".to_string()],
            3,
        );
        assert_eq!(status.loops, vec!["a".to_string()]);
        assert_eq!(status.message, "reloaded: a");
        assert_eq!(latest.get("z"), None);
        assert_eq!(latest_seq, 3);
    }

    #[test]
    fn reload_diff_bumps_only_new_loops() {
        let mut old = HashMap::new();
        old.insert("b".to_string(), 2u64);
        old.insert("h".to_string(), 2u64);
        let new = next_loop_generations(&old, &["b".to_string(), "h".to_string(), "k".to_string()]);
        assert_eq!(new.get("b"), Some(&2), "unchanged loop keeps generation");
        assert_eq!(new.get("h"), Some(&2));
        assert_eq!(new.get("k"), Some(&3), "new loop gets max+1");
    }

    #[test]
    fn transform_alt_keys_map_to_kinds() {
        use cymbal_core::transform::TransformKind;
        let table = [
            (KeyCode::Char('r'), TransformKind::Reverse),
            (KeyCode::Char('h'), TransformKind::HalfSpeed),
            (KeyCode::Char('['), TransformKind::RotateLeft),
            (KeyCode::Char(']'), TransformKind::RotateRight),
        ];
        for (code, expected) in table {
            assert_eq!(alt_transform_kind(code), Some(expected));
        }
        assert_eq!(alt_transform_kind(KeyCode::Char('q')), None);
    }

    #[test]
    fn record_loop_reports_create_failure() {
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        let dir = std::env::temp_dir().join(format!("cymbal_nodir_{}", std::process::id()));
        let path = dir.join("recording.wav");
        let (tx, rx) = mpsc::channel::<UiMsg>();
        let rec2 = rec.clone();
        let path2 = path.clone();
        std::thread::spawn(move || record_loop(&rec2, &path2, &tx, Instant::now()))
            .join()
            .unwrap();
        let msg = rx.recv().unwrap();
        let UiMsg::RecordError(s) = msg else {
            panic!("expected RecordError");
        };
        assert!(s.contains("cannot create"), "{s}");
    }

    #[test]
    fn record_loop_writes_and_finalizes() {
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        let mut block = rec.take_pool_block();
        block[..4].copy_from_slice(&[0.5, 0.5, -0.5, -0.5]);
        rec.push_filled(block);
        rec.stop();
        let path = std::env::temp_dir().join(format!("cymbal_rec_{}.wav", std::process::id()));
        let (tx, _rx) = mpsc::channel::<UiMsg>();
        let rec2 = rec.clone();
        let path2 = path.clone();
        std::thread::spawn(move || record_loop(&rec2, &path2, &tx, Instant::now()))
            .join()
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let data = cymbal_core::wav::decode_wav(&bytes).unwrap();
        assert_eq!(data.frames.as_slice(), &[0.5, -0.5, 0.0, 0.0]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recording_path_is_timestamped_with_collision_suffix() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let base = dir.join(format!("cymbal_ts_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let ts = "20260804-153245".to_string();
        let p1 = recording_path(&base, &ts);
        assert_eq!(
            p1.file_name().unwrap().to_str().unwrap(),
            "recording-20260804-153245.wav"
        );
        std::fs::File::create(&p1).unwrap().write_all(b"x").unwrap();
        let p2 = recording_path(&base, &ts);
        assert_eq!(
            p2.file_name().unwrap().to_str().unwrap(),
            "recording-20260804-153245-2.wav"
        );
        std::fs::File::create(&p2).unwrap().write_all(b"x").unwrap();
        let p3 = recording_path(&base, &ts);
        assert_eq!(
            p3.file_name().unwrap().to_str().unwrap(),
            "recording-20260804-153245-3.wav"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sanitize_name_replaces_unsafe_chars() {
        assert_eq!(cymbal_core::wav::sanitize_name("beat 1"), "beat-1");
        assert_eq!(cymbal_core::wav::sanitize_name("a/b\\c:d"), "a-b-c-d");
        assert_eq!(cymbal_core::wav::sanitize_name("plain"), "plain");
    }

    #[test]
    fn colliding_sanitized_names_get_distinct_paths() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("cymbal_collide_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stem = |name: &str| {
            format!(
                "recording-20260804-153245-{}",
                cymbal_core::wav::sanitize_name(name)
            )
        };
        let p1 = free_path(&dir, &stem("beat 1"));
        std::fs::File::create(&p1).unwrap().write_all(b"x").unwrap();
        let p2 = free_path(&dir, &stem("beat-1"));
        assert_ne!(p1, p2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_args_single_file() {
        let args = vec!["a.cym".to_string()];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, None);
        assert_eq!(tracks, None);
        assert_eq!(file, Some("a.cym"));
    }

    #[test]
    fn parse_args_midi_file_uses_first_port() {
        let args = vec!["--midi".to_string(), "a.cym".to_string()];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, Some(String::new()));
        assert_eq!(render, None);
        assert_eq!(tracks, None);
        assert_eq!(file, Some("a.cym"));
    }

    #[test]
    fn parse_args_midi_named_port_file() {
        let args = vec![
            "--midi".to_string(),
            "UM-1".to_string(),
            "a.cym".to_string(),
        ];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, Some("UM-1".to_string()));
        assert_eq!(render, None);
        assert_eq!(tracks, None);
        assert_eq!(file, Some("a.cym"));
    }

    #[test]
    fn parse_args_midi_render_uses_first_port() {
        let args = vec![
            "--midi".to_string(),
            "render".to_string(),
            "in.cym".to_string(),
            "out.wav".to_string(),
        ];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, Some(String::new()));
        assert_eq!(render, Some(("in.cym", "out.wav", None, false)));
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_render() {
        let args = vec![
            "render".to_string(),
            "in.cym".to_string(),
            "out.wav".to_string(),
        ];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, Some(("in.cym", "out.wav", None, false)));
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_render_f32() {
        let args = vec![
            "render".to_string(),
            "--f32".to_string(),
            "in.cym".to_string(),
            "out.wav".to_string(),
        ];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, Some(("in.cym", "out.wav", None, true)));
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_render_f32_with_seconds() {
        let args = vec![
            "render".to_string(),
            "--f32".to_string(),
            "in.cym".to_string(),
            "out.wav".to_string(),
            "5".to_string(),
        ];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, Some(("in.cym", "out.wav", Some("5"), true)));
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_render_with_seconds() {
        let args = vec![
            "render".to_string(),
            "in.cym".to_string(),
            "out.wav".to_string(),
            "5".to_string(),
        ];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, Some(("in.cym", "out.wav", Some("5"), false)));
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_render_tracks() {
        let args = vec![
            "render".to_string(),
            "--tracks".to_string(),
            "in.cym".to_string(),
            "outdir".to_string(),
        ];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, None);
        assert_eq!(tracks, Some(("in.cym", "outdir")));
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_garbage_is_all_none() {
        let args = vec!["render".to_string(), "in.cym".to_string()];
        let (midi, render, tracks, file) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, None);
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }
}
