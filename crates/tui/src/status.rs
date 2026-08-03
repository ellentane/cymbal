#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub tempo: f64,
    pub bar: u64,
    pub error: Option<String>,
    pub message: String,
}

impl Status {
    pub fn new() -> Self {
        Self {
            tempo: 120.0,
            bar: 0,
            error: None,
            message: "Ctrl-S reload | Ctrl-Q quit | Ctrl-=/- tempo".into(),
        }
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
}
