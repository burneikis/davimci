//! Frame pacing against the backend's audio clock (plan.md Phase 9b).
//!
//! Audio is the master clock (spec §10.1), so video is fitted to it rather
//! than the other way round. Two policies, both counted so tests can assert
//! them exactly:
//!
//! - **drop-late**: a decoded frame older than the clock is discarded *when a
//!   newer one is behind it in the queue*. Dropping is how the picture
//!   catches up with the sound, so it only ever skips towards the clock:
//!   the newest frame that is not in the future is always presented, even
//!   when it is itself behind the clock. Discarding it too would leave a
//!   frozen picture over moving audio, since a frame handed over by the
//!   backend is by construction a frame the clock has already passed.
//! - **repeat-on-starve**: when no frame has arrived for the current clock
//!   position, the last presented frame is shown again. A black flash is
//!   worse than a repeated field.
//!
//! A frame pulled ahead of the clock is not thrown away either: it is held
//! as `pending` and offered again on the tick it becomes due, because the
//! backend hands each frame over exactly once.

use davimci_backend::{RenderBackend, VideoFrame};
use davimci_core::Frame;

use crate::error::PresentError;

/// What the pacer decided for one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// A fresh frame is on screen.
    Presented(Frame),
    /// Nothing new was due; the previous frame stays up.
    Repeated(Frame),
    /// Nothing has ever been presented and nothing arrived.
    Empty,
}

/// Pacing counters, for tests and for a future on-screen debug overlay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaceStats {
    pub presented: u64,
    /// Frames decoded but discarded for being behind the clock.
    pub dropped_late: u64,
    /// Ticks where nothing was ready and the last frame was held.
    pub repeated: u64,
}

/// Holds the frame currently on screen and decides what replaces it.
#[derive(Debug, Default)]
pub struct Pacer {
    current: Option<VideoFrame>,
    /// Pulled but not yet due: ahead of the clock when we saw it. Kept
    /// because `next_preview_frame` consumes the queue - dropping a future
    /// frame would lose it for good.
    pending: Option<VideoFrame>,
    stats: PaceStats,
    /// Cap on frames pulled per tick, so a backend that is far ahead cannot
    /// stall the event loop draining its queue.
    max_pulls_per_tick: u32,
}

