//! Playback and shuttle (spec §3.2.1, §15.5; plan.md Phase 9a/9b).
//!
//! Transport is **not** an edit. Nothing here produces a `Command` and
//! nothing reaches the undo log: playing a timeline changes the playhead,
//! which is navigation, exactly like a motion. That is why the whole module
//! talks to `Session::set_playhead` and never to `Session::exec`.
//!
//! Audio is the master clock while playing, so the playhead follows the
//! backend rather than driving it. Shuttling has no audio, so there the
//! playhead leads and every step is an explicit seek.

use davimci_backend::{PreviewScale, RenderBackend};
use davimci_cmd::Session;
use davimci_core::Frame;
use davimci_present::{Presentation, Presenter};

/// What the transport is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Stopped,
    Playing,
    /// Scrubbing at `frames per tick`, negative for backwards. Never zero.
    Shuttling(i32),
}

/// Fastest shuttle step, in frames per tick. `L` doubles up to this and
/// stops there rather than running away from the user.
pub const MAX_SHUTTLE: i32 = 8;

/// Drives preview playback and shuttling against one backend.
#[derive(Debug)]
pub struct Transport {
    state: TransportState,
    /// Where `<Space>p` must come back to when playback stops.
    return_to: Option<Frame>,
    /// Where the current playback was started from, and whether the audio
    /// clock has caught up with it yet. A consumer reports position 0 until
    /// its first frame is shown, so following it blindly makes the playhead
    /// flash to the start of the timeline before jumping back.
    origin: Frame,
    clock_locked: bool,
}

