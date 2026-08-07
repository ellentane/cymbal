use std::sync::Arc;

use crossbeam_queue::ArrayQueue;
use cymbal_core::scheduler::Timeline;

use crate::recorder::Recorder;

#[derive(Debug)]
pub enum Msg {
    Swap(Arc<Timeline>, u64, Vec<Arc<Recorder>>),
    RecordStart {
        master: Arc<Recorder>,
        tracks: Vec<(String, Arc<Recorder>)>,
        spares: Vec<Arc<Recorder>>,
    },
    RecordStop,
    Shutdown,
}

pub struct AudioQueue {
    latest_swap: ArrayQueue<Msg>,
    fifo: ArrayQueue<Msg>,
    retired: ArrayQueue<Arc<Timeline>>,
}

impl AudioQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            latest_swap: ArrayQueue::new(1),
            fifo: ArrayQueue::new(capacity),
            retired: ArrayQueue::new(4),
        }
    }

    pub fn send(&self, msg: Msg) -> Result<(), Msg> {
        match msg {
            Msg::Swap(..) => {
                let _ = self.latest_swap.pop();
                self.latest_swap.push(msg)
            }
            other => self.fifo.push(other),
        }
    }

    pub fn try_recv(&self) -> Option<Msg> {
        self.fifo.pop().or_else(|| self.latest_swap.pop())
    }

    pub fn push_retired(&self, tl: Arc<Timeline>) -> bool {
        self.retired.push(tl).is_ok()
    }

    pub fn take_retired(&self) -> Vec<Arc<Timeline>> {
        let mut out = Vec::new();
        while let Some(tl) = self.retired.pop() {
            out.push(tl);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use cymbal_core::scheduler::Timeline;

    fn tl(generation: u64) -> Arc<Timeline> {
        Arc::new(Timeline {
            events: vec![],
            generation,
            tempo: 120.0,
            bar_samples: 96000,
            sample_rate: 48000,
            loops: vec![],
            loop_generations: vec![],
            midi: vec![],
            window_start: 0,
            window_len: u64::MAX,
        })
    }

    #[test]
    fn sends_and_receives_swap_messages() {
        let q = AudioQueue::new(4);
        q.send(Msg::Swap(tl(1), 1, vec![])).unwrap();
        q.send(Msg::Swap(tl(2), 2, vec![])).unwrap();
        let got = q.try_recv();
        assert!(
            matches!(got, Some(Msg::Swap(tl, _, _)) if tl.generation == 2),
            "first swap is coalesced away"
        );
        assert!(q.try_recv().is_none());
    }

    #[test]
    fn full_queue_rejects() {
        let q = AudioQueue::new(1);
        assert!(q.send(Msg::RecordStop).is_ok());
        assert!(q.send(Msg::Shutdown).is_err());
    }

    #[test]
    fn fifo_order_preserved() {
        let q = AudioQueue::new(8);
        for i in 0..5 {
            q.send(Msg::Swap(tl(i), i, vec![])).unwrap();
        }
        let mut gens = Vec::new();
        while let Some(m) = q.try_recv() {
            if let Msg::Swap(tl, _, _) = m {
                gens.push(tl.generation);
            }
        }
        assert_eq!(gens, vec![4]);
    }

    #[test]
    fn record_messages_are_delivered() {
        use crate::recorder::Recorder;
        let q = AudioQueue::new(4);
        let rec = Recorder::new(2, 2);
        q.send(Msg::RecordStart {
            master: rec,
            tracks: vec![],
            spares: vec![],
        })
        .unwrap();
        q.send(Msg::RecordStop).unwrap();
        assert!(matches!(q.try_recv(), Some(Msg::RecordStart { .. })));
        assert!(matches!(q.try_recv(), Some(Msg::RecordStop)));
    }

    #[test]
    fn shutdown_is_delivered_in_order() {
        let q = AudioQueue::new(4);
        q.send(Msg::Swap(tl(1), 1, vec![])).unwrap();
        q.send(Msg::Shutdown).unwrap();
        q.send(Msg::Swap(tl(2), 2, vec![])).unwrap();
        let mut order = Vec::new();
        while let Some(m) = q.try_recv() {
            order.push(match m {
                Msg::Swap(..) => "swap",
                Msg::RecordStart { .. } | Msg::RecordStop => "record",
                Msg::Shutdown => "shutdown",
            });
        }
        assert_eq!(
            order,
            vec!["shutdown", "swap"],
            "fifo drains before the swap slot"
        );
    }

    #[test]
    fn swap_slot_coalesces() {
        let q = AudioQueue::new(16);
        assert!(q.send(Msg::Swap(tl(1), 1, vec![])).is_ok());
        assert!(q.send(Msg::Swap(tl(2), 2, vec![])).is_ok());
        assert!(q.send(Msg::Swap(tl(3), 3, vec![])).is_ok());
        let mut got = Vec::new();
        while let Some(m) = q.try_recv() {
            if let Msg::Swap(t, _, _) = m {
                got.push(t.generation);
            }
        }
        assert_eq!(got, vec![3], "only the newest swap survives");
    }

    #[test]
    fn fifo_drains_before_swap_slot() {
        let q = AudioQueue::new(16);
        q.send(Msg::Swap(tl(1), 1, vec![])).unwrap();
        q.send(Msg::RecordStop).unwrap();
        let first = q.try_recv().unwrap();
        assert!(matches!(first, Msg::RecordStop), "FIFO drains first");
        assert!(matches!(q.try_recv().unwrap(), Msg::Swap(_, _, _)));
    }

    #[test]
    fn fifo_overflow_reports_failure() {
        let q = AudioQueue::new(1);
        assert!(q.send(Msg::RecordStop).is_ok());
        assert!(q.send(Msg::RecordStop).is_err(), "fifo full -> Err");
    }

    #[test]
    fn retired_timelines_are_drained_in_order() {
        let q = AudioQueue::new(4);
        let a = tl(1);
        let b = tl(2);
        assert!(q.push_retired(a.clone()));
        assert!(q.push_retired(b.clone()));
        let retired = q.take_retired();
        assert_eq!(retired.len(), 2);
        assert!(Arc::ptr_eq(&retired[0], &a));
        assert!(Arc::ptr_eq(&retired[1], &b));
        assert!(q.take_retired().is_empty(), "slot is drained");
    }

    #[test]
    fn retired_overflow_is_bounded() {
        let q = AudioQueue::new(2);
        assert!(q.push_retired(tl(1)));
        assert!(q.push_retired(tl(2)));
        assert!(q.push_retired(tl(3)));
        assert!(q.push_retired(tl(4)));
        assert!(
            !q.push_retired(tl(5)),
            "overflowing the retired slot reports failure instead of allocating"
        );
    }
}
