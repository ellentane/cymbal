use crate::ast::{Program, Stmt};

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub name: &'static str,
    pub description: &'static str,
    pub example: &'static str,
}

pub struct Section {
    pub title: &'static str,
    pub entries: &'static [Entry],
}

pub const PARAM_NAMES: &[&str] = &[
    "pan", "vel", "delay", "reverb", "bass", "treble", "comp", "swing",
    "start", "end", "dur", "cycle",
];

pub const VOICE_NAMES: &[&str] = &["kick", "snare", "hat", "bass", "lead", "sample"];

const PARAMS: &[Entry] = &[
    Entry { name: "pan", description: "stereo position, -1 (left) to 1 (right); also accepts a ramp a..b across the bar", example: "pan=-0.4" },
    Entry { name: "vel", description: "scales hit velocity, 0..=1; multiplies the per-hit '*' velocity", example: "vel=0.9" },
    Entry { name: "delay", description: "delay send, 0..=1; accepts a ramp", example: "delay=0.25" },
    Entry { name: "reverb", description: "reverb send, 0..=1; accepts a ramp", example: "reverb=0.4" },
    Entry { name: "bass", description: "per-voice low shelf, 0..=1", example: "bass=0.5" },
    Entry { name: "treble", description: "per-voice high shelf, 0..=1", example: "treble=0.5" },
    Entry { name: "comp", description: "per-voice compressor, 0..=1", example: "comp=0.3" },
    Entry { name: "swing", description: "delays the odd 8th steps, 0..=0.5", example: "swing=0.25" },
    Entry { name: "start", description: "sample region start (0..=1), sample voices only", example: "start=0.25" },
    Entry { name: "end", description: "sample region end (0..=1), sample voices only", example: "end=0.75" },
    Entry { name: "dur", description: "trigger length in seconds (0..=60), sample voices only", example: "dur=0.2" },
    Entry { name: "cycle", description: "loop the sample region for the trigger duration (0 or 1), sample voices only", example: "cycle=1" },
];

const SYMBOLS: &[Entry] = &[
    Entry { name: "<<", description: "send a pattern to a voice", example: "kick << \"x . x .\"" },
    Entry { name: "|", description: "chain a transform onto a pattern; combine freely", example: "\"x . x .\" | rev" },
    Entry { name: "*", description: "per-hit velocity scale, 0..=1", example: "x*0.5" },
    Entry { name: "+", description: "transpose a hit up N semitones", example: "x+2" },
    Entry { name: "-", description: "transpose a hit down N semitones", example: "x-2" },
    Entry { name: "..", description: "a ramp from a to b across each bar, in a param", example: "pan=-0.5..0.5" },
    Entry { name: "[ ]", description: "a note array — evenly spaced triggers, one pitch each", example: "[c4, d4, e4, g4]" },
    Entry { name: " ", description: "a rhythm pattern — x hits, . rests, length is the subdivision", example: "\"x . . x\"" },
    Entry { name: "=", description: "parameter assignment", example: "vel=0.9" },
    Entry { name: ":", description: "loop header terminator", example: "loop \"beat\":" },
    Entry { name: ",", description: "separates list items and every(n, rev) arguments", example: "[c4, d4] every(4, rev)" },
    Entry { name: "--", description: "comment to end of line", example: "-- tempo 120" },
    Entry { name: "x", description: "a hit inside a rhythm pattern", example: "x" },
    Entry { name: ".", description: "a rest inside a rhythm pattern", example: "." },
];

const KEYWORDS: &[Entry] = &[
    Entry { name: "let", description: "define a voice by name; names bind (declared wins over built-ins)", example: "let kick = kick()" },
    Entry { name: "loop", description: "open a repeating loop block; binds live inside it", example: "loop \"beat\":" },
    Entry { name: "tempo", description: "set the transport tempo, 20..=4000", example: "tempo 120" },
    Entry { name: "sample", description: "load a sample voice from a WAV file or the bundled kit", example: "let clap = sample \"clap\"" },
    Entry { name: "every", description: "apply a transform on every nth cycle", example: "| every(4, rev)" },
    Entry { name: "rev", description: "reverse a pattern's steps", example: "| rev" },
    Entry { name: "help", description: "show help for a topic, or the whole cheat sheet", example: "help pan" },
];

const VOICES: &[Entry] = &[
    Entry { name: "kick", description: "kick drum voice", example: "kick()" },
    Entry { name: "snare", description: "snare voice", example: "snare()" },
    Entry { name: "hat", description: "hi-hat voice", example: "hat()" },
    Entry { name: "bass", description: "bass voice", example: "bass()" },
    Entry { name: "lead", description: "lead voice", example: "lead()" },
    Entry { name: "sample", description: "sample voice from a WAV file or the bundled kit", example: "sample \"kick\"" },
];

const SECTIONS: &[Section] = &[
    Section { title: "Symbols", entries: SYMBOLS },
    Section { title: "Params", entries: PARAMS },
    Section { title: "Keywords", entries: KEYWORDS },
    Section { title: "Voices", entries: VOICES },
];

pub fn sections() -> &'static [Section] {
    SECTIONS
}

pub fn lookup(topic: &str) -> Option<&'static Entry> {
    let t = topic.trim_matches('"');
    for section in SECTIONS {
        if let Some(e) = section.entries.iter().find(|e| e.name == t) {
            return Some(e);
        }
    }
    None
}