impl Default for Transport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: TransportState::Stopped,
            return_to: None,
            origin: Frame::ZERO,
            clock_locked: false,
        }
    }

    #[must_use]
    pub fn state(&self) -> TransportState {
        self.state
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.state == TransportState::Playing
    }

    /// `<Space><Space>`.
    pub fn play_pause(
        &mut self,
        backend: &mut dyn RenderBackend,
        session: &Session,
        scale: PreviewScale,
    ) -> Result<String, String> {
        match self.state {
            // A shuttle is motion too, so play/pause stops it. With no
            // default stop key (spec §3.2.1) this is the way out of a
            // shuttle other than decelerating through zero.
            TransportState::Playing | TransportState::Shuttling(_) => {
                self.stop(backend, session)?;
                Ok("paused".into())
            }
            _ => {
                self.start(backend, session, scale)?;
                Ok("playing".into())
            }
        }
    }

    /// `<Space>p`: play, then return to where playback started.
    pub fn preview_and_return(
        &mut self,
        backend: &mut dyn RenderBackend,
        session: &Session,
        scale: PreviewScale,
    ) -> Result<String, String> {
        if self.is_playing() {
            self.stop(backend, session)?;
            return Ok("paused".into());
        }
        let from = session.timeline().playhead().frame;
        self.start(backend, session, scale)?;
        self.return_to = Some(from);
        Ok("previewing".into())
    }

    /// `H` / `L`: step the shuttle rate one notch, doubling each press.
    ///
    /// Reversing direction goes through 1x rather than jumping to the mirror
    /// rate, so `H` out of a fast forward shuttle slows down first, as in
    /// every NLE.
    pub fn shuttle(
        &mut self,
        forward: bool,
        backend: &mut dyn RenderBackend,
        session: &Session,
    ) -> Result<String, String> {
        // Shuttling is a scrub, not varispeed playback: `RenderBackend` has
        // no rate control, so audio is stopped rather than pitched.
        if self.is_playing() {
            self.stop(backend, session)?;
        }
        let want = if forward { 1 } else { -1 };
        let rate = match self.state {
            TransportState::Shuttling(r) if r.signum() == want => {
                (r.saturating_mul(2)).clamp(-MAX_SHUTTLE, MAX_SHUTTLE)
            }
            TransportState::Shuttling(r) => {
                let slower = r / 2;
                if slower == 0 { want } else { slower }
            }
            _ => want,
        };
        self.state = TransportState::Shuttling(rate);
        Ok(format!("shuttle {rate:+}x"))
    }

    /// Stop playback because something else wants the playhead (spec §3.2.1).
    ///
    /// Unlike [`Transport::play_pause`] this never toggles, and unlike
    /// [`Transport::preview_and_return`] it *commits*: a motion typed during
    /// `<Space>p` means "go here", so the pending return-to-origin is dropped
    /// rather than fighting the motion for the playhead.
    ///
    /// Returns whether anything was actually running, so the caller can stay
    /// silent when it was not.
    pub fn interrupt(&mut self, backend: &mut dyn RenderBackend) -> Result<bool, String> {
        if self.state == TransportState::Stopped {
            return Ok(false);
        }
        if backend.is_previewing() {
            backend.preview_stop().map_err(|e| e.to_string())?;
        }
        self.state = TransportState::Stopped;
        self.return_to = None;
        Ok(true)
    }

    /// Stop everything, leaving the playhead where it is. Unbound by
    /// default; available for users who map a dedicated stop key.
    pub fn shuttle_stop(
        &mut self,
        backend: &mut dyn RenderBackend,
        session: &Session,
    ) -> Result<String, String> {
        self.stop(backend, session)?;
        Ok("stopped".into())
    }

    fn start(
        &mut self,
        backend: &mut dyn RenderBackend,
        session: &Session,
        scale: PreviewScale,
    ) -> Result<(), String> {
        let from = session.timeline().playhead().frame;
        backend
            .preview_start(from, scale)
            .map_err(|e| e.to_string())?;
        self.state = TransportState::Playing;
        self.return_to = None;
        self.origin = from;
        self.clock_locked = false;
        Ok(())
    }

    /// Stop whatever is running. Idempotent: stopping a stopped transport is
    /// not an error, because `K` is allowed at any time.
    pub fn stop(
        &mut self,
        backend: &mut dyn RenderBackend,
        session: &Session,
    ) -> Result<Option<Frame>, String> {
        let _ = session;
        if backend.is_previewing() {
            backend.preview_stop().map_err(|e| e.to_string())?;
        }
        self.state = TransportState::Stopped;
        Ok(self.return_to.take())
    }

    /// One presentation tick. Returns the frame the playhead should now sit
    /// on, which the caller applies through `Session::set_playhead`.
    ///
    /// Returning the position instead of writing it keeps this module free
    /// of any write access to the session at all.
    pub fn tick(
        &mut self,
        backend: &mut dyn RenderBackend,
        presenter: &mut Presenter,
        session: &Session,
        scale: PreviewScale,
    ) -> TickResult {
        let duration = session.timeline().duration();
        match self.state {
            TransportState::Stopped => TickResult::default(),
            TransportState::Playing => {
                // Exactly one pull per tick: the pacer's drop/repeat counts
                // are only meaningful if a tick means one presentation, so
                // the presentation is handed back rather than left for the
                // caller to fetch with a second `present`.
                let presentation = presenter.present(backend).ok();
                // Ignore the clock until it has reached where playback was
                // started from: before that it is still reporting its
                // pre-roll position, not ours.
                let mut at = backend.audio_clock_position();
                if !self.clock_locked {
                    match at {
                        Some(f) if f >= self.origin => self.clock_locked = true,
                        _ => at = None,
                    }
                }
                // Running off the end stops playback rather than wedging at
                // the last frame.
                if presentation.is_none() || at.is_some_and(|f| f >= duration) {
                    let back = self.stop(backend, session).unwrap_or(None);
                    return TickResult {
                        playhead: Some(back.unwrap_or(duration).min(duration)),
                        stopped: true,
                        presentation,
                    };
                }
                TickResult {
                    playhead: at,
                    stopped: false,
                    presentation,
                }
            }
            TransportState::Shuttling(rate) => {
                let here = session.timeline().playhead().frame;
                let next = if rate >= 0 {
                    here.saturating_add(Frame(rate.unsigned_abs().into()))
                } else {
                    here.saturating_sub(Frame(rate.unsigned_abs().into()))
                };
                let clamped = next.min(duration);
                let at_end = clamped == here || clamped >= duration && rate > 0;
                let mut presentation = None;
                if backend.seek(clamped).is_ok()
                    && let Ok(frame) = backend.frame_at(clamped, scale)
                {
                    presentation = presenter.present_frame(frame).ok();
                }
                if at_end {
                    self.state = TransportState::Stopped;
                    return TickResult {
                        playhead: Some(clamped),
                        stopped: true,
                        presentation,
                    };
                }
                TickResult {
                    playhead: Some(clamped),
                    stopped: false,
                    presentation,
                }
            }
        }
    }
}

