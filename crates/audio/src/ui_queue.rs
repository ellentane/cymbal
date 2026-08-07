use crate::recorder::Recorder;
use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

pub enum UiEvent {
    Bar(u64),
    TrackClaimed {
        rec: Arc<Recorder>,
        seq: u64,
        loop_index: u32,
    },
    MidiDropped(u64),
    NeedSegment(u64),
}

pub struct UiQueue {
    inner: ArrayQueue<UiEvent>,
}

impl UiQueue {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: ArrayQueue::new(capacity),
        })
    }

    pub fn try_push(&self, ev: UiEvent) -> bool {
        self.inner.push(ev).is_ok()
    }

    pub fn try_pop(&self) -> Option<UiEvent> {
        self.inner.pop()
    }
}
