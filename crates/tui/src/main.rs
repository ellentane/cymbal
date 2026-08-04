mod editor;
mod highlight;
mod status;

use std::io;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

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
    Loops(Vec<String>),
}

fn apply_ui_msg(status: &mut Status, msg: UiMsg) {
    match msg {
        UiMsg::Err(e) => status.set_error(e.to_string()),
        UiMsg::Info(s) => {
            status.clear_error();
            status.message = s;
        }
        UiMsg::Loops(loops) => status.loops = loops,
    }
}

pub fn build_timeline(src: &str, sample_rate: u32) -> Result<Timeline, Error> {
    let tokens = lex(src)?;
    let program = parse(&tokens)?;
    let mut loop_generations = std::collections::HashMap::new();
    for stmt in &program.statements {
        if let cymbal_core::ast::Stmt::Loop(l) = stmt {
            loop_generations.insert(l.name.clone(), 0);
        }
    }
    cymbal_core::scheduler::schedule(
        &program,
        &loop_generations,
        &std::collections::HashMap::new(),
        MAX_SAMPLES,
        sample_rate,
    )
}

fn render_to_wav(
    input: &std::path::Path,
    output: &std::path::Path,
    seconds: u64,
) -> Result<(), String> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    let max_samples = seconds.saturating_mul(SAMPLE_RATE as u64).min(MAX_SAMPLES);
    let samples = cymbal_core::render::render_offline(
        &src,
        max_samples,
        SAMPLE_RATE,
        &std::collections::HashMap::new(),
    )
    .map_err(|e| format!("render failed: {e}"))?;
    cymbal_core::wav::write_wav(output, &samples, SAMPLE_RATE)
        .map_err(|e| format!("cannot write {}: {e}", output.display()))
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

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["render", input, output] => {
            match render_to_wav(
                std::path::Path::new(input),
                std::path::Path::new(output),
                RENDER_DEFAULT_SECONDS,
            ) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("{msg}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        ["render", input, output, seconds] => match seconds.parse::<u64>() {
            Ok(s) if s > 0 => {
                match render_to_wav(std::path::Path::new(input), std::path::Path::new(output), s) {
                    Ok(()) => std::process::ExitCode::SUCCESS,
                    Err(msg) => {
                        eprintln!("{msg}");
                        std::process::ExitCode::FAILURE
                    }
                }
            }
            _ => {
                eprintln!("invalid seconds: {seconds}");
                std::process::ExitCode::FAILURE
            }
        },
        [file] => match run_tui(std::path::Path::new(file)) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("{msg}");
                std::process::ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: cymbal <file.cym>\n       cymbal render <in.cym> <out.wav> [seconds]"
            );
            std::process::ExitCode::from(2)
        }
    }
}

fn run_tui(file: &std::path::Path) -> Result<(), String> {
    let src = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;

    let queue = Arc::new(AudioQueue::new(16));
    let initial = build_timeline(&src, SAMPLE_RATE).map_err(|e| e.to_string())?;
    let mut status = Status::new();
    status.loops = initial.loops.clone();
    let handle = match cymbal_audio::stream::start_audio(queue.clone(), Arc::new(initial), |_e| {})
    {
        Ok(h) => Some(h),
        Err(e) => {
            status.set_error(e.into_error().to_string());
            None
        }
    };
    status.latency_ms = handle.as_ref().and_then(|h| h.latency_ms);

    let (msg_tx, msg_rx) = mpsc::channel::<UiMsg>();

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let mut editor = Editor::new(src);

    let result = (|| -> io::Result<()> {
        loop {
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(k) = event::read()?
            {
                if k.modifiers.contains(KeyModifiers::CONTROL) {
                    match k.code {
                        KeyCode::Char('s') => {
                            let src = editor.content();
                            let queue = queue.clone();
                            let tx = msg_tx.clone();
                            std::thread::spawn(move || match build_timeline(&src, SAMPLE_RATE) {
                                Ok(tl) => {
                                    let _ = tx.send(UiMsg::Loops(tl.loops.clone()));
                                    let _ = queue.send(Msg::Swap(Arc::new(tl)));
                                }
                                Err(e) => {
                                    let _ = tx.send(UiMsg::Err(e));
                                }
                            });
                            status.clear_error();
                            status.message = "reloading...".into();
                        }
                        KeyCode::Char('q') => break,
                        KeyCode::Char('e') => {
                            let src = editor.content();
                            let out_path = file
                                .parent()
                                .map(|p| p.join("out.wav"))
                                .unwrap_or_else(|| std::path::PathBuf::from("out.wav"));
                            let tx = msg_tx.clone();
                            std::thread::spawn(move || {
                                match cymbal_core::render::render_offline(
                                    &src,
                                    MAX_SAMPLES,
                                    SAMPLE_RATE,
                                    &std::collections::HashMap::new(),
                                ) {
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
                            let queue = queue.clone();
                            let tx = msg_tx.clone();
                            std::thread::spawn(move || match build_timeline(&src, SAMPLE_RATE) {
                                Ok(tl) => {
                                    let _ = tx.send(UiMsg::Loops(tl.loops.clone()));
                                    let _ = queue.send(Msg::Swap(Arc::new(tl)));
                                }
                                Err(e) => {
                                    let _ = tx.send(UiMsg::Err(e));
                                }
                            });
                        }
                        KeyCode::Char('-') => {
                            status.lower_tempo();
                            let src = apply_tempo_override(&editor.content(), status.tempo);
                            let queue = queue.clone();
                            let tx = msg_tx.clone();
                            std::thread::spawn(move || match build_timeline(&src, SAMPLE_RATE) {
                                Ok(tl) => {
                                    let _ = tx.send(UiMsg::Loops(tl.loops.clone()));
                                    let _ = queue.send(Msg::Swap(Arc::new(tl)));
                                }
                                Err(e) => {
                                    let _ = tx.send(UiMsg::Err(e));
                                }
                            });
                        }
                        _ => {}
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
                apply_ui_msg(&mut status, msg);
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
        let tl = build_timeline(src, 48000).unwrap();
        assert!(!tl.events.is_empty());
        assert!(tl.events.iter().all(|e| e.voice == VoiceKind::Kick));
    }

    #[test]
    fn invalid_source_reports_error() {
        let err = build_timeline("loop \"b\":\n    kick << \"x y\"\n", 48000).unwrap_err();
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
    fn apply_ui_msg_sets_loops_from_swap() {
        let mut status = Status::new();
        apply_ui_msg(&mut status, UiMsg::Loops(vec!["b".into(), "h".into()]));
        assert_eq!(status.loops, vec!["b".to_string(), "h".to_string()]);
        assert_eq!(status.error, None);
    }

    #[test]
    fn apply_ui_msg_info_clears_error() {
        let mut status = Status::new();
        status.set_error("boom".into());
        apply_ui_msg(&mut status, UiMsg::Info("exported out.wav".into()));
        assert_eq!(status.error, None);
        assert_eq!(status.message, "exported out.wav");
    }
}
