pub const MAX_SEGMENT_RETRIES: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentAction {
    None,
    /// Spawn a segment for `end`. `retries > 0` marks a retry of a failed
    /// request for the same end (the caller reports it).
    Spawn {
        end: u64,
        retries: u32,
    },
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredRequest {
    end: u64,
    retry: bool,
}

/// Pure state machine for the streaming-segment pipeline. The engine's
/// NeedSegment is authoritative: it fires exactly once per applied swap with
/// the engine's real current window end, so a request is never dropped here.
/// When it cannot be dispatched (segment or reload in flight) it is stored in
/// a single slot — newest wins, matching the engine's one-shot latch — and
/// re-evaluated when the pipeline clears. Over-dispatch is harmless: a swap
/// sent behind a reload is rejected by the sender's seq guard.
#[derive(Debug)]
pub struct SegmentScheduler {
    last_window_end: u64,
    stored: Option<StoredRequest>,
    segment_in_flight: bool,
    reload_in_flight: bool,
    segment_retries: u32,
    last_segment_seq: u64,
}

impl SegmentScheduler {
    pub fn new(last_window_end: u64) -> Self {
        Self {
            last_window_end,
            stored: None,
            segment_in_flight: false,
            reload_in_flight: false,
            segment_retries: 0,
            last_segment_seq: 0,
        }
    }

    #[cfg(test)]
    pub fn last_window_end(&self) -> u64 {
        self.last_window_end
    }

    pub fn is_current(&self, seq: u64) -> bool {
        seq == self.last_segment_seq
    }

    fn dispatch_if_ready(&mut self) -> SegmentAction {
        if self.segment_in_flight || self.reload_in_flight {
            return SegmentAction::None;
        }
        let Some(req) = self.stored.take() else {
            return SegmentAction::None;
        };
        let retries = if req.retry { self.segment_retries } else { 0 };
        SegmentAction::Spawn {
            end: req.end,
            retries,
        }
    }

    /// The engine applied a window ending at `end` and asks for the next one.
    /// Store it (newest wins) and dispatch if the pipeline is idle.
    pub fn on_need_segment(&mut self, end: u64) -> SegmentAction {
        self.segment_retries = 0;
        self.stored = Some(StoredRequest { end, retry: false });
        self.dispatch_if_ready()
    }

    /// A segment attempt with `seq` was spawned.
    pub fn note_spawn(&mut self, seq: u64) {
        self.segment_in_flight = true;
        self.last_segment_seq = seq;
    }

    /// `end` is the applied window end (Some) or failure (None).
    /// `superseded` means a newer reload was dispatched after this segment
    /// started (its swap may never be applied), so `end` must not advance
    /// `last_window_end` — the engine's own NeedSegment carries the truth.
    pub fn on_segment_done(
        &mut self,
        end: Option<u64>,
        seq: u64,
        superseded: bool,
    ) -> SegmentAction {
        self.segment_in_flight = false;
        match end {
            Some(end) => {
                self.segment_retries = 0;
                if !superseded {
                    self.last_window_end = end;
                }
            }
            None => {
                if seq == self.last_segment_seq && self.stored.is_none() {
                    if self.segment_retries < MAX_SEGMENT_RETRIES {
                        self.segment_retries += 1;
                        self.stored = Some(StoredRequest {
                            end: self.last_window_end,
                            retry: true,
                        });
                    } else {
                        return SegmentAction::Error(
                            "segment production failed repeatedly; press Ctrl-S to restart the timeline"
                                .into(),
                        );
                    }
                }
            }
        }
        self.dispatch_if_ready()
    }

    pub fn on_reload_started(&mut self) {
        self.reload_in_flight = true;
    }

    /// A reload settled: applied with `window_end` (Some if accepted, so it
    /// becomes the applied window end) or failed (None, engine windowless).
    /// Re-evaluate any stored request.
    pub fn on_reload_settled(&mut self, window_end: Option<u64>) -> SegmentAction {
        self.reload_in_flight = false;
        if let Some(end) = window_end {
            self.last_window_end = end;
        }
        self.dispatch_if_ready()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(action: SegmentAction) -> (u64, u32) {
        match action {
            SegmentAction::Spawn { end, retries } => (end, retries),
            other => panic!("expected spawn, got {other:?}"),
        }
    }

    fn none(action: SegmentAction) {
        assert_eq!(action, SegmentAction::None);
    }

    fn error(action: SegmentAction) {
        match action {
            SegmentAction::Error(_) => {}
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn superseded_segment_done_must_not_poison_the_gate() {
        // The permanent-stall interleaving: NeedSegment(300) dispatches a
        // segment; a reload supersedes it; the segment's stale SegmentDone
        // would have advanced last_window_end to 600 and the engine's own
        // NeedSegment(300) would then have been dropped as "stale".
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        s.on_reload_started();
        none(s.on_segment_done(Some(600), 2, true));
        assert_eq!(s.last_window_end(), 300, "superseded done must not advance");
        // the reload applied; the engine latches its one-shot request
        none(s.on_need_segment(300));
        // Reloaded processing settles the reload: the stored request dispatches
        assert_eq!(spawn(s.on_reload_settled(Some(300))), (300, 0));
    }

    #[test]
    fn steady_state_pipeline() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        none(s.on_segment_done(Some(600), 2, false));
        assert_eq!(s.last_window_end(), 600);
        assert_eq!(spawn(s.on_need_segment(600)), (600, 0));
        s.note_spawn(3);
        none(s.on_segment_done(Some(900), 3, false));
        assert_eq!(s.last_window_end(), 900);
    }

    #[test]
    fn superseded_success_does_not_advance_but_unrelated_success_does() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        none(s.on_segment_done(Some(600), 2, false));
        assert_eq!(s.last_window_end(), 600);
        assert_eq!(spawn(s.on_need_segment(600)), (600, 0));
        s.note_spawn(3);
        none(s.on_segment_done(Some(900), 3, true));
        assert_eq!(s.last_window_end(), 600);
    }

    #[test]
    fn request_during_flight_is_deferred_and_dispatched_when_segment_clears() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        // a reload applied while our segment was in flight: its request lands
        // in the stored slot
        none(s.on_need_segment(300));
        assert_eq!(spawn(s.on_segment_done(Some(600), 2, true)), (300, 0));
        assert_eq!(s.last_window_end(), 300);
    }

    #[test]
    fn newer_request_overwrites_stored_request() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        none(s.on_need_segment(300));
        none(s.on_need_segment(600));
        assert_eq!(spawn(s.on_segment_done(Some(600), 2, true)), (600, 0));
    }

    #[test]
    fn failure_retries_bounded_and_deferred_while_reload_in_flight() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        s.on_reload_started();
        // failure during a reload: retry stored, not dispatched
        none(s.on_segment_done(None, 2, false));
        // reload failed: the engine never re-fires; the retry must go out
        assert_eq!(spawn(s.on_reload_settled(None)), (300, 1));
        s.note_spawn(3);
        assert_eq!(spawn(s.on_segment_done(None, 3, false)), (300, 2));
        s.note_spawn(4);
        error(s.on_segment_done(None, 4, false));
    }

    #[test]
    fn retry_deferred_until_reload_settles_on_success() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        s.on_reload_started();
        none(s.on_segment_done(None, 2, false));
        assert_eq!(spawn(s.on_reload_settled(Some(300))), (300, 1));
    }

    #[test]
    fn fresh_engine_request_supersedes_deferred_retry_and_resets_budget() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(2);
        s.on_reload_started();
        none(s.on_segment_done(None, 2, false));
        // the reload applies and latches its own request: fresher than the
        // deferred retry, so the settle dispatches it as a fresh request
        none(s.on_need_segment(300));
        assert_eq!(spawn(s.on_reload_settled(Some(300))), (300, 0));
    }

    #[test]
    fn stale_failure_does_not_retry() {
        let mut s = SegmentScheduler::new(300);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
        s.note_spawn(4);
        none(s.on_segment_done(None, 2, false));
        assert_eq!(s.last_window_end(), 300);
    }

    #[test]
    fn need_segment_is_authoritative_over_local_bookkeeping() {
        // Even if TUI bookkeeping drifted, the engine's end wins: over-dispatch
        // is harmless because the sender's seq guard rejects stale swaps.
        let mut s = SegmentScheduler::new(600);
        assert_eq!(spawn(s.on_need_segment(300)), (300, 0));
    }

    #[test]
    fn request_while_reload_in_flight_is_deferred_until_settle() {
        let mut s = SegmentScheduler::new(300);
        s.on_reload_started();
        none(s.on_need_segment(300));
        assert_eq!(spawn(s.on_reload_settled(Some(300))), (300, 0));
    }
}
