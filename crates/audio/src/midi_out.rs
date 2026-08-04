use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

pub enum MidiItem {
    Note { offset: u64, bytes: [u8; 3] },
    Rebase { offset: u64, tempo: f64 },
}

pub struct MidiOut {
    tx: ArrayQueue<MidiItem>,
}

impl MidiOut {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            tx: ArrayQueue::new(capacity),
        })
    }

    pub fn try_send(&self, item: MidiItem) -> bool {
        self.tx.push(item).is_ok()
    }

    pub fn take_note(&self) -> Option<[u8; 3]> {
        while let Some(item) = self.tx.pop() {
            if let MidiItem::Note { bytes, .. } = item {
                return Some(bytes);
            }
        }
        None
    }

    pub fn take_rebase_offset(&self) -> Option<u64> {
        while let Some(item) = self.tx.pop() {
            if let MidiItem::Rebase { offset, .. } = item {
                return Some(offset);
            }
        }
        None
    }

    /// True if a port named `port_name` (or any port, if empty) exists.
    pub fn port_available(port_name: &str) -> bool {
        use midir::MidiOutput;
        let Ok(midi_out) = MidiOutput::new("cymbal") else {
            return false;
        };
        let ports = midi_out.ports();
        if port_name.is_empty() {
            !ports.is_empty()
        } else {
            ports
                .iter()
                .any(|p| midi_out.port_name(p).ok().as_deref() == Some(port_name))
        }
    }

    /// Writer thread: opens the port inside the thread (no Send requirement on
    /// the connection) and streams note/clock messages. If the port is gone
    /// the thread exits silently; audio is unaffected.
    pub fn spawn_writer(self: Arc<Self>, port_name: &str) -> std::thread::JoinHandle<()> {
        let tx = self.clone();
        let port_name = port_name.to_string();
        std::thread::spawn(move || {
            use midir::MidiOutput;
            let Ok(midi_out) = MidiOutput::new("cymbal") else {
                return;
            };
            let ports = midi_out.ports();
            let port = if port_name.is_empty() {
                ports.into_iter().next()
            } else {
                ports
                    .into_iter()
                    .find(|p| midi_out.port_name(p).ok().as_deref() == Some(port_name.as_str()))
            };
            let Some(port) = port else { return };
            let Ok(conn) = midi_out.connect(&port, "cymbal-out") else {
                return;
            };
            writer_loop(tx, conn);
        })
    }
}

fn writer_loop(tx: Arc<MidiOut>, mut conn: midir::MidiOutputConnection) {
    use std::time::{Duration, Instant};
    let mut origin_offset: Option<u64> = None;
    let mut origin_time: Option<Instant> = None;
    let mut next_pulse: Option<Instant> = None;
    let mut pulse_period = Duration::from_millis(500);
    loop {
        if let Some(item) = tx.tx.pop() {
            match item {
                MidiItem::Rebase { offset, tempo } => {
                    origin_offset = Some(offset);
                    origin_time = Some(Instant::now());
                    pulse_period = Duration::from_secs_f64(60.0 / (tempo * 24.0));
                    next_pulse = Some(Instant::now() + pulse_period);
                }
                MidiItem::Note { offset, bytes } => {
                    let target = match (origin_offset, origin_time) {
                        (Some(o0), Some(t0)) => {
                            let delay = (offset as f64 - o0 as f64).max(0.0) / 48000.0;
                            t0 + Duration::from_secs_f64(delay)
                        }
                        _ => Instant::now(),
                    };
                    loop {
                        let now = Instant::now();
                        if now >= target {
                            break;
                        }
                        let wake = match next_pulse {
                            Some(p) => target.min(p),
                            None => target,
                        };
                        if wake > now {
                            std::thread::sleep(wake - now);
                        }
                        if let Some(next) = next_pulse
                            && Instant::now() >= next
                        {
                            let _ = conn.send(&[0xF8]);
                            next_pulse = Some(next + pulse_period);
                        }
                    }
                    let _ = conn.send(&bytes);
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(next) = next_pulse {
            let now = Instant::now();
            if now >= next {
                let _ = conn.send(&[0xF8]);
                next_pulse = Some(next + pulse_period);
            }
        }
    }
}