/// Closest candidate within 2 edits; prefix-superset candidates win
/// (del -> delay over vel), then shortest distance, then shortest name.
pub fn nearest<'a>(candidates: &[&'a str], input: &str) -> Option<&'a str> {
    let mut best: Option<(usize, bool, &'a str)> = None; // (distance, prefix, name)
    for c in candidates {
        let d = levenshtein(c, input);
        if d > 2 {
            continue;
        }
        let prefix = c.starts_with(input);
        let better = match best {
            None => true,
            Some((bd, bp, bc)) => (!prefix, d, c.len()) < (!bp, bd, bc.len()),
        };
        if better {
            best = Some((d, prefix, c));
        }
    }
    best.map(|(_, _, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.chars().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != cb))
                    .min(cur[j] + 1)
                    .min(prev[j + 1] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn help_text(program: &Program) -> Option<String> {
    for stmt in &program.statements {
        if let Stmt::Help(topic, _) = stmt {
            return Some(match topic {
                Some(t) => {
                    let t = t.trim_matches('"');
                    match lookup(t) {
                        Some(e) => format!(
                            "{} — {}\n  example: {}",
                            e.name, e.description, e.example
                        ),
                        None => format!("no help for '{t}'"),
                    }
                }
                None => render_all(),
            });
        }
    }
    None
}

pub fn help_text_src(src: &str) -> Option<String> {
    let tokens = crate::lexer::lex(src).ok()?;
    let program = crate::parser::parse(&tokens).ok()?;
    help_text(&program)
}

fn render_all() -> String {
    let mut out = String::new();
    for section in SECTIONS {
        out.push_str(&format!("{}\n", section.title));
        for e in section.entries {
            out.push_str(&format!("  {} — {} ({})\n", e.name, e.description, e.example));
        }
    }
    out
}

pub fn markdown() -> String {
    let mut out = String::from("# cymbal cheat sheet\n\n");
    for section in SECTIONS {
        out.push_str(&format!("## {}\n\n", section.title));
        for e in section.entries {
            out.push_str(&format!("- `{}` — {}. Example: `{}`\n", e.name, e.description, e.example));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Program, Stmt};
    use crate::error::Span;

    #[test]
    fn lookup_finds_every_topic_class() {
        assert!(lookup("pan").is_some());
        assert!(lookup("vel").is_some());
        assert!(lookup("swing").is_some());
        assert!(lookup("|").is_some());
        assert!(lookup("..").is_some());
        assert!(lookup("<<").is_some());
        assert!(lookup(":").is_some());
        assert!(lookup(",").is_some());
        assert!(lookup("kick").is_some());
        assert!(lookup("help").is_some());
        assert!(lookup("vol").is_none());
    }

    #[test]
    fn every_entry_resolves() {
        for section in SECTIONS {
            for e in section.entries {
                assert!(
                    lookup(e.name).is_some(),
                    "entry '{}' must be findable via lookup",
                    e.name
                );
            }
        }
    }

    #[test]
    fn bass_resolves_to_params_not_voices() {
        let e = lookup("bass").unwrap();
        assert_eq!(e.example, "bass=0.5");
    }

    #[test]
    fn nearest_suggests_typo_fixes() {
        assert_eq!(nearest(PARAM_NAMES, "vol"), Some("vel"));
        assert_eq!(nearest(PARAM_NAMES, "del"), Some("delay"));
        assert_eq!(nearest(PARAM_NAMES, "pan"), Some("pan"));
        assert_eq!(nearest(VOICE_NAMES, "snaree"), Some("snare"));
        assert_eq!(nearest(PARAM_NAMES, "xyzzy"), None);
    }

    #[test]
    fn help_text_resolves_topics() {
        let program = Program {
            statements: vec![Stmt::Help(Some("pan".into()), Span { line: 1, col: 1 })],
        };
        let text = help_text(&program).unwrap();
        assert!(text.contains("pan"));
        assert!(text.contains("example"));

        let all = Program {
            statements: vec![Stmt::Help(None, Span { line: 1, col: 1 })],
        };
        let text = help_text(&all).unwrap();
        assert!(text.contains("Symbols"));
        assert!(text.contains("Params"));

        let none = Program { statements: vec![] };
        assert!(help_text(&none).is_none());
    }

    #[test]
    fn help_text_accepts_quoted_glyphs() {
        let program = Program {
            statements: vec![Stmt::Help(Some("\"*\"".into()), Span { line: 1, col: 1 })],
        };
        assert!(help_text(&program).unwrap().contains('*'));
    }

    #[test]
    fn markdown_covers_all_params() {
        let md = markdown();
        for p in PARAM_NAMES {
            assert!(md.contains(p), "markdown must document {p}");
        }
    }

    #[test]
    fn name_constants_match_tables() {
        let param_entries: Vec<&str> = PARAMS.iter().map(|e| e.name).collect();
        let voice_entries: Vec<&str> = VOICES.iter().map(|e| e.name).collect();
        for p in PARAM_NAMES {
            assert!(param_entries.contains(p), "PARAM_NAMES '{p}' missing from PARAMS table");
        }
        for v in VOICE_NAMES {
            assert!(voice_entries.contains(v), "VOICE_NAMES '{v}' missing from VOICES table");
        }
    }
}