impl Pacer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            current: None,
            pending: None,
            stats: PaceStats::default(),
            max_pulls_per_tick: 8,
        }
    }

    pub fn set_max_pulls_per_tick(&mut self, n: u32) {
        self.max_pulls_per_tick = n.max(1);
    }

    #[must_use]
    pub fn stats(&self) -> PaceStats {
        self.stats
    }

    /// The frame currently on screen.
    #[must_use]
    pub fn current(&self) -> Option<&VideoFrame> {
        self.current.as_ref()
    }

    /// Put a frame up directly, bypassing pacing - scrubbing and seeking are
    /// not playback and have no clock to pace against.
    pub fn show(&mut self, frame: VideoFrame) {
        self.stats.presented = self.stats.presented.saturating_add(1);
        self.current = Some(frame);
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.pending = None;
    }

    /// One presentation tick. `clock` is the audio clock position; `None`
    /// means the clock is not running, in which case the last frame is held
    /// and nothing is pulled.
    pub fn tick(
        &mut self,
        clock: Option<Frame>,
        backend: &mut dyn RenderBackend,
    ) -> Result<Pace, PresentError> {
        let Some(clock) = clock else {
            return Ok(self.hold());
        };
        // A frame held back from an earlier tick comes first, so ordering is
        // preserved: it is older than anything still in the backend's queue.
        let mut fresh: Option<VideoFrame> = match self.pending.take() {
            Some(f) if f.position <= clock => Some(f),
            Some(f) => {
                // Still in the future. Hold it and pull nothing: the queue
                // behind it is further ahead again.
                self.pending = Some(f);
                return Ok(self.hold());
            }
            None => None,
        };
        for _ in 0..self.max_pulls_per_tick {
            let pulled = backend
                .next_preview_frame()
                .map_err(|e| PresentError::Pull(e.to_string()))?;
            let Some(frame) = pulled else { break };
            if frame.position > clock {
                // Ahead of the clock: not due yet. Hold it for a later tick
                // and stop - the rest of the queue is further ahead again.
                self.pending = Some(frame);
                break;
            }
            // Due, or late. Either way it is the newest candidate so far;
            // anything it replaces was older still and is dropped, which is
            // how the picture catches up with the clock.
            let due_now = frame.position == clock;
            if fresh.replace(frame).is_some() {
                self.stats.dropped_late = self.stats.dropped_late.saturating_add(1);
            }
            if due_now {
                // Exactly the frame the clock is asking for. Anything behind
                // it belongs to a later tick, and draining further would run
                // the picture ahead of the sound.
                break;
            }
        }

        // Never go backwards: a frame older than what is on screen is a late
        // arrival we have already overtaken.
        if let (Some(f), Some(cur)) = (&fresh, &self.current)
            && f.position <= cur.position
        {
            self.stats.dropped_late = self.stats.dropped_late.saturating_add(1);
            fresh = None;
        }

        match fresh {
            Some(frame) => {
                let at = frame.position;
                self.stats.presented = self.stats.presented.saturating_add(1);
                self.current = Some(frame);
                Ok(Pace::Presented(at))
            }
            None => Ok(self.hold()),
        }
    }

    fn hold(&mut self) -> Pace {
        match &self.current {
            Some(f) => {
                self.stats.repeated = self.stats.repeated.saturating_add(1);
                Pace::Repeated(f.position)
            }
            None => Pace::Empty,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_backend::MockBackend;
    use davimci_core::Resolution;

    fn backend() -> MockBackend {
        let mut b = MockBackend::new(Resolution {
            width: 4,
            height: 2,
        });
        b.preview_start(Frame::ZERO, davimci_backend::PreviewScale::Full)
            .unwrap();
        b
    }

    #[test]
    fn a_source_in_step_with_the_clock_never_drops_or_repeats() {
        let mut b = backend();
        let mut p = Pacer::new();
        for i in 0..10 {
            assert_eq!(
                p.tick(Some(Frame(i)), &mut b).unwrap(),
                Pace::Presented(Frame(i))
            );
        }
        assert_eq!(
            p.stats(),
            PaceStats {
                presented: 10,
                dropped_late: 0,
                repeated: 0
            }
        );
    }

    #[test]
    fn a_fast_source_drops_late_frames_to_catch_up() {
        let mut b = backend();
        let mut p = Pacer::new();
        // The clock is four frames ahead of the queue head: the four frames
        // behind it are skipped and the one that is due goes up.
        p.tick(Some(Frame(4)), &mut b).unwrap();
        assert_eq!(p.stats().dropped_late, 4);
        assert_eq!(p.current().unwrap().position, Frame(4));
    }

    /// Regression: frames arrive stamped with the position the clock has
    /// *just* passed, so treating "behind the clock" as "discard" threw away
    /// every frame and left the picture frozen while audio ran on. Dropping
    /// is only ever a skip towards the clock.
    #[test]
    fn a_frame_the_clock_has_already_passed_is_still_shown() {
        let mut b = backend();
        b.preview_budget = Some(1);
        let mut p = Pacer::new();
        assert_eq!(
            p.tick(Some(Frame(3)), &mut b).unwrap(),
            Pace::Presented(Frame(0)),
            "the only frame available was discarded for being late"
        );
    }

    /// Regression: a frame pulled before it was due used to be presented
    /// immediately, running the picture ahead of the sound.
    #[test]
    fn a_frame_ahead_of_the_clock_waits_instead_of_being_shown_early() {
        let mut b = backend();
        let mut p = Pacer::new();
        p.tick(Some(Frame(0)), &mut b).unwrap();
        // Frame 1 is in the queue but the clock has not reached it.
        assert_eq!(
            p.tick(Some(Frame(0)), &mut b).unwrap(),
            Pace::Repeated(Frame(0))
        );
        assert_eq!(p.current().unwrap().position, Frame(0));
        // ...and it is not lost: it goes up on the tick it falls due.
        assert_eq!(
            p.tick(Some(Frame(1)), &mut b).unwrap(),
            Pace::Presented(Frame(1))
        );
    }

    #[test]
    fn the_picture_never_steps_backwards() {
        let mut b = backend();
        let mut p = Pacer::new();
        p.tick(Some(Frame(5)), &mut b).unwrap();
        assert_eq!(p.current().unwrap().position, Frame(5));
        // The queue still holds frame 6 onwards; a clock that went backwards
        // must not pull the picture back with it.
        let pace = p.tick(Some(Frame(2)), &mut b).unwrap();
        assert_eq!(pace, Pace::Repeated(Frame(5)));
    }

    #[test]
    fn a_starved_source_repeats_the_last_frame() {
        let mut b = backend();
        b.preview_budget = Some(1);
        let mut p = Pacer::new();
        assert_eq!(
            p.tick(Some(Frame(0)), &mut b).unwrap(),
            Pace::Presented(Frame(0))
        );
        assert_eq!(
            p.tick(Some(Frame(1)), &mut b).unwrap(),
            Pace::Repeated(Frame(0))
        );
        assert_eq!(
            p.tick(Some(Frame(2)), &mut b).unwrap(),
            Pace::Repeated(Frame(0))
        );
        assert_eq!(p.stats().repeated, 2);
        assert_eq!(p.stats().presented, 1);
    }

    #[test]
    fn nothing_presented_and_nothing_ready_is_empty_not_a_repeat() {
        let mut b = backend();
        b.preview_budget = Some(0);
        let mut p = Pacer::new();
        assert_eq!(p.tick(Some(Frame(0)), &mut b).unwrap(), Pace::Empty);
        assert_eq!(p.stats().repeated, 0);
    }

    #[test]
    fn a_stopped_clock_holds_the_picture_and_pulls_nothing() {
        let mut b = backend();
        let mut p = Pacer::new();
        p.tick(Some(Frame(0)), &mut b).unwrap();
        let before = b.audio_clock_position();
        assert_eq!(p.tick(None, &mut b).unwrap(), Pace::Repeated(Frame(0)));
        assert_eq!(
            b.audio_clock_position(),
            before,
            "a paused pacer pulled a frame"
        );
    }

    #[test]
    fn draining_is_bounded_per_tick() {
        let mut b = backend();
        let mut p = Pacer::new();
        p.set_max_pulls_per_tick(3);
        // The clock is far ahead, so every pulled frame is late; the pacer
        // must still return after three pulls rather than spinning.
        let pace = p.tick(Some(Frame(1_000)), &mut b).unwrap();
        assert_eq!(pace, Pace::Presented(Frame(2)), "the newest of three pulls");
        assert_eq!(p.stats().dropped_late, 2);
    }

    #[test]
    fn scrubbing_shows_a_frame_without_a_clock() {
        let mut b = backend();
        let f = b
            .frame_at(Frame(42), davimci_backend::PreviewScale::Half)
            .unwrap();
        let mut p = Pacer::new();
        p.show(f);
        assert_eq!(p.current().unwrap().position, Frame(42));
    }
}
