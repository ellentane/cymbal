use cymbal_core::docs;

const KEYBINDINGS: &[(&str, &str)] = &[
    ("Ctrl-S", "reload — only changed loops rebuild"),
    ("Ctrl-= / Ctrl--", "raise / lower tempo (full reload)"),
    (
        "Alt-R / Alt-H / Alt-[ / Alt-]",
        "reverse / half-speed / rotate the line",
    ),
    ("Ctrl-R", "toggle recording"),
    ("Ctrl-J", "MIDI start/stop"),
    ("Ctrl-E", "export to out.wav"),
    ("F1 / Esc", "toggle this panel"),
    ("Tab", "autocomplete (Tab cycles, Enter accepts)"),
    ("Ctrl-Q", "quit"),
];

/// The param `name=value` pair whose text spans `col`, if any.
pub fn highlight_topic(line: &str, col: usize) -> Option<&'static str> {
    let byte_idx = line
        .char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let mut rest = &line[..byte_idx];
    while let Some(pos) = rest.rfind('=') {
        let name_end = pos;
        let name_start = rest[..name_end]
            .rfind(|c: char| !c.is_ascii_alphanumeric())
            .map_or(0, |i| i + 1);
        let name = match rest.get(name_start..name_end) {
            Some(name) => name,
            None => break,
        };
        if let Some(static_name) = docs::PARAM_NAMES.iter().copied().find(|n| *n == name) {
            return Some(static_name);
        }
        rest = &rest[..name_start.saturating_sub(1)];
    }
    None
}

pub fn help_panel_text(cursor: Option<(&str, usize)>) -> String {
    let mut out = String::new();
    if let Some((line, col)) = cursor
        && let Some(topic) = highlight_topic(line, col)
        && let Some(e) = docs::lookup(topic)
    {
        out.push_str(&format!(
            "context: {} — {} ({})\n",
            e.name, e.description, e.example
        ));
    }
    out.push_str("Help — F1 to close, arrows scroll\n");
    for section in docs::sections() {
        out.push_str(&format!("{}\n", section.title));
        for e in section.entries {
            out.push_str(&format!(
                "  {} — {} ({})\n",
                e.name, e.description, e.example
            ));
        }
    }
    out.push_str("Keybindings\n");
    for (key, action) in KEYBINDINGS {
        out.push_str(&format!("  {key} — {action}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_renders_sections_and_keybindings() {
        let text = help_panel_text(None);
        assert!(text.contains("Symbols"));
        assert!(text.contains("Params"));
        assert!(text.contains("Keybindings"));
        assert!(text.contains("<<"));
        assert!(text.contains("Ctrl-S"));
    }

    #[test]
    fn cursor_column_selects_param() {
        let line = "    kick << \"x . x .\" vel=0.9 pan=-0.4";
        assert_eq!(highlight_topic(line, 30), Some("vel"));
        assert_eq!(highlight_topic(line, 40), Some("pan"));
        let line = "    kick << \"x . x .\"";
        assert_eq!(highlight_topic(line, 5), None);
    }

    #[test]
    fn highlighted_topic_is_prepended() {
        let text = help_panel_text(Some(("    kick << \"x\" vel=0.5", 21)));
        assert!(text.starts_with("context: vel"));
    }

    #[test]
    fn unicode_before_param_does_not_panic() {
        let line = "-- note: φ=golden";
        assert!(highlight_topic(line, 20).is_none());
    }
}
