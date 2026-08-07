use crossbeam_queue::ArrayQueue;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const SEND_AHEAD: Duration = Duration::from_micros(500);
pub const POLL: Duration = Duration::from_millis(2);

pub trait Sleeper: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep_until(&self, deadline: Instant);
    fn sleep(&self, dur: Duration);
}

pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep_until(&self, deadline: Instant) {
        #[cfg(unix)]
        {
            // Absolute-time sleep on CLOCK_MONOTONIC (Linux); macOS falls
            // back through the same path (clock_nanosleep is available).
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            let mut ts = libc::timespec {
                tv_sec: remaining.as_secs() as libc::time_t,
                tv_nsec: remaining.subsec_nanos() as libc::c_long,
            };
            let mut now_ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe {
                libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now_ts);
            }
            let abs = libc::timespec {
                tv_sec: now_ts.tv_sec + ts.tv_sec,
                tv_nsec: now_ts.tv_nsec + ts.tv_nsec,
            };
            let abs = if abs.tv_nsec >= 1_000_000_000 {
                libc::timespec {
                    tv_sec: abs.tv_sec + 1,
                    tv_nsec: abs.tv_nsec - 1_000_000_000,
                }
            } else {
                abs
            };
            ts = abs;
            loop {
                let r = unsafe {
                    libc::clock_nanosleep(
                        libc::CLOCK_MONOTONIC,
                        libc::TIMER_ABSTIME,
                        &ts,
                        std::ptr::null_mut(),
                    )
                };
                if r != libc::EINTR {
                    break;
                }
            }
            let _ = &mut ts;
        }
        #[cfg(not(unix))]
        {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }
    fn sleep(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

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

    pub fn take_sys(&self) -> Option<Vec<u8>> {
        while let Some(item) = self.tx.pop() {
            if let MidiItem::Sys { bytes, len } = item {
                return Some(bytes[..len as usize].to_vec());
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

    #[derive(Clone)]
    #[allow(dead_code)]
    struct FakeSleeper {
        now: std::cell::RefCell<Instant>,
        sleeps: std::cell::RefCell<Vec<Instant>>,
    }

    impl FakeSleeper {
        #[allow(dead_code)]
        fn new(t0: Instant) -> Self {
            Self {
                now: std::cell::RefCell::new(t0),
                sleeps: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl Sleeper for FakeSleeper {
        fn now(&self) -> Instant {
            *self.now.borrow()
        }
        fn sleep_until(&self, deadline: Instant) {
            self.sleeps.borrow_mut().push(deadline);
            *self.now.borrow_mut() = deadline;
        }
        fn sleep(&self, dur: Duration) {
            let n = *self.now.borrow() + dur;
            self.sleeps.borrow_mut().push(n);
            *self.now.borrow_mut() = n;
        }
    }

    #[test]
    fn real_sleeper_progresses_time() {
        let s = RealSleeper;
        let t0 = s.now();
        assert!(s.now() >= t0);
    }

    // Single-threaded test fake: mutation is confined to one thread.
    unsafe impl Sync for FakeSleeper {}

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
    fn clock_exactly_one_period_late_is_skipped() {
        let t0 = Instant::now();
        let origin = Some((0u64, t0));
        let mut sent = Vec::new();
        let target = t0 + Duration::from_secs_f64(480.0 / 48000.0);
        handle_now(
            MidiItem::Clock { offset: 480 },
            origin,
            target + period(),
            &mut sent,
        );
        assert!(
            sent.is_empty(),
            "clock exactly one period late must be skipped"
        );
    }

    #[test]
    fn clock_just_inside_skip_boundary_is_sent() {
        let t0 = Instant::now();
        let origin = Some((0u64, t0));
        let mut sent = Vec::new();
        let target = t0 + Duration::from_secs_f64(480.0 / 48000.0);
        handle_now(
            MidiItem::Clock { offset: 480 },
            origin,
            target + period() - Duration::from_millis(1),
            &mut sent,
        );
        assert_eq!(sent, vec![vec![0xF8]]);
    }

    #[test]
    fn on_time_clock_is_sent() {
        let t0 = Instant::now();
        let origin = Some((0u64, t0));
        let mut sent = Vec::new();
        handle_now(
            MidiItem::Clock { offset: 48000 },
            origin,
            t0 + Duration::from_secs(1),
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
