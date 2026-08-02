#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transport {
    pub tempo: f64,
    pub sample_rate: u32,
}

impl Transport {
    pub const SAMPLE_RATE: u32 = 48000;

    pub fn new(tempo: f64, sample_rate: u32) -> Self {
        Self {
            tempo: tempo.max(1.0),
            sample_rate,
        }
    }

    pub fn beat_samples(&self) -> u64 {
        ((60.0 / self.tempo) * self.sample_rate as f64).round() as u64
    }

    pub fn bar_samples(&self) -> u64 {
        self.beat_samples() * 4
    }

    pub fn note_frequency(midi: u8) -> f64 {
        440.0 * 2.0f64.powf((midi as f64 - 69.0) / 12.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_and_bar_samples() {
        let t = Transport::new(120.0, 48000);
        assert_eq!(t.beat_samples(), 24000);
        assert_eq!(t.bar_samples(), 96000);
    }

    #[test]
    fn bar_samples_round() {
        let t = Transport::new(90.0, 48000);
        assert_eq!(t.beat_samples(), 32000);
        assert_eq!(t.bar_samples(), 128000);
    }

    #[test]
    fn note_frequency() {
        assert!((Transport::note_frequency(69) - 440.0).abs() < 1e-9);
        assert!((Transport::note_frequency(60) - 261.6256).abs() < 0.001);
    }
}
