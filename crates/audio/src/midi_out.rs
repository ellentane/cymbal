use crossbeam_queue::ArrayQueue;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum MidiItem {
    Note { offset: u64, bytes: [u8; 3] },
    Rebase { offset: u64, tempo: f64 },
    Clock { offset: u64 },
    Sys { bytes: [u8; 3], len: u8 },
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

    pub fn take_clock(&self) -> Option<u64> {
        while let Some(item) = self.tx.pop() {
            if let MidiItem::Clock { offset } = item {
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

fn handle_item(
    item: MidiItem,
    origin: Option<(u64, Instant)>,
    period: Duration,
    now: Instant,
    send: &mut impl FnMut(&[u8]),
) {
    match item {
        MidiItem::Sys { bytes, len } => {
            send(&bytes[..len as usize]);
        }
        MidiItem::Clock { offset } => {
            let Some((o0, t0)) = origin else { return };
            let delay = (offset as f64 - o0 as f64).max(0.0) / 48000.0;
            let target = t0 + Duration::from_secs_f64(delay);
            if now >= target + period {
                return;
            }
            if now < target {
                std::thread::sleep(target - now);
            }
            send(&[0xF8]);
        }
        MidiItem::Note { offset, bytes } => {
            let Some((o0, t0)) = origin else { return };
            let delay = (offset as f64 - o0 as f64).max(0.0) / 48000.0;
            let target = t0 + Duration::from_secs_f64(delay);
            if now < target {
                std::thread::sleep(target - now);
            }
            send(&bytes);
        }
        MidiItem::Rebase { .. } => {}
    }
}

fn writer_loop(tx: Arc<MidiOut>, mut conn: midir::MidiOutputConnection) {
    let mut origin: Option<(u64, Instant)> = None;
    let mut period = Duration::from_millis(500);
    loop {
        let Some(item) = tx.tx.pop() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        match item {
            MidiItem::Rebase { offset, tempo } => {
                origin = Some((offset, Instant::now()));
                period = Duration::from_secs_f64(60.0 / (tempo.max(1.0) * 24.0));
            }
            other => handle_item(other, origin, period, Instant::now(), &mut |b| {
                let _ = conn.send(b);
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn period() -> Duration {
        Duration::from_secs_f64(250.0 / 48000.0)
    }

    fn handle_now(
        item: MidiItem,
        origin: Option<(u64, Instant)>,
        now: Instant,
        sent: &mut Vec<Vec<u8>>,
    ) {
        handle_item(item, origin, period(), now, &mut |b: &[u8]| {
            sent.push(b.to_vec())
        });
    }

    #[test]
    fn sys_sends_immediately() {
        let mut sent = Vec::new();
        handle_now(
            MidiItem::Sys {
                bytes: [0xFA, 0, 0],
                len: 1,
            },
            None,
            Instant::now(),
            &mut sent,
        );
        assert_eq!(sent, vec![vec![0xFA]]);
    }

    #[test]
    fn overdue_clock_is_skipped() {
        let t0 = Instant::now();
        let origin = Some((0u64, t0));
        let mut sent = Vec::new();
        handle_now(
            MidiItem::Clock { offset: 48000 },
            origin,
            t0 + Duration::from_secs(2),
            &mut sent,
        );
        assert!(sent.is_empty(), "overdue clock must be skipped, not burst");
    }

    #[test]
    fn on_time_clock_is_sent() {
        let t0 = Instant::now();
        let origin = Some((0u64, t0));
        let mut sent = Vec::new();
        handle_now(
            MidiItem::Clock { offset: 48000 },
            origin,
            t0 + Duration::from_millis(999),
            &mut sent,
        );
        assert_eq!(sent, vec![vec![0xF8]]);
    }

    #[test]
    fn note_bytes_are_forwarded() {
        let t0 = Instant::now();
        let origin = Some((0u64, t0));
        let mut sent = Vec::new();
        handle_now(
            MidiItem::Note {
                offset: 48000,
                bytes: [0x90, 60, 100],
            },
            origin,
            t0 + Duration::from_secs(1),
            &mut sent,
        );
        assert_eq!(sent, vec![vec![0x90, 60, 100]]);
    }
}
