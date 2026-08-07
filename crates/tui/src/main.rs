mod editor;
mod help_panel;
mod highlight;
mod samples;
mod segment;
mod status;

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
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
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use cymbal_audio::ring::{AudioQueue, Msg};
use cymbal_core::error::Error;
use cymbal_core::lexer::lex;
use cymbal_core::parser::parse;
use cymbal_core::scheduler::Timeline;

use editor::Editor;
use highlight::highlight_line;
use segment::{SegmentAction, SegmentScheduler};
use status::Status;

const MAX_SAMPLES: u64 = 3600 * 48000;
const SAMPLE_RATE: u32 = 48000;
const RENDER_DEFAULT_SECONDS: u64 = 120;
const WINDOW_LEN: u64 = cymbal_core::render::STREAM_WINDOW_LEN;

enum UiMsg {
    Err(Error, Option<u64>),
    Info(String),
    RecordError(String),
    Help(String),
    Reloaded {
        generations: HashMap<String, u64>,
        loops: Vec<String>,
        seq: u64,
        window_end: u64,
        src: String,
    },
    SegmentDone {
        window_end: Option<u64>,
        seq: u64,
        loops: Vec<String>,
    },
}

type PendingClaims = HashMap<u64, Vec<(u32, String, Arc<cymbal_audio::recorder::Recorder>)>>;

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
        UiMsg::Err(e, _) => status.set_error(e.to_string()),
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
    recent: &mut Vec<(u64, Vec<String>)>,
    generations: HashMap<String, u64>,
    loops: Vec<String>,
    seq: u64,
) {
    record_reload(recent, seq, loops.clone());
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

fn is_stale(seq: u64, latest_seq: u64) -> bool {
    seq < latest_seq
}

fn record_reload(recent: &mut Vec<(u64, Vec<String>)>, seq: u64, loops: Vec<String>) {
    recent.push((seq, loops));
    if recent.len() > 64 {
        recent.remove(0);
    }
}

fn resolve_claim(recent: &[(u64, Vec<String>)], seq: u64, loop_index: u32) -> Option<String> {
    recent
        .iter()
        .rev()
        .find(|(s, _)| *s == seq)
        .and_then(|(_, loops)| loops.get(loop_index as usize))
        .cloned()
}

fn drain_pending(
    pending: &mut PendingClaims,
    seq: u64,
) -> Vec<(u32, String, Arc<cymbal_audio::recorder::Recorder>)> {
    pending.remove(&seq).unwrap_or_default()
}

enum ClaimAction {
    Spawn(String),
    Pend,
    Stale,
    Ignore,
}

fn handle_claim(
    rec: &Arc<cymbal_audio::recorder::Recorder>,
    seq: u64,
    loop_index: u32,
    recent: &[(u64, Vec<String>)],
    pending: &mut PendingClaims,
    latest_seq: u64,
    recording: bool,
) -> ClaimAction {
    if !recording {
        return ClaimAction::Ignore;
    }
    if is_stale(seq, latest_seq) {
        return ClaimAction::Stale;
    }
    if let Some(name) = resolve_claim(recent, seq, loop_index) {
        return ClaimAction::Spawn(name);
    }
    pending
        .entry(seq)
        .or_default()
        .push((loop_index, String::new(), rec.clone()));
    ClaimAction::Pend
}

enum FlushAction {
    Spawn(String),
    Lost,
    Ignore,
}

fn flush_claim(
    seq: u64,
    loop_index: u32,
    recent: &[(u64, Vec<String>)],
    recording: bool,
) -> FlushAction {
    if !recording {
        return FlushAction::Ignore;
    }
    match resolve_claim(recent, seq, loop_index) {
        Some(name) => FlushAction::Spawn(name),
        None => FlushAction::Lost,
    }
}

fn flush_pending_claims(
    seq: u64,
    pending: &mut PendingClaims,
    recent: &[(u64, Vec<String>)],
    record_ts: Option<&str>,
    record_dir: Option<&std::path::Path>,
    msg_tx: &mpsc::Sender<UiMsg>,
    record_writers: &mut Vec<std::thread::JoinHandle<()>>,
) {
    for (loop_index, _, rec) in drain_pending(pending, seq) {
        match flush_claim(seq, loop_index, recent, record_ts.is_some()) {
            FlushAction::Spawn(name) => {
                record_writers.push(spawn_track_writer(
                    &rec, &name, record_ts, record_dir, msg_tx,
                ));
            }
            FlushAction::Lost => {
                let _ = msg_tx.send(UiMsg::Err(
                    Error::new(
                        cymbal_core::error::Span { line: 0, col: 0 },
                        cymbal_core::error::ErrorKind::Io,
                        format!(
                            "lost mid-recording track claim (seq {seq}): loop missing from reload"
                        ),
                    ),
                    None,
                ));
            }
            FlushAction::Ignore => {}
        }
    }
}

fn join_writers(writers: &mut Vec<std::thread::JoinHandle<()>>, deadline: Instant) {
    for w in writers.drain(..) {
        while !w.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn spawn_track_writer(
    rec: &Arc<cymbal_audio::recorder::Recorder>,
    name: &str,
    ts: Option<&str>,
    dir: Option<&std::path::Path>,
    tx: &mpsc::Sender<UiMsg>,
) -> std::thread::JoinHandle<()> {
    let rec = rec.clone();
    let ts = ts.expect("claim writer requires a recording timestamp");
    let dir = dir.expect("claim writer requires a record directory");
    let path = free_path(
        dir,
        &format!("recording-{ts}-{}", cymbal_core::wav::sanitize_name(name)),
    );
    let tx = tx.clone();
    let start = Instant::now();
    std::thread::spawn(move || record_loop(&rec, &path, &tx, start))
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
    cymbal_core::scheduler::schedule_window(
        &program,
        loop_generations,
        &samples,
        0,
        WINDOW_LEN,
        sample_rate,
    )
}

fn render_src_into(
    src: &str,
    base_dir: &std::path::Path,
    max_samples: u64,
    out: &mut impl FnMut(&[f32]),
) -> Result<(), Error> {
    let program = parse(&lex(src)?)?;
    let samples = samples::load_samples(&program, base_dir)?;
    cymbal_core::render::render_offline_streaming(src, max_samples, SAMPLE_RATE, &samples, out)
}

#[cfg(test)]
fn render_src(src: &str, base_dir: &std::path::Path, max_samples: u64) -> Result<Vec<f32>, Error> {
    let mut out = Vec::new();
    render_src_into(src, base_dir, max_samples, &mut |chunk| {
        out.extend_from_slice(chunk);
    })?;
    Ok(out)
}

fn export_src_to_wav(
    src: &str,
    base_dir: &std::path::Path,
    out_path: &std::path::Path,
) -> Result<(), String> {
    let mut w = cymbal_core::wav::WavWriter::create(out_path, SAMPLE_RATE, 2)
        .map_err(|e| format!("cannot create {}: {e}", out_path.display()))?;
    let mut write_err: Option<String> = None;
    let render = render_src_into(src, base_dir, MAX_SAMPLES, &mut |chunk| {
        if write_err.is_none()
            && let Err(e) = w.write_interleaved(chunk)
        {
            write_err = Some(e.to_string());
        }
    });
    let result = match (write_err, render) {
        (Some(e), _) => Err(format!("cannot write {}: {e}", out_path.display())),
        (None, Err(e)) => Err(format!("render failed: {e}")),
        (None, Ok(())) => w
            .finalize()
            .map_err(|e| format!("cannot finalize {}: {e}", out_path.display())),
    };
    if result.is_err() {
        let _ = std::fs::remove_file(out_path);
    }
    result
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
    if let Some(text) = cymbal_core::docs::help_text_src(&src) {
        eprintln!("{text}");
    }
    let mut w = cymbal_core::wav::WavWriter::create_with_format(output, SAMPLE_RATE, 2, format)
        .map_err(|e| format!("cannot write {}: {e}", output.display()))?;
    let mut write_err: Option<String> = None;
    let render = render_src_into(&src, base, max_samples, &mut |chunk| {
        if write_err.is_none()
            && let Err(e) = w.write_interleaved(chunk)
        {
            write_err = Some(e.to_string());
        }
    });
    let result = match (write_err, render) {
        (Some(e), _) => Err(format!("cannot write {}: {e}", output.display())),
        (None, Err(e)) => Err(format!("render failed: {e}")),
        (None, Ok(())) => w
            .finalize()
            .map_err(|e| format!("cannot finalize {}: {e}", output.display())),
    };
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
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

fn spares_needed(new_names: &[String], old: &HashMap<String, u64>) -> usize {
    new_names.iter().filter(|n| !old.contains_key(*n)).count() + 8
}

fn spawn_reload(
    src: String,
    base: std::path::PathBuf,
    latest: HashMap<String, u64>,
    recording: bool,
    seq: u64,
    msg_tx: mpsc::Sender<UiMsg>,
    queue: Arc<AudioQueue>,
) {
    std::thread::spawn(move || match loop_names(&src) {
        Ok(names) => {
            let gens = next_loop_generations(&latest, &names);
            match build_timeline_with(&src, SAMPLE_RATE, &base, &gens) {
                Ok(tl) => {
                    let needed = spares_needed(&names, &latest);
                    let window_end = tl.window_start.saturating_add(tl.window_len);
                    let _ = msg_tx.send(UiMsg::Reloaded {
                        generations: gens,
                        loops: names,
                        seq,
                        window_end,
                        src: src.clone(),
                    });
                    if let Some(text) = cymbal_core::docs::help_text_src(&src) {
                        let _ = msg_tx.send(UiMsg::Help(text));
                    }
                    let spares: Vec<_> = if recording {
                        (0..needed)
                            .map(|_| cymbal_audio::recorder::Recorder::new(32, 4096))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let _ = queue.send(Msg::Swap(Arc::new(tl), seq, spares));
                }
                Err(e) => {
                    let _ = msg_tx.send(UiMsg::Err(e, Some(seq)));
                }
            }
        }
        Err(e) => {
            let _ = msg_tx.send(UiMsg::Err(e, Some(seq)));
        }
    });
}

fn align_window_end(raw_end: u64, bars: &[u64]) -> u64 {
    bars.iter()
        .fold(raw_end, |end, b| end.max(raw_end / *b * *b + *b))
}

fn loop_bar_samples(program: &cymbal_core::ast::Program) -> Vec<u64> {
    use cymbal_core::ast::Stmt;
    use cymbal_core::transport::Transport;
    let tempo = program
        .statements
        .iter()
        .find_map(|s| {
            if let Stmt::Tempo(t, _) = s {
                Some(Transport::new(*t, SAMPLE_RATE).tempo)
            } else {
                None
            }
        })
        .unwrap_or(120.0);
    let mut bars = vec![Transport::new(tempo, SAMPLE_RATE).bar_samples()];
    for stmt in &program.statements {
        if let Stmt::Loop(l) = stmt {
            bars.push(Transport::new(l.tempo.unwrap_or(tempo), SAMPLE_RATE).bar_samples());
        }
    }
    bars
}

struct SegmentRequest {
    end: u64,
    src: String,
    base: std::path::PathBuf,
    latest: HashMap<String, u64>,
    recording: bool,
    seq: u64,
}

struct SegmentSpawner<'a> {
    base: &'a std::path::Path,
    reload_seq: &'a Arc<AtomicU64>,
    seq_lock: &'a Arc<Mutex<()>>,
    msg_tx: &'a mpsc::Sender<UiMsg>,
    queue: &'a Arc<AudioQueue>,
}

impl SegmentSpawner<'_> {
    fn dispatch(
        &self,
        scheduler: &mut SegmentScheduler,
        action: SegmentAction,
        applied_src: &str,
        latest: &HashMap<String, u64>,
        recording: bool,
        status: &mut Status,
    ) {
        let SegmentAction::Spawn { end, retries } = action else {
            return;
        };
        let seq = {
            let _guard = self.seq_lock.lock().unwrap();
            self.reload_seq.fetch_add(1, Ordering::SeqCst) + 1
        };
        scheduler.note_spawn(seq);
        spawn_segment(
            SegmentRequest {
                end,
                src: applied_src.to_string(),
                base: self.base.to_path_buf(),
                latest: latest.clone(),
                recording,
                seq,
            },
            self.reload_seq.clone(),
            self.seq_lock.clone(),
            self.msg_tx.clone(),
            self.queue.clone(),
        );
        if retries > 0 {
            status.clear_error();
            status.message = format!("segment failed; retrying ({retries}/2)");
        }
    }
}

fn spawn_segment(
    req: SegmentRequest,
    reload_seq: Arc<AtomicU64>,
    seq_lock: Arc<Mutex<()>>,
    msg_tx: mpsc::Sender<UiMsg>,
    queue: Arc<AudioQueue>,
) {
    std::thread::spawn(move || {
        let result: Result<Option<(u64, Vec<String>)>, Error> = (|| {
            let program = parse(&lex(&req.src)?)?;
            let samples = samples::load_samples(&program, &req.base)?;
            let names: Vec<String> = program
                .statements
                .iter()
                .filter_map(|s| match s {
                    cymbal_core::ast::Stmt::Loop(l) => Some(l.name.clone()),
                    _ => None,
                })
                .collect();
            let gens = next_loop_generations(&req.latest, &names);
            let raw_end = req.end.saturating_add(WINDOW_LEN);
            let window_end = align_window_end(raw_end, &loop_bar_samples(&program));
            let tl = cymbal_core::scheduler::schedule_window(
                &program,
                &gens,
                &samples,
                req.end,
                window_end - req.end,
                SAMPLE_RATE,
            )?;
            let spares: Vec<_> = if req.recording {
                (0..spares_needed(&names, &req.latest))
                    .map(|_| cymbal_audio::recorder::Recorder::new(32, 4096))
                    .collect()
            } else {
                Vec::new()
            };
            // Check-and-send is atomic with user reload bumps (they take the
            // same lock), so a stale segment can never replace a reload's swap
            // in the coalescing slot after the reload was requested. A failed
            // send is a failure: the caller must retry or the timeline stalls.
            let sent = {
                let _guard = seq_lock.lock().unwrap();
                if reload_seq.load(Ordering::SeqCst) <= req.seq {
                    queue.send(Msg::Swap(Arc::new(tl), req.seq, spares)).is_ok()
                } else {
                    false
                }
            };
            if sent {
                Ok(Some((window_end, names)))
            } else {
                Ok(None)
            }
        })();
        let (window_end, loops) = match result {
            Ok(Some((end, loops))) => (Some(end), loops),
            _ => (None, Vec::new()),
        };
        let _ = msg_tx.send(UiMsg::SegmentDone {
            window_end,
            seq: req.seq,
            loops,
        });
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
    bool, // docs
);

fn parse_args(args: &[String]) -> ParsedArgs<'_> {
    // returns (midi_port, render args (input, output, seconds), tracks args (input, outdir), tui file, docs)
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
                ["render", input, output] => {
                    (port, Some((input, output, None, false)), None, None, false)
                }
                ["render", "--tracks", input, outdir] => {
                    (port, None, Some((input, outdir)), None, false)
                }
                ["render", "--f32", input, output] => {
                    (port, Some((input, output, None, true)), None, None, false)
                }
                ["render", "--f32", input, output, seconds] => (
                    port,
                    Some((input, output, Some(seconds), true)),
                    None,
                    None,
                    false,
                ),
                ["render", input, output, seconds] => (
                    port,
                    Some((input, output, Some(seconds), false)),
                    None,
                    None,
                    false,
                ),
                [file] => (port, None, None, Some(file), false),
                _ => (port, None, None, None, false),
            }
        }
        Some("render") => {
            let rest: Vec<&str> = it.collect();
            match rest.as_slice() {
                [input, output] => (None, Some((input, output, None, false)), None, None, false),
                ["--tracks", input, outdir] => (None, None, Some((input, outdir)), None, false),
                ["--f32", input, output] => {
                    (None, Some((input, output, None, true)), None, None, false)
                }
                ["--f32", input, output, seconds] => (
                    None,
                    Some((input, output, Some(seconds), true)),
                    None,
                    None,
                    false,
                ),
                [input, output, seconds] => (
                    None,
                    Some((input, output, Some(seconds), false)),
                    None,
                    None,
                    false,
                ),
                _ => (None, None, None, None, false),
            }
        }
        Some("docs") => (None, None, None, None, true),
        Some(file) => (None, None, None, Some(file), false),
        None => (None, None, None, None, false),
    }
}

const COMPLETION_KEYWORDS: &[&str] = &["let", "loop", "tempo", "sample", "help", "rev", "every("];

fn note_names() -> Vec<String> {
    let mut out = Vec::new();
    for oct in 2..=5 {
        for letter in ['c', 'd', 'e', 'f', 'g', 'a', 'b'] {
            for acc in ["", "#", "b"] {
                out.push(format!("{letter}{acc}{oct}"));
            }
        }
    }
    out
}

fn completion_candidates(prefix: &str, let_names: &[String]) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |w: &str| {
        if w.starts_with(prefix) {
            out.push(w.to_string());
        }
    };
    for w in COMPLETION_KEYWORDS {
        push(w);
    }
    for w in cymbal_core::docs::PARAM_NAMES {
        push(w);
    }
    for w in note_names() {
        push(&w);
    }
    for w in cymbal_core::docs::VOICE_NAMES {
        push(w);
    }
    for w in let_names {
        if w.starts_with(prefix) {
            out.push(w.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn word_prefix(line: &str, col: usize) -> Option<String> {
    let byte_idx = line
        .char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let before = &line[..byte_idx];
    let start_byte = before
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '_' && *c != '#')
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word = &before[start_byte..];
    if word.is_empty() {
        None
    } else {
        Some(word.to_string())
    }
}

fn voice_names(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("let ")
            && let Some(name) = rest.split('=').next()
        {
            let name = name.trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (midi_port, render, tracks, tui_file, docs) = parse_args(&args);
    if docs {
        print!("{}", cymbal_core::docs::markdown());
        return std::process::ExitCode::SUCCESS;
    }
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
        "usage: cymbal [--midi [port]] <file.cym>\n       cymbal render <in.cym> <out.wav> [seconds]\n       cymbal render --f32 <in.cym> <out.wav> [seconds]\n       cymbal render --tracks <in.cym> <outdir>\n       cymbal docs"
    );
    std::process::ExitCode::from(2)
}

fn midi_toggle(
    midi: &Option<Arc<cymbal_audio::midi_out::MidiOut>>,
    sending: &mut bool,
) -> Option<String> {
    let Some(out) = midi else {
        return Some("no midi: nothing to start".into());
    };
    *sending = !*sending;
    let (byte, label) = if *sending {
        (0xFA, "midi start")
    } else {
        (0xFC, "midi stop")
    };
    let _ = out.try_send(cymbal_audio::midi_out::MidiItem::Sys {
        bytes: [byte, 0, 0],
        len: 1,
    });
    Some(label.into())
}

fn midi_toggle_key(
    status: &mut Status,
    midi: &Option<Arc<cymbal_audio::midi_out::MidiOut>>,
    sending: &mut bool,
) {
    if let Some(msg) = midi_toggle(midi, sending) {
        status.clear_error();
        status.message = msg;
        status.midi_sending = *sending;
    }
}

fn run_tui(file: &std::path::Path, midi_port: Option<String>) -> Result<(), String> {
    let src = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;

    let queue = Arc::new(AudioQueue::new(16));
    let ui_queue = cymbal_audio::ui_queue::UiQueue::new(64);
    let base = file.parent().unwrap_or_else(|| std::path::Path::new("."));
    let initial =
        build_timeline_with(&src, SAMPLE_RATE, base, &HashMap::new()).map_err(|e| e.to_string())?;
    let initial_window_end = initial.window_start.saturating_add(initial.window_len);
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
        midi_out.clone(),
        Some(ui_queue.clone()),
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

    let mut editor = Editor::new(src.clone());
    let mut applied_src = src;

    let mut recording = false;
    let mut help_open = false;
    let mut help_override: Option<String> = None;
    let mut help_scroll = 0u16;
    let mut completion: Option<(Vec<String>, usize)> = None;
    let mut midi_sending = false;
    let mut record_start: Option<Instant> = None;
    let mut record_writers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let reload_seq = Arc::new(AtomicU64::new(1));
    let seq_lock = Arc::new(Mutex::new(()));
    let mut scheduler = SegmentScheduler::new(initial_window_end);
    let segment_spawner = SegmentSpawner {
        base,
        reload_seq: &reload_seq,
        seq_lock: &seq_lock,
        msg_tx: &msg_tx,
        queue: &queue,
    };
    let mut latest_seq: u64 = 0;
    let mut user_reload_seq: u64 = 0;
    let mut recent: Vec<(u64, Vec<String>)> = Vec::new();
    let mut pending: PendingClaims = HashMap::new();
    let mut record_ts: Option<String> = None;
    let mut record_dir: Option<std::path::PathBuf> = None;

    let result = (|| -> io::Result<()> {
        loop {
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(k) = event::read()?
            {
                if k.modifiers.contains(KeyModifiers::CONTROL) {
                    completion = None;
                    match k.code {
                        KeyCode::Char('s') => {
                            let src = editor.content();
                            let base = file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let latest = latest_loops.clone();
                            scheduler.on_reload_started();
                            let seq = {
                                let _guard = seq_lock.lock().unwrap();
                                reload_seq.fetch_add(1, Ordering::SeqCst) + 1
                            };
                            user_reload_seq = seq;
                            spawn_reload(
                                src,
                                base,
                                latest,
                                recording,
                                seq,
                                msg_tx.clone(),
                                queue.clone(),
                            );
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
                                join_writers(
                                    &mut record_writers,
                                    Instant::now() + Duration::from_secs(5),
                                );
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
                                let spares: Vec<_> = (0..8)
                                    .map(|_| cymbal_audio::recorder::Recorder::new(32, 4096))
                                    .collect();
                                if queue
                                    .send(Msg::RecordStart {
                                        master: master_for_queue,
                                        tracks: tracks
                                            .iter()
                                            .map(|(n, r)| (n.clone(), r.clone()))
                                            .collect(),
                                        spares,
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
                                    pending.clear();
                                    record_ts = Some(ts);
                                    record_dir = Some(dir);
                                    status.recording = true;
                                    status.clear_error();
                                    status.message = "recording...".into();
                                }
                            } else {
                                recording = false;
                                if queue.send(Msg::RecordStop).is_err() {
                                    status.set_error("recording queue full: stop failed".into());
                                }
                                record_ts = None;
                                record_dir = None;
                                join_writers(
                                    &mut record_writers,
                                    Instant::now() + Duration::from_secs(5),
                                );
                                status.recording = false;
                                status.message = "stopping...".into();
                            }
                        }
                        KeyCode::Char('e') => {
                            let src = editor.content();
                            let dir = file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let out_path = free_path(&dir, "out");
                            let base = dir;
                            let tx = msg_tx.clone();
                            std::thread::spawn(move || {
                                match export_src_to_wav(&src, &base, &out_path) {
                                    Ok(()) => {
                                        let _ = tx.send(UiMsg::Info(format!(
                                            "exported {}",
                                            out_path.display()
                                        )));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(UiMsg::Err(
                                            Error::new(
                                                cymbal_core::error::Span { line: 0, col: 0 },
                                                cymbal_core::error::ErrorKind::Io,
                                                e,
                                            ),
                                            None,
                                        ));
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
                            scheduler.on_reload_started();
                            let seq = {
                                let _guard = seq_lock.lock().unwrap();
                                reload_seq.fetch_add(1, Ordering::SeqCst) + 1
                            };
                            user_reload_seq = seq;
                            spawn_reload(
                                src,
                                base,
                                latest,
                                recording,
                                seq,
                                msg_tx.clone(),
                                queue.clone(),
                            );
                        }
                        KeyCode::Char('-') => {
                            status.lower_tempo();
                            let src = apply_tempo_override(&editor.content(), status.tempo);
                            let base = file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let latest = latest_loops
                                .iter()
                                .map(|(k, v)| (k.clone(), v + 1))
                                .collect();
                            scheduler.on_reload_started();
                            let seq = {
                                let _guard = seq_lock.lock().unwrap();
                                reload_seq.fetch_add(1, Ordering::SeqCst) + 1
                            };
                            user_reload_seq = seq;
                            spawn_reload(
                                src,
                                base,
                                latest,
                                recording,
                                seq,
                                msg_tx.clone(),
                                queue.clone(),
                            );
                        }
                        KeyCode::Char('j') => {
                            midi_toggle_key(&mut status, &midi_out, &mut midi_sending);
                        }
                        _ => {}
                    }
                } else if help_open {
                    // panel is open: swallow every remaining key
                    completion = None;
                } else if k.modifiers.contains(KeyModifiers::ALT) {
                    completion = None;
                    if let Some(kind) = alt_transform_kind(k.code) {
                        let src = editor.content();
                        match cymbal_core::transform::transform_src(&src, editor.cursor().1, kind) {
                            Ok(new_src) => {
                                editor.set_content(new_src);
                                let base =
                                    file.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                                let latest = latest_loops.clone();
                                scheduler.on_reload_started();
                                let seq = {
                                    let _guard = seq_lock.lock().unwrap();
                                    reload_seq.fetch_add(1, Ordering::SeqCst) + 1
                                };
                                user_reload_seq = seq;
                                spawn_reload(
                                    editor.content(),
                                    base,
                                    latest,
                                    recording,
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
                    if !matches!(k.code, KeyCode::Tab | KeyCode::Enter) {
                        completion = None;
                    }
                    match k.code {
                        KeyCode::F(1) if help_open => {
                            help_open = false;
                            help_override = None;
                        }
                        KeyCode::F(1) => help_open = !help_open,
                        KeyCode::Esc if help_open => {
                            help_open = false;
                            help_override = None;
                        }
                        KeyCode::Up if help_open => {
                            help_scroll = help_scroll.saturating_sub(1);
                        }
                        KeyCode::Down if help_open => {
                            help_scroll = help_scroll.saturating_add(1);
                        }
                        KeyCode::Char(_) if help_open => {}
                        KeyCode::Enter if help_open => {}
                        KeyCode::Backspace if help_open => {}
                        KeyCode::Tab if help_open => {}
                        KeyCode::Left if help_open => {}
                        KeyCode::Right if help_open => {}
                        KeyCode::Home if help_open => {}
                        KeyCode::End if help_open => {}
                        KeyCode::Tab => {
                            if let Some((cands, idx)) = &mut completion {
                                *idx = (*idx + 1) % cands.len();
                            } else {
                                let (col, line_idx) = editor.cursor();
                                let prefix = word_prefix(&editor.lines()[line_idx], col);
                                let cands = prefix
                                    .map(|p| {
                                        completion_candidates(&p, &voice_names(&editor.content()))
                                    })
                                    .unwrap_or_default();
                                if !cands.is_empty() {
                                    completion = Some((cands, 0));
                                }
                            }
                        }
                        KeyCode::Enter => {
                            if let Some((cands, idx)) = completion.take() {
                                if let Some(word) = cands.get(idx) {
                                    editor.delete_word_before_cursor();
                                    editor.insert_str(word);
                                }
                            } else {
                                editor.newline();
                            }
                        }
                        KeyCode::Backspace => editor.backspace(),
                        KeyCode::Left => editor.move_left(),
                        KeyCode::Right => editor.move_right(),
                        KeyCode::Up => editor.move_up(),
                        KeyCode::Down => editor.move_down(),
                        KeyCode::Home => editor.move_home(),
                        KeyCode::End => editor.move_end(),
                        KeyCode::Char(c) => editor.insert_char(c),
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
                        window_end,
                        src,
                    } => {
                        let accepted = seq > latest_seq;
                        let action = if accepted {
                            scheduler.on_reload_settled(Some(window_end))
                        } else {
                            // Stale: a newer reload's settle already ran. Its
                            // outcome must not write the scheduler's result —
                            // a superseded reload neither applied a swap nor
                            // left the pipeline un-driven, and recording it
                            // as a failure would let a late segment failure
                            // dispatch a retry that displaces the newer
                            // reload's queued swap.
                            SegmentAction::None
                        };
                        if accepted {
                            applied_src = src;
                        }
                        handle_reloaded(
                            &mut status,
                            &mut latest_loops,
                            &mut latest_seq,
                            &mut recent,
                            generations,
                            loops,
                            seq,
                        );
                        flush_pending_claims(
                            seq,
                            &mut pending,
                            &recent,
                            record_ts.as_deref(),
                            record_dir.as_deref(),
                            &msg_tx,
                            &mut record_writers,
                        );
                        match action {
                            SegmentAction::Spawn { .. } => segment_spawner.dispatch(
                                &mut scheduler,
                                action,
                                &applied_src,
                                &latest_loops,
                                recording,
                                &mut status,
                            ),
                            SegmentAction::Error(msg) => status.set_error(msg),
                            SegmentAction::None => {}
                        }
                    }
                    UiMsg::RecordError(s) => {
                        recording = false;
                        record_start = None;
                        record_ts = None;
                        record_dir = None;
                        if queue.send(Msg::RecordStop).is_err() {
                            status.set_error(format!("{s} (recording queue full: stop failed)"));
                        } else {
                            status.set_error(s);
                        }
                        join_writers(&mut record_writers, Instant::now() + Duration::from_secs(5));
                        status.recording = false;
                    }
                    UiMsg::Err(e, seq) => {
                        if let Some(seq) = seq {
                            // Only the newest STARTED user reload settles the
                            // scheduler. A failing reload never advances
                            // latest_seq (that only moves on an accepted
                            // Reloaded), so gating on it would let an older
                            // reload's Err clear reload_in_flight while a
                            // newer reload is still compiling: the stale
                            // failure would then dispatch a stored retry that
                            // displaces the newer reload's queued swap.
                            // user_reload_seq counts user reload dispatches
                            // only — reload_seq is shared with segment
                            // dispatches, so gating on it would reject a
                            // genuine Err when a segment dispatched in between.
                            let action = if seq == user_reload_seq {
                                scheduler.on_reload_settled(None)
                            } else {
                                SegmentAction::None
                            };
                            flush_pending_claims(
                                seq,
                                &mut pending,
                                &recent,
                                record_ts.as_deref(),
                                record_dir.as_deref(),
                                &msg_tx,
                                &mut record_writers,
                            );
                            match action {
                                SegmentAction::Spawn { .. } => segment_spawner.dispatch(
                                    &mut scheduler,
                                    action,
                                    &applied_src,
                                    &latest_loops,
                                    recording,
                                    &mut status,
                                ),
                                SegmentAction::Error(msg) => status.set_error(msg),
                                SegmentAction::None => {}
                            }
                        }
                        status.set_error(e.to_string());
                    }
                    UiMsg::Help(text) => {
                        help_override = Some(text);
                        help_open = true;
                    }
                    UiMsg::SegmentDone {
                        window_end,
                        seq,
                        loops,
                    } => {
                        // Superseded: a newer reload was dispatched after this
                        // segment started, so its swap may never be applied —
                        // its end must not advance last_window_end. The
                        // engine's own NeedSegment carries the truth.
                        let superseded =
                            !scheduler.is_current(seq) || reload_seq.load(Ordering::SeqCst) > seq;
                        let action = scheduler.on_segment_done(window_end, seq, superseded);
                        if window_end.is_some() {
                            // Segment swaps can claim recording tracks: the
                            // swap's loops register in the claim history just
                            // like a reload, so pends resolve in arrival order.
                            record_reload(&mut recent, seq, loops);
                            flush_pending_claims(
                                seq,
                                &mut pending,
                                &recent,
                                record_ts.as_deref(),
                                record_dir.as_deref(),
                                &msg_tx,
                                &mut record_writers,
                            );
                        }
                        match action {
                            SegmentAction::Spawn { .. } => segment_spawner.dispatch(
                                &mut scheduler,
                                action,
                                &applied_src,
                                &latest_loops,
                                recording,
                                &mut status,
                            ),
                            SegmentAction::Error(msg) => status.set_error(msg),
                            SegmentAction::None => {}
                        }
                    }
                    m => apply_ui_msg(&mut status, m),
                }
            }
            let _ = queue.take_retired();
            while let Some(ev) = ui_queue.try_pop() {
                match ev {
                    cymbal_audio::ui_queue::UiEvent::Bar(n) => status.bar = n,
                    cymbal_audio::ui_queue::UiEvent::MidiDropped(n) => {
                        status.message = format!("MIDI queue overflow — {n} messages dropped");
                    }
                    cymbal_audio::ui_queue::UiEvent::TrackClaimed {
                        rec,
                        seq,
                        loop_index,
                    } => match handle_claim(
                        &rec,
                        seq,
                        loop_index,
                        &recent,
                        &mut pending,
                        latest_seq,
                        record_ts.is_some(),
                    ) {
                        ClaimAction::Spawn(name) => {
                            record_writers.push(spawn_track_writer(
                                &rec,
                                &name,
                                record_ts.as_deref(),
                                record_dir.as_deref(),
                                &msg_tx,
                            ));
                        }
                        ClaimAction::Pend => {}
                        ClaimAction::Stale => {
                            let _ = msg_tx.send(UiMsg::Err(
                                Error::new(
                                    cymbal_core::error::Span { line: 0, col: 0 },
                                    cymbal_core::error::ErrorKind::Io,
                                    format!("stale mid-recording track claim (seq {seq})"),
                                ),
                                None,
                            ));
                        }
                        ClaimAction::Ignore => {}
                    },
                    cymbal_audio::ui_queue::UiEvent::NeedSegment(end) => {
                        // Authoritative (the engine fires once per applied swap
                        // with its real window end): defer, never drop. A stale
                        // request is either overwritten by a newer one or its
                        // swap is rejected by the sender's seq guard.
                        let action = scheduler.on_need_segment(end);
                        match action {
                            SegmentAction::Spawn { .. } => segment_spawner.dispatch(
                                &mut scheduler,
                                action,
                                &applied_src,
                                &latest_loops,
                                recording,
                                &mut status,
                            ),
                            SegmentAction::Error(msg) => status.set_error(msg),
                            SegmentAction::None => {}
                        }
                    }
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
                let mut status_text = status.render();
                if let Some((cands, idx)) = &completion
                    && let Some(w) = cands.get(*idx)
                {
                    status_text.push_str(&format!(
                        "  | complete: {w} ({} of {}, Tab next, Enter accept, Esc close)",
                        idx + 1,
                        cands.len()
                    ));
                }
                f.render_widget(
                    Paragraph::new(status_text).block(Block::default().borders(Borders::ALL)),
                    chunks[1],
                );
                if help_open {
                    let text = match &help_override {
                        Some(t) => t.clone(),
                        None => {
                            let cursor_line =
                                editor.lines().get(editor.cursor().1).map(|l| l.as_str());
                            let cursor_col = editor.cursor().0;
                            help_panel::help_panel_text(cursor_line.map(|l| (l, cursor_col)))
                        }
                    };
                    let lines: Vec<ratatui::text::Line> =
                        text.lines().map(ratatui::text::Line::raw).collect();
                    let panel = Paragraph::new(lines)
                        .block(Block::default().borders(Borders::ALL).title("help"))
                        .scroll((help_scroll, 0));
                    let area = chunks[0];
                    f.render_widget(Clear, area);
                    f.render_widget(panel, area);
                }
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
    fn export_uses_collision_guard() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("cymbal_export_src_{}.cym", std::process::id()));
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(f, "let kick = kick()\nloop \"b\":\n    kick << \"x\"\n").unwrap();
        let out_dir = dir.join(format!("cymbal_export_out_{}", std::process::id()));
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(out_dir.join("out.wav"), b"occupied").unwrap();
        render_to_wav(&src_path, &free_path(&out_dir, "out"), 1).unwrap();
        assert!(
            out_dir.join("out-2.wav").exists(),
            "collision must pick out-2.wav"
        );
        assert_eq!(
            std::fs::read(out_dir.join("out.wav")).unwrap(),
            b"occupied",
            "existing file untouched"
        );
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_dir_all(&out_dir);
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
    fn segment_swap_lands_with_claim_loops() {
        // The segment producer must deliver its swap AND register the swap's
        // loops in the claim history, so mid-recording claims for its seq
        // resolve like reload claims (precondition: recording claims).
        let queue = Arc::new(AudioQueue::new(16));
        let reload_seq = Arc::new(AtomicU64::new(1));
        let seq_lock = Arc::new(Mutex::new(()));
        let (tx, rx) = mpsc::channel::<UiMsg>();
        spawn_segment(
            SegmentRequest {
                end: 96000,
                src: "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x\"\n".into(),
                base: std::path::PathBuf::from("."),
                latest: HashMap::new(),
                recording: false,
                seq: 2,
            },
            reload_seq,
            seq_lock,
            tx,
            queue.clone(),
        );
        let msg = rx.recv().unwrap();
        let UiMsg::SegmentDone {
            window_end,
            seq,
            loops,
        } = msg
        else {
            panic!("expected SegmentDone");
        };
        assert_eq!(seq, 2);
        // end 96000 + 300s = 14496000; alignment extends to the next 120bpm bar
        // multiple (the window is half-open): 151*96000 + 96000.
        assert_eq!(window_end, Some(14592000));
        assert_eq!(loops, vec!["b".to_string()]);
        let swap = loop {
            if let Some(Msg::Swap(tl, seq, _)) = queue.try_recv() {
                assert_eq!(seq, 2);
                break tl;
            }
        };
        assert_eq!(swap.window_start, 96000);
        assert_eq!(swap.window_len, 14592000 - 96000);
        assert_eq!(swap.loops, vec!["b".to_string()]);
        assert!(
            swap.events
                .iter()
                .all(|e| e.sample_offset < swap.window_len),
            "events are window-relative"
        );
    }

    #[test]
    fn segment_send_is_suppressed_after_reload_bump() {
        // A reload bumped reload_seq past the request's seq before the send:
        // the swap must not reach the engine (precondition: TOCTOU precedence).
        let queue = Arc::new(AudioQueue::new(16));
        let reload_seq = Arc::new(AtomicU64::new(3));
        let seq_lock = Arc::new(Mutex::new(()));
        let (tx, rx) = mpsc::channel::<UiMsg>();
        spawn_segment(
            SegmentRequest {
                end: 96000,
                src: "tempo 120\nlet kick = kick()\nloop \"b\":\n    kick << \"x\"\n".into(),
                base: std::path::PathBuf::from("."),
                latest: HashMap::new(),
                recording: false,
                seq: 2,
            },
            reload_seq,
            seq_lock,
            tx,
            queue.clone(),
        );
        let msg = rx.recv().unwrap();
        let UiMsg::SegmentDone {
            window_end, seq, ..
        } = msg
        else {
            panic!("expected SegmentDone");
        };
        assert_eq!(seq, 2);
        assert_eq!(window_end, None, "stale segment must report failure");
        assert!(queue.try_recv().is_none(), "no swap may reach the engine");
    }

    #[test]
    fn segment_claim_pends_then_flushes_from_segment_loops() {
        // The SegmentDone handler's sequence: a mid-recording claim for a
        // segment seq pends until SegmentDone registers the segment's loops,
        // then flushes to Spawn — never Lost (precondition: recording claims).
        let mut pending: PendingClaims = HashMap::new();
        let mut recent: Vec<(u64, Vec<String>)> = Vec::new();
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        assert!(matches!(
            handle_claim(&rec, 9, 0, &recent, &mut pending, 8, true),
            ClaimAction::Pend
        ));
        record_reload(&mut recent, 9, vec!["b".to_string()]);
        assert_eq!(drain_pending(&mut pending, 9).len(), 1);
        assert!(matches!(
            flush_claim(9, 0, &recent, true),
            FlushAction::Spawn(name) if name == "b"
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn handle_reloaded_sets_ordered_loops_and_noop_message() {
        let mut status = Status::new();
        let mut latest = HashMap::new();
        let mut latest_seq = 0;
        let mut recent = Vec::new();
        let mut gens = HashMap::new();
        gens.insert("b".to_string(), 1u64);
        latest.insert("b".to_string(), 1u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            &mut recent,
            gens.clone(),
            vec!["b".to_string()],
            1,
        );
        assert_eq!(status.loops, vec!["b".to_string()]);
        assert_eq!(status.message, "reloaded: nothing changed");
        assert_eq!(latest, gens);
        assert_eq!(latest_seq, 1);
        assert_eq!(recent, vec![(1, vec!["b".to_string()])]);
    }

    #[test]
    fn handle_reloaded_reports_changed_loops() {
        let mut status = Status::new();
        let mut latest = HashMap::new();
        latest.insert("a".to_string(), 1u64);
        latest.insert("b".to_string(), 1u64);
        let mut latest_seq = 0;
        let mut recent = Vec::new();
        let mut gens = HashMap::new();
        gens.insert("a".to_string(), 1u64);
        gens.insert("b".to_string(), 2u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            &mut recent,
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
        let mut recent = Vec::new();
        let mut gens = HashMap::new();
        gens.insert("b".to_string(), 2u64);
        gens.insert("k".to_string(), 3u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            &mut recent,
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
        let mut recent = Vec::new();
        let mut gens = HashMap::new();
        gens.insert("z".to_string(), 9u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            &mut recent,
            gens,
            vec!["z".to_string()],
            3,
        );
        assert_eq!(status.loops, vec!["a".to_string()]);
        assert_eq!(status.message, "reloaded: a");
        assert_eq!(latest.get("z"), None);
        assert_eq!(latest_seq, 3);
        assert_eq!(
            recent,
            vec![(3, vec!["z".to_string()])],
            "a stale seq is still recorded in arrival order"
        );
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
    fn midi_toggle_switches_start_stop() {
        let out = cymbal_audio::midi_out::MidiOut::new(64);
        let mut sending = false;
        let msg1 = midi_toggle(&Some(out.clone()), &mut sending);
        assert!(sending);
        assert!(msg1.unwrap().contains("midi start"));
        assert_eq!(out.take_sys(), Some(vec![0xFA]));
        let msg2 = midi_toggle(&Some(out.clone()), &mut sending);
        assert!(!sending);
        assert!(msg2.unwrap().contains("midi stop"));
        assert_eq!(out.take_sys(), Some(vec![0xFC]));
        let msg3 = midi_toggle(&None, &mut sending);
        assert!(msg3.unwrap().contains("no midi"));
    }

    #[test]
    fn midi_toggle_key_wires_transport_state_into_status() {
        let out = cymbal_audio::midi_out::MidiOut::new(64);
        let mut status = Status::new();
        let mut sending = false;
        midi_toggle_key(&mut status, &Some(out.clone()), &mut sending);
        assert!(status.midi_sending);
        assert!(status.message.contains("midi start"));
        midi_toggle_key(&mut status, &Some(out.clone()), &mut sending);
        assert!(!status.midi_sending);
        assert!(status.message.contains("midi stop"));
    }

    #[test]
    fn midi_toggle_key_without_port_reports_no_midi() {
        let mut status = Status::new();
        let mut sending = false;
        midi_toggle_key(&mut status, &None, &mut sending);
        assert!(!sending);
        assert!(!status.midi_sending);
        assert_eq!(status.message, "no midi: nothing to start");
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, None);
        assert_eq!(tracks, None);
        assert_eq!(file, Some("a.cym"));
    }

    #[test]
    fn parse_args_midi_file_uses_first_port() {
        let args = vec!["--midi".to_string(), "a.cym".to_string()];
        let (midi, render, tracks, file, _docs) = parse_args(&args);
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
        assert_eq!(midi, Some(String::new()));
        assert_eq!(render, Some(("in.cym", "out.wav", None, false)));
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_midi_render_with_seconds() {
        let args = vec![
            "--midi".to_string(),
            "render".to_string(),
            "in.cym".to_string(),
            "out.wav".to_string(),
            "30".to_string(),
        ];
        let (midi, render, tracks, file, _docs) = parse_args(&args);
        assert_eq!(midi, Some(String::new()));
        assert_eq!(render, Some(("in.cym", "out.wav", Some("30"), false)));
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
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
        let (midi, render, tracks, file, _docs) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, None);
        assert_eq!(tracks, Some(("in.cym", "outdir")));
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_garbage_is_all_none() {
        let args = vec!["render".to_string(), "in.cym".to_string()];
        let (midi, render, tracks, file, _docs) = parse_args(&args);
        assert_eq!(midi, None);
        assert_eq!(render, None);
        assert_eq!(tracks, None);
        assert_eq!(file, None);
    }

    #[test]
    fn parse_args_accepts_docs() {
        let args = vec!["docs".to_string()];
        let (_, _, _, _, docs) = parse_args(&args);
        assert!(docs);
    }

    #[test]
    fn parse_args_defaults_docs_false() {
        let args = vec!["beat.cym".to_string()];
        let (_, _, _, _, docs) = parse_args(&args);
        assert!(!docs);
    }

    #[test]
    fn claim_resolution_uses_recent_reload_history() {
        let recent: Vec<(u64, Vec<String>)> = vec![(1, vec!["b".to_string()])];
        let name = resolve_claim(&recent, 1, 0);
        assert_eq!(name, Some("b".to_string()));
        let name = resolve_claim(&recent, 1, 3);
        assert_eq!(name, None, "index out of range");
    }

    #[test]
    fn claim_history_bounded_to_sixty_four() {
        let mut recent = Vec::new();
        for seq in 1..=65 {
            record_reload(&mut recent, seq, vec![format!("l{seq}")]);
        }
        assert_eq!(recent.len(), 64);
        assert_eq!(recent.first().unwrap().0, 2, "oldest entry evicted");
    }

    #[test]
    fn pending_claims_flush_on_matching_reload() {
        let mut pending: PendingClaims = HashMap::new();
        pending.insert(
            3,
            vec![(
                0,
                "claim".to_string(),
                cymbal_audio::recorder::Recorder::new(4, 4),
            )],
        );
        let flushed = drain_pending(&mut pending, 3);
        assert_eq!(flushed.len(), 1);
        assert_eq!((flushed[0].0, flushed[0].1.as_str()), (0, "claim"));
        assert!(pending.is_empty());
        let stale = drain_pending(&mut pending, 4);
        assert!(stale.is_empty());
    }

    #[test]
    fn claim_at_current_seq_is_not_stale() {
        let recent: Vec<(u64, Vec<String>)> = vec![(2, vec!["b".to_string(), "h".to_string()])];
        let name = resolve_claim(&recent, 2, 1);
        assert_eq!(name, Some("h".to_string()));
        assert!(!is_stale(2, 2), "seq == latest_seq is not stale");
        assert!(is_stale(1, 2), "only older-than-seen swaps are stale");
    }

    #[test]
    fn join_writers_respects_the_shared_deadline() {
        let fast = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(100)));
        let slow = std::thread::spawn(|| std::thread::sleep(Duration::from_secs(10)));
        let mut writers = vec![fast, slow];
        let deadline = Instant::now() + Duration::from_millis(300);
        join_writers(&mut writers, deadline);
        assert!(writers.is_empty(), "handles are consumed either way");
    }

    #[test]
    fn join_writers_finishes_before_deadline_and_detaches_after() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let finished = Arc::new(AtomicBool::new(false));
        let f1 = finished.clone();
        let mut writers = vec![std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            f1.store(true, Ordering::Relaxed);
        })];
        join_writers(&mut writers, Instant::now() + Duration::from_secs(5));
        assert!(writers.is_empty());
        assert!(
            finished.load(Ordering::Relaxed),
            "a 1s writer finishes within a 5s deadline"
        );
        let detached = Arc::new(AtomicBool::new(false));
        let d1 = detached.clone();
        let mut writers = vec![std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            d1.store(true, Ordering::Relaxed);
        })];
        join_writers(&mut writers, Instant::now() + Duration::from_millis(50));
        assert!(writers.is_empty());
        assert!(
            !detached.load(Ordering::Relaxed),
            "a 10s writer is detached after a 50ms deadline"
        );
    }

    #[test]
    fn spawn_track_writer_writes_claimed_loop_to_timestamped_file() {
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        let mut block = rec.take_pool_block();
        block[..4].copy_from_slice(&[0.25, -0.25, 0.25, -0.25]);
        rec.push_filled(block);
        rec.stop();
        let dir = std::env::temp_dir().join(format!("cymbal_claim_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, _rx) = mpsc::channel::<UiMsg>();
        let handle = spawn_track_writer(&rec, "beat 1", Some("20260804-153245"), Some(&dir), &tx);
        handle.join().unwrap();
        assert!(
            dir.join("recording-20260804-153245-beat-1.wav").exists(),
            "claimed loop written with the ts-name convention"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_claim_spawns_for_current_seq() {
        let recent: Vec<(u64, Vec<String>)> = vec![(2, vec!["b".to_string(), "h".to_string()])];
        let mut pending: PendingClaims = HashMap::new();
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        assert!(matches!(
            handle_claim(&rec, 2, 1, &recent, &mut pending, 2, true),
            ClaimAction::Spawn(name) if name == "h"
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn handle_claim_errors_on_stale_seq() {
        let recent: Vec<(u64, Vec<String>)> = vec![(1, vec!["b".to_string()])];
        let mut pending: PendingClaims = HashMap::new();
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        assert!(matches!(
            handle_claim(&rec, 1, 0, &recent, &mut pending, 2, true),
            ClaimAction::Stale
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn handle_claim_pends_for_unseen_seq() {
        let recent: Vec<(u64, Vec<String>)> = vec![(2, vec!["b".to_string()])];
        let mut pending: PendingClaims = HashMap::new();
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        assert!(matches!(
            handle_claim(&rec, 3, 0, &recent, &mut pending, 2, true),
            ClaimAction::Pend
        ));
        assert_eq!(pending.get(&3).map(Vec::len), Some(1));
        assert_eq!(pending[&3][0].0, 0);
    }

    #[test]
    fn handle_claim_ignores_off_recording() {
        let recent: Vec<(u64, Vec<String>)> = vec![(2, vec!["b".to_string()])];
        let mut pending: PendingClaims = HashMap::new();
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        assert!(matches!(
            handle_claim(&rec, 2, 0, &recent, &mut pending, 2, false),
            ClaimAction::Ignore
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn flush_claim_spawns_once_the_reload_lands() {
        let recent: Vec<(u64, Vec<String>)> = vec![(3, vec!["b".to_string(), "h".to_string()])];
        assert!(matches!(
            flush_claim(3, 1, &recent, true),
            FlushAction::Spawn(name) if name == "h"
        ));
    }

    #[test]
    fn flush_claim_drops_pend_when_reload_failed() {
        // a claim pended for a reload that errored never resolves: the
        // reload-error path flushes it and drops the pend, so the reload's
        // own error is the one the user sees.
        let recent: Vec<(u64, Vec<String>)> = vec![(2, vec!["b".to_string()])];
        let mut pending: PendingClaims = HashMap::new();
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        assert!(matches!(
            handle_claim(&rec, 3, 0, &recent, &mut pending, 2, true),
            ClaimAction::Pend
        ));
        let drained = drain_pending(&mut pending, 3);
        assert_eq!(
            drained.len(),
            1,
            "the pended claim is drained when its reload errors"
        );
        assert!(pending.is_empty(), "the pend is dropped, not left behind");
        assert!(
            matches!(flush_claim(3, 0, &recent, true), FlushAction::Lost),
            "a reload that never lands cannot spawn a writer"
        );
    }

    #[test]
    fn flush_claim_ignores_off_recording() {
        let recent: Vec<(u64, Vec<String>)> = vec![(3, vec!["b".to_string()])];
        assert!(matches!(
            flush_claim(3, 0, &recent, false),
            FlushAction::Ignore
        ));
    }

    #[test]
    fn out_of_order_reloads_resolve_claims() {
        // reload seq 6 arrives before seq 5; a TrackClaimed for seq 5 must
        // resolve to Spawn once seq 5's Reloaded arrives — never Lost.
        let mut recent: Vec<(u64, Vec<String>)> = vec![(6, vec!["h".to_string()])];
        let mut pending: PendingClaims = HashMap::new();
        let rec = cymbal_audio::recorder::Recorder::new(4, 4);
        // claim(seq 5) -> Pend
        assert!(matches!(
            handle_claim(&rec, 5, 0, &recent, &mut pending, 5, true),
            ClaimAction::Pend
        ));
        // Reloaded(seq 5) arrives -> recorded in arrival order
        let mut status = Status::new();
        let mut latest = HashMap::new();
        let mut latest_seq = 5;
        let mut gens = HashMap::new();
        gens.insert("b".to_string(), 1u64);
        handle_reloaded(
            &mut status,
            &mut latest,
            &mut latest_seq,
            &mut recent,
            gens,
            vec!["b".to_string()],
            5,
        );
        assert_eq!(
            recent.last().map(|(s, _)| *s),
            Some(5),
            "the out-of-order reload is still recorded"
        );
        // drain_pending -> flush_claim -> Spawn, never Lost
        assert_eq!(drain_pending(&mut pending, 5).len(), 1);
        assert!(
            matches!(flush_claim(5, 0, &recent, true), FlushAction::Spawn(name) if name == "b"),
            "an out-of-order reload resolves the claim to Spawn, never Lost"
        );
    }

    #[test]
    fn spares_needed_counts_net_additions() {
        let old: HashMap<String, u64> =
            ["a", "b", "c"].iter().map(|n| (n.to_string(), 1)).collect();
        let new_names: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| n.to_string())
            .collect();
        assert_eq!(spares_needed(&new_names, &old), 10);
    }

    #[test]
    fn spares_needed_counts_replaced_loops() {
        let old: HashMap<String, u64> = (0..30).map(|i| (format!("old-{i}"), 1u64)).collect();
        let new_names: Vec<String> = (0..30).map(|i| format!("new-{i}")).collect();
        assert_eq!(spares_needed(&new_names, &old), 38);
    }

    #[test]
    fn spares_needed_keeps_buffer_when_nothing_changes() {
        let old: HashMap<String, u64> = ["a", "b"].iter().map(|n| (n.to_string(), 1)).collect();
        let new_names: Vec<String> = ["a", "b"].iter().map(|n| n.to_string()).collect();
        assert_eq!(spares_needed(&new_names, &old), 8);
    }

    #[test]
    fn window_end_rounds_up_to_every_loop_bar() {
        // 120bpm global bar 96000, 240bpm loop bar 48000: raw end 100000 must
        // move to the next global multiple (192000), which also clears 144000.
        let bars = vec![96000, 48000];
        assert_eq!(align_window_end(100_000, &bars), 192_000);
        assert_eq!(align_window_end(200_000, &bars), 288_000);
    }

    #[test]
    fn window_end_extends_already_aligned_ends() {
        // an end already on the grid still moves to the next multiple: the
        // window is [start, end), so a hit exactly at the raw end belongs to
        // the next window and must not be scheduled by this one.
        assert_eq!(align_window_end(96_000, &[96_000]), 192_000);
    }

    #[test]
    fn loop_bar_samples_reads_per_loop_tempos() {
        let program = parse(
            &lex("tempo 120\nlet kick = kick()\nloop \"b\" tempo=240:\n    kick << \"x\"\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(loop_bar_samples(&program), vec![96000, 48000]);
    }

    #[test]
    fn completion_candidates_find_keywords_and_params() {
        let c = completion_candidates("ve", &[]);
        assert!(c.contains(&"vel".to_string()));
        let c = completion_candidates("tem", &[]);
        assert!(c.contains(&"tempo".to_string()));
        let c = completion_candidates("every", &[]);
        assert_eq!(c, vec!["every(".to_string()]);
    }

    #[test]
    fn completion_candidates_include_voices_and_let_names() {
        let c = completion_candidates("k", &["kick".to_string(), "klank".to_string()]);
        assert!(c.contains(&"kick".to_string()));
        assert!(c.contains(&"klank".to_string()));
        let c = completion_candidates("", &["kick".to_string()]);
        assert!(c.is_empty());
    }

    #[test]
    fn word_before_cursor_extracts_prefix() {
        assert_eq!(
            word_prefix("    kick << \"x\" ve", 18),
            Some("ve".to_string())
        );
        assert_eq!(word_prefix("kick", 4), Some("kick".to_string()));
        assert_eq!(word_prefix("  ", 2), None);
    }

    #[test]
    fn word_prefix_survives_multibyte_preceding() {
        assert_eq!(word_prefix("ével", 4), Some("vel".to_string()));
    }

    #[test]
    fn voice_names_from_src() {
        let src = "let kick = kick()\nlet clap = sample \"clap\"\n";
        assert_eq!(
            voice_names(src),
            vec!["kick".to_string(), "clap".to_string()]
        );
    }
}