/// What one [`Transport::tick`] decided.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickResult {
    /// Where the playhead should be, if it moved.
    pub playhead: Option<Frame>,
    /// True when this tick ended playback.
    pub stopped: bool,
    /// The frame composed by this tick, if one was. Exactly one presentation
    /// per tick, so pacing counters mean what they say.
    pub presentation: Option<Presentation>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_backend::MockBackend;
    use davimci_core::testing::fixture;
    use davimci_core::{Fps, Resolution};
    use davimci_present::Host;

    fn session() -> Session {
        Session::new(fixture(&[("V1", &[(0, 50, "a")])]))
    }

    fn parts() -> (MockBackend, Presenter) {
        (
            MockBackend::new(Resolution {
                width: 4,
                height: 2,
            }),
            Presenter::new(
                Host::Embedded,
                Resolution {
                    width: 8,
                    height: 4,
                },
                Fps::FPS_60,
            ),
        )
    }

    #[test]
    fn interrupt_stops_playback_and_reports_whether_anything_was_running() {
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        // Nothing playing: silent no-op, so a bind never announces a pause
        // that did not happen (spec §3.2.1).
        assert!(!t.interrupt(&mut b).unwrap());
        t.play_pause(&mut b, &s, PreviewScale::Full).unwrap();
        assert!(t.interrupt(&mut b).unwrap());
        assert!(!b.is_previewing());
        assert_eq!(t.state(), TransportState::Stopped);
        // A shuttle is motion too, so it is interrupted the same way.
        t.shuttle(true, &mut b, &s).unwrap();
        assert!(t.interrupt(&mut b).unwrap());
        assert_eq!(t.state(), TransportState::Stopped);
    }

    /// spec §3.2.1: interrupting commits where playback reached, so a
    /// `<Space>p` preview interrupted by a motion does not snap back.
    #[test]
    fn interrupt_discards_a_pending_preview_return() {
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        t.preview_and_return(&mut b, &s, PreviewScale::Full)
            .unwrap();
        assert!(t.interrupt(&mut b).unwrap());
        assert_eq!(t.stop(&mut b, &s).unwrap(), None);
    }

    #[test]
    fn play_pause_toggles_the_backend_preview() {
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        assert_eq!(
            t.play_pause(&mut b, &s, PreviewScale::Full).unwrap(),
            "playing"
        );
        assert!(b.is_previewing());
        assert_eq!(
            t.play_pause(&mut b, &s, PreviewScale::Full).unwrap(),
            "paused"
        );
        assert!(!b.is_previewing());
    }

    #[test]
    fn playback_follows_the_audio_clock() {
        let (mut b, mut p) = parts();
        let s = session();
        let mut t = Transport::new();
        t.play_pause(&mut b, &s, PreviewScale::Full).unwrap();
        let first = t.tick(&mut b, &mut p, &s, PreviewScale::Full);
        assert_eq!(first.playhead, Some(Frame(1)));
        assert!(!first.stopped);
    }

    /// Regression: `Transport::tick` composed a frame *and* `Editor::tick`
    /// composed another, so every tick pulled twice and the pacing counters
    /// reported roughly double what actually happened. One tick is one
    /// presentation; the frame is returned rather than re-fetched.
    #[test]
    fn a_tick_presents_exactly_once() {
        let (mut b, mut p) = parts();
        let s = session();
        let mut t = Transport::new();
        t.play_pause(&mut b, &s, PreviewScale::Full).unwrap();
        for _ in 0..10 {
            let r = t.tick(&mut b, &mut p, &s, PreviewScale::Full);
            assert!(r.presentation.is_some(), "a playing tick composed nothing");
        }
        let stats = p.stats();
        assert_eq!(
            stats.presented + stats.repeated,
            10,
            "a tick presented more than once: {stats:?}"
        );
    }

    /// Regression: the playhead followed the audio clock from the first
    /// tick, but a consumer reports position 0 until its first frame is
    /// shown, so starting playback mid-timeline flashed the playhead to the
    /// start of the track and back.
    #[test]
    fn a_warming_up_clock_does_not_yank_the_playhead_to_zero() {
        let (mut b, mut p) = parts();
        let mut s = session();
        s.set_playhead(Frame(20), s.timeline().playhead().track)
            .unwrap();
        b.clock_warmup = 30;
        let mut t = Transport::new();
        t.play_pause(&mut b, &s, PreviewScale::Full).unwrap();
        let mut seen = Vec::new();
        for _ in 0..20 {
            let r = t.tick(&mut b, &mut p, &s, PreviewScale::Full);
            if let Some(f) = r.playhead {
                seen.push(f);
            }
            if r.stopped {
                break;
            }
        }
        assert!(
            seen.iter().all(|f| *f >= Frame(20)),
            "the playhead jumped behind where playback started: {seen:?}"
        );
    }

    #[test]
    fn playback_stops_at_the_end_of_the_timeline() {
        let (mut b, mut p) = parts();
        let s = session();
        let mut t = Transport::new();
        t.play_pause(&mut b, &s, PreviewScale::Full).unwrap();
        let mut last = TickResult::default();
        for _ in 0..200 {
            last = t.tick(&mut b, &mut p, &s, PreviewScale::Full);
            if last.stopped {
                break;
            }
        }
        assert!(last.stopped, "playback ran past the timeline");
        assert!(!b.is_previewing());
        assert_eq!(t.state(), TransportState::Stopped);
    }

    #[test]
    fn preview_and_return_comes_back_to_where_it_started() {
        let (mut b, mut p) = parts();
        let mut s = session();
        s.set_playhead(Frame(10), s.timeline().playhead().track)
            .unwrap();
        let mut t = Transport::new();
        t.preview_and_return(&mut b, &s, PreviewScale::Full)
            .unwrap();
        let mut last = TickResult::default();
        for _ in 0..200 {
            last = t.tick(&mut b, &mut p, &s, PreviewScale::Full);
            if last.stopped {
                break;
            }
        }
        assert_eq!(last.playhead, Some(Frame(10)), "did not return");
    }

    #[test]
    fn shuttle_doubles_then_clamps() {
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        for expected in [1, 2, 4, 8, 8] {
            t.shuttle(true, &mut b, &s).unwrap();
            assert_eq!(t.state(), TransportState::Shuttling(expected));
        }
    }

    #[test]
    fn reversing_a_shuttle_slows_down_before_it_turns_around() {
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        t.shuttle(true, &mut b, &s).unwrap();
        t.shuttle(true, &mut b, &s).unwrap();
        assert_eq!(t.state(), TransportState::Shuttling(2));
        t.shuttle(false, &mut b, &s).unwrap();
        assert_eq!(t.state(), TransportState::Shuttling(1));
        t.shuttle(false, &mut b, &s).unwrap();
        assert_eq!(t.state(), TransportState::Shuttling(-1));
    }

    #[test]
    fn shuttling_stops_audio_playback_first() {
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        t.play_pause(&mut b, &s, PreviewScale::Full).unwrap();
        t.shuttle(true, &mut b, &s).unwrap();
        assert!(!b.is_previewing(), "shuttle left audio running");
    }

    #[test]
    fn shuttle_steps_the_playhead_and_halts_at_the_end() {
        let (mut b, mut p) = parts();
        let s = session();
        let mut t = Transport::new();
        t.shuttle(true, &mut b, &s).unwrap();
        let r = t.tick(&mut b, &mut p, &s, PreviewScale::Full);
        assert_eq!(r.playhead, Some(Frame(1)));
        // A shuttle backwards from zero has nowhere to go and stops.
        let mut t2 = Transport::new();
        t2.shuttle(false, &mut b, &s).unwrap();
        let r2 = t2.tick(&mut b, &mut p, &s, PreviewScale::Full);
        assert_eq!(r2.playhead, Some(Frame::ZERO));
        assert!(r2.stopped);
    }

    #[test]
    fn play_pause_stops_a_shuttle() {
        // With no default stop binding, `<Space><Space>` must be a way out
        // of a shuttle rather than starting playback from it.
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        t.shuttle(true, &mut b, &s).unwrap();
        t.shuttle(true, &mut b, &s).unwrap();
        t.play_pause(&mut b, &s, PreviewScale::Full).unwrap();
        assert_eq!(t.state(), TransportState::Stopped);
    }

    #[test]
    fn stopping_a_stopped_transport_is_not_an_error() {
        let (mut b, _) = parts();
        let s = session();
        let mut t = Transport::new();
        assert!(t.shuttle_stop(&mut b, &s).is_ok());
        assert!(t.shuttle_stop(&mut b, &s).is_ok());
    }

    #[test]
    fn a_stopped_transport_does_nothing_on_tick() {
        let (mut b, mut p) = parts();
        let s = session();
        let mut t = Transport::new();
        assert_eq!(
            t.tick(&mut b, &mut p, &s, PreviewScale::Full),
            TickResult::default()
        );
        assert!(b.seeks.is_empty());
    }
}
