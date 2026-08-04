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
        let pool = ArrayQueue::new(blocks);
        for _ in 0..blocks {
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

    // audio thread: fill a block from the pool (allocates only if the writer
    // is behind, which never happens in practice)
    pub fn take_pool_block(&self) -> Option<Box<[f32]>> {
        Some(
            self.pool
                .pop()
                .unwrap_or_else(|| vec![0.0f32; self.block_frames * 2].into_boxed_slice()),
        )
    }

    // audio thread: hand a full block to the writer; dropped when full (gap)
    pub fn push_filled(&self, block: Box<[f32]>) {
        let _ = self.filled.push(block);
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
mod tests {
    use super::*;

    #[test]
    fn blocks_flow_through_ring() {
        let rec = Recorder::new(4, 4);
        let mut b = rec.take_pool_block().unwrap();
        b[..8].copy_from_slice(&[0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        rec.push_filled(b);
        let out = rec.take_filled().unwrap();
        assert_eq!(&out[..], &[0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn blocks_are_reused_from_pool() {
        let rec = Recorder::new(2, 2);
        let b = rec.take_pool_block().unwrap();
        rec.push_filled(b);
        let b2 = rec.take_pool_block().unwrap();
        rec.push_filled(b2);
        let got = rec.take_filled().unwrap();
        rec.return_block(got);
        let b3 = rec.take_pool_block().unwrap();
        assert_eq!(b3.len(), 4, "returned block is reusable");
        rec.push_filled(b3);
        assert!(rec.take_filled().is_some());
    }

    #[test]
    fn pool_exhaustion_drops_block() {
        let rec = Recorder::new(1, 2);
        let b = rec.take_pool_block().unwrap();
        rec.push_filled(b);
        let b2 = rec.take_pool_block().unwrap();
        rec.push_filled(b2);
        assert!(rec.take_filled().is_some(), "first block survives");
        assert!(
            rec.take_filled().is_none(),
            "second block dropped when full"
        );
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
