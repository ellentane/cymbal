#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub tempo: f64,
    pub bar: u64,
    pub loops: Vec<String>,
    pub latency_ms: Option<f32>,
    pub device_rate: Option<u32>,
    pub midi_port: Option<String>,
    pub midi_sending: bool,
    pub recording: bool,
    pub record_elapsed_secs: u64,
    pub error: Option<String>,
    pub message: String,
}

impl Status {
    pub fn new() -> Self {
        Self {
            tempo: 120.0,
            bar: 0,
            loops: Vec::new(),
            latency_ms: None,
            device_rate: None,
            midi_port: None,
            midi_sending: false,
            recording: false,
            record_elapsed_secs: 0,
            error: None,
            message: "Ctrl-S reload | Ctrl-Q quit | Ctrl-=/- tempo".into(),
        }
    }

    pub fn render(&self) -> String {
        let loops = if self.loops.is_empty() {
            "-".to_string()
        } else {
            self.loops.join(", ")
        };
        let lat = self
            .latency_ms
            .map(|ms| format!("~{ms:.1}ms"))
            .unwrap_or_else(|| "-".into());
        let rate = self
            .device_rate
            .map(|r| format!("{:.1}kHz", r as f32 / 1000.0))
            .unwrap_or_else(|| "-".into());
        let rec = if self.recording {
            let mm = self.record_elapsed_secs / 60;
            let ss = self.record_elapsed_secs % 60;
            format!("REC {mm:02}:{ss:02}")
        } else {
            "rec -".into()
        };
        let midi = self
            .midi_port
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| format!("midi {p}"))
            .unwrap_or_else(|| "midi -".into());
        let transport = if self.midi_sending {
            String::from("midi run")
        } else {
            String::from("midi stop")
        };
        let msg = self.error.clone().unwrap_or_else(|| self.message.clone());
        format!(
            "tempo {:.0} | bar {} | loops {} | lat {} | {} | {} | {} | {} | {}",
            self.tempo, self.bar, loops, lat, rate, midi, transport, rec, msg
        )
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    pub fn raise_tempo(&mut self) {
        self.tempo = (self.tempo + 5.0).min(400.0);
    }

    pub fn lower_tempo(&mut self) {
        self.tempo = (self.tempo - 5.0).max(20.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_and_clears_error() {
        let mut s = Status::new();
        s.set_error("bad tempo".into());
        assert_eq!(s.error.as_deref(), Some("bad tempo"));
        s.clear_error();
        assert_eq!(s.error, None);
    }

    #[test]
    fn tempo_render_clamps() {
        let mut s = Status::new();
        assert_eq!(s.tempo, 120.0);
        s.raise_tempo();
        assert_eq!(s.tempo, 125.0);
        s.lower_tempo();
        assert_eq!(s.tempo, 120.0);
    }

    #[test]
    fn render_shows_loops_and_latency() {
        let mut s = Status::new();
        s.loops = vec!["b".into(), "h".into()];
        s.latency_ms = Some(5.3333335);
        assert_eq!(
            s.render(),
            "tempo 120 | bar 0 | loops b, h | lat ~5.3ms | - | midi - | midi stop | rec - | Ctrl-S reload | Ctrl-Q quit | Ctrl-=/- tempo"
        );
    }

    #[test]
    fn render_handles_missing_loops_and_latency() {
        let s = Status::new();
        assert_eq!(
            s.render(),
            "tempo 120 | bar 0 | loops - | lat - | - | midi - | midi stop | rec - | Ctrl-S reload | Ctrl-Q quit | Ctrl-=/- tempo"
        );
    }

    #[test]
    fn render_shows_device_rate() {
        let mut s = Status::new();
        s.device_rate = Some(44100);
        assert!(s.render().contains("44.1kHz"));
    }

    #[test]
    fn render_shows_recording() {
        let mut s = Status::new();
        s.recording = true;
        s.record_elapsed_secs = 12;
        assert!(s.render().contains("REC 00:12"));
    }

    #[test]
    fn render_shows_not_recording() {
        let s = Status::new();
        assert!(s.render().contains("rec -"));
    }

    #[test]
    fn render_shows_midi_port() {
        let mut s = Status::new();
        s.midi_port = Some("UM-1".into());
        assert!(s.render().contains("midi UM-1"));
    }

    #[test]
    fn render_shows_midi_dash_for_empty_port() {
        let mut s = Status::new();
        s.midi_port = Some(String::new());
        assert!(s.render().contains("| midi - |"));
    }

    #[test]
    fn render_shows_midi_transport_state() {
        let mut s = Status::new();
        assert!(s.render().contains("midi stop"));
        s.midi_sending = true;
        assert!(s.render().contains("midi run"));
    }

    #[test]
    fn render_prioritizes_error_over_message() {
        let mut s = Status::new();
        s.set_error("line 2, col 3: parse error".into());
        s.message = "reloading...".into();
        assert!(s.render().ends_with("| line 2, col 3: parse error"));
    }
}
