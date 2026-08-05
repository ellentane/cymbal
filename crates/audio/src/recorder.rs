use crossbeam_queue::ArrayQueue;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub struct Recorder {
    filled: ArrayQueue<Box<[f32]>>,
    pool: ArrayQueue<Box<[f32]>>,
    stopped: AtomicBool,
    block_frames: usize,
}

impl Recorder {
    pub fn new(blocks: usize, block_frames: usize) -> Arc<Self> {
        let len = block_frames * 2;
        let pool = ArrayQueue::new(blocks + 8);
        for _ in 0..(blocks + 8) {
            pool.push(vec![0.0f32; len].into_boxed_slice()).unwrap();
        }
        Arc::new(Self {
            filled: ArrayQueue::new(blocks),
            pool,
            stopped: AtomicBool::new(false),
            block_frames,
        })
    }

    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    pub fn take_pool_block(&self) -> Box<[f32]> {
        self.pool.pop().expect("recorder pool must never empty")
    }

    pub fn push_filled(&self, block: Box<[f32]>) {
        if let Err(block) = self.filled.push(block) {
            let _ = self.pool.push(block);
        }
    }

    // writer thread
    pub fn take_filled(&self) -> Option<Box<[f32]>> {
        self.filled.pop()
    }

    // writer thread
    pub fn return_block(&self, block: Box<[f32]>) {
        let _ = self.pool.push(block);
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
impl Recorder {
    pub fn pool_len(&self) -> usize {
        self.pool.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_flow_through_ring() {
        let rec = Recorder::new(4, 4);
        let mut b = rec.take_pool_block();
        b[..8].copy_from_slice(&[0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        rec.push_filled(b);
        let out = rec.take_filled().unwrap();
        assert_eq!(&out[..], &[0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn blocks_are_reused_from_pool() {
        let rec = Recorder::new(2, 2);
        let b = rec.take_pool_block();
        rec.push_filled(b);
        let b2 = rec.take_pool_block();
        rec.push_filled(b2);
        let got = rec.take_filled().unwrap();
        rec.return_block(got);
        let b3 = rec.take_pool_block();
        assert_eq!(b3.len(), 4, "returned block is reusable");
        rec.push_filled(b3);
        assert!(rec.take_filled().is_some());
    }

    #[test]
    fn full_filled_recycles_block() {
        let rec = Recorder::new(1, 2);
        let before = rec.pool_len();
        let b = rec.take_pool_block();
        rec.push_filled(b);
        let b2 = rec.take_pool_block();
        rec.push_filled(b2);
        let out = rec.take_filled().unwrap();
        rec.return_block(out);
        assert_eq!(
            rec.pool_len(),
            before,
            "recycled block returned to the pool"
        );
    }

    #[test]
    fn pool_never_empties_under_stalled_writer() {
        let rec = Recorder::new(4, 4);
        let mut min_pool = usize::MAX;
        for _ in 0..1000 {
            let b = rec.take_pool_block();
            if let Some(f) = rec.take_filled() {
                rec.return_block(f);
            }
            rec.push_filled(b);
            min_pool = min_pool.min(rec.pool_len());
        }
        assert!(min_pool >= 1, "the pool must never empty: {min_pool}");
    }

    #[test]
    fn pool_size_stays_stable() {
        let rec = Recorder::new(4, 4);
        let before = rec.pool_len();
        for _ in 0..100 {
            let b = rec.take_pool_block();
            if let Some(f) = rec.take_filled() {
                rec.return_block(f);
            }
            rec.push_filled(b);
        }
        while let Some(f) = rec.take_filled() {
            rec.return_block(f);
        }
        assert_eq!(rec.pool_len(), before, "pool must not shrink");
    }

    #[test]
    fn stop_flag_lifecycle() {
        let rec = Recorder::new(4, 4);
        assert!(!rec.is_stopped());
        rec.stop();
        assert!(rec.is_stopped());
    }

    #[test]
    fn recorder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Recorder>();
    }
}
