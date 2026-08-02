//! A deterministic in-memory backend.
//!
//! Every upstream crate tests against this: it decodes nothing, allocates
//! small frames, and produces a colour that is a pure function of the frame
//! number, so a test can assert *which* frame it got from four bytes.

use std::collections::HashMap;
use std::path::Path;

use davimci_core::{Frame, Resolution, Timeline};

use crate::error::{BackendError, Result};
use crate::frame::{PreviewScale, VideoFrame};
use crate::job::{RenderJob, RenderProgress, RenderState};
use crate::{RenderBackend, SourceInfo};

/// The colour a mock frame carries at `position`.
///
/// Distinct for every frame within a 251-frame window on each channel, and
/// the three moduli are coprime, so two different positions inside any
/// realistic test range never collide.
#[must_use]
pub fn mock_signature(position: Frame) -> [u8; 4] {
    let p = position.get();
    [
        (p % 251) as u8,
        ((p.wrapping_mul(7)) % 241) as u8,
        ((p.wrapping_mul(13)) % 239) as u8,
        255,
    ]
}

/// Deterministic [`RenderBackend`] with no media and no window.
#[derive(Debug)]
pub struct MockBackend {
    resolution: Resolution,
    sources: HashMap<String, SourceInfo>,
    timeline_duration: Frame,
    position: Frame,
    previewing: bool,
    /// Frames the preview may still hand out before it starves. `None` is
    /// "never starves"; tests set it to exercise repeat-on-starve pacing.
    pub preview_budget: Option<u64>,
    /// When set, [`RenderBackend::render`] stays `Running` until
    /// [`MockBackend::advance_render`] is called - for cancellation tests.
    pub manual_render: bool,
    progress: RenderProgress,
    /// Every [`RenderBackend::seek`] target, in order.
    pub seeks: Vec<Frame>,
    /// How many times the timeline has been projected.
    pub projections: usize,
    pub last_job: Option<RenderJob>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new(Resolution {
            width: 16,
            height: 8,
        })
    }
}

impl MockBackend {
    #[must_use]
    pub fn new(resolution: Resolution) -> Self {
        Self {
            resolution,
            sources: HashMap::new(),
            timeline_duration: Frame::ZERO,
            position: Frame::ZERO,
            previewing: false,
            preview_budget: None,
            manual_render: false,
            progress: RenderProgress::idle(),
            seeks: Vec::new(),
            projections: 0,
            last_job: None,
        }
    }

    /// Register a file the mock will claim to know about.
    pub fn add_source(&mut self, info: SourceInfo) {
        self.sources.insert(info.path.clone(), info);
    }

    #[must_use]
    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// Advance a manual render by `n` frames, completing it at the total.
    pub fn advance_render(&mut self, n: u64) {
        if self.progress.state != RenderState::Running {
            return;
        }
        self.progress.rendered = (self.progress.rendered + n).min(self.progress.total);
        if self.progress.rendered == self.progress.total {
            self.progress.state = RenderState::Done;
        }
    }

    fn make_frame(&self, position: Frame, scale: PreviewScale) -> VideoFrame {
        let res = scale.apply(self.resolution);
        let sig = mock_signature(position);
        let mut rgba = Vec::with_capacity((res.width * res.height * 4) as usize);
        for _ in 0..(res.width * res.height) {
            rgba.extend_from_slice(&sig);
        }
        VideoFrame {
            position,
            width: res.width,
            height: res.height,
            rgba,
        }
    }
}

impl RenderBackend for MockBackend {
    fn probe(&mut self, path: &Path) -> Result<SourceInfo> {
        let key = path.to_string_lossy().to_string();
        self.sources
            .get(&key)
            .cloned()
            .ok_or(BackendError::Offline { path: key })
    }

    fn set_timeline(&mut self, timeline: &Timeline) -> Result<()> {
        self.projections += 1;
        self.timeline_duration = timeline.duration();
        self.resolution = timeline.props.resolution;
        Ok(())
    }

    fn seek(&mut self, frame: Frame) -> Result<()> {
        self.seeks.push(frame);
        self.position = frame;
        Ok(())
    }

    fn frame_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<VideoFrame> {
        self.position = frame;
        Ok(self.make_frame(frame, scale))
    }

    fn preview_start(&mut self, from: Frame, scale: PreviewScale) -> Result<()> {
        if self.previewing {
            return Err(BackendError::PreviewAlreadyRunning);
        }
        let _ = scale;
        self.previewing = true;
        self.position = from;
        Ok(())
    }

    fn preview_stop(&mut self) -> Result<()> {
        if !self.previewing {
            return Err(BackendError::PreviewNotRunning);
        }
        self.previewing = false;
        Ok(())
    }

    fn is_previewing(&self) -> bool {
        self.previewing
    }

    fn next_preview_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.previewing {
            return Err(BackendError::PreviewNotRunning);
        }
        if let Some(budget) = self.preview_budget.as_mut() {
            if *budget == 0 {
                return Ok(None);
            }
            *budget -= 1;
        }
        let frame = self.make_frame(self.position, PreviewScale::Full);
        self.position = Frame(self.position.get() + 1);
        Ok(Some(frame))
    }

    fn audio_clock_position(&self) -> Option<Frame> {
        self.previewing.then_some(self.position)
    }

    fn render(&mut self, job: RenderJob) -> Result<()> {
        let (start, end) = job.range.unwrap_or((Frame::ZERO, self.timeline_duration));
        if end < start {
            return Err(BackendError::Render {
                reason: format!("the export range {start} to {end} runs backwards"),
            });
        }
        let total = end.get() - start.get();
        self.last_job = Some(job);
        self.progress = RenderProgress {
            state: RenderState::Running,
            rendered: 0,
            total,
        };
        if !self.manual_render {
            self.progress.rendered = total;
            self.progress.state = RenderState::Done;
        }
        Ok(())
    }

    fn progress(&self) -> RenderProgress {
        self.progress.clone()
    }

    fn cancel_render(&mut self) -> Result<()> {
        if self.progress.state == RenderState::Running {
            self.progress.state = RenderState::Cancelled;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_core::testing::fixture;

    #[test]
    fn frames_identify_themselves_by_colour() {
        let mut b = MockBackend::default();
        let a = b.frame_at(Frame(10), PreviewScale::Full).unwrap();
        let c = b.frame_at(Frame(11), PreviewScale::Full).unwrap();
        assert_eq!(a.signature(), mock_signature(Frame(10)));
        assert_ne!(a.signature(), c.signature());
        assert!(a.is_well_formed());
    }

    #[test]
    fn scaled_pull_is_the_same_frame_at_a_smaller_size() {
        let mut b = MockBackend::new(Resolution {
            width: 64,
            height: 32,
        });
        let full = b.frame_at(Frame(3), PreviewScale::Full).unwrap();
        let quarter = b.frame_at(Frame(3), PreviewScale::Quarter).unwrap();
        assert_eq!(quarter.width, 16);
        assert_eq!(quarter.height, 8);
        assert_eq!(full.signature(), quarter.signature());
    }

    #[test]
    fn preview_pulls_are_monotonic_and_never_duplicate() {
        let mut b = MockBackend::default();
        b.preview_start(Frame(5), PreviewScale::Full).unwrap();
        let mut last = None;
        for _ in 0..8 {
            let f = b.next_preview_frame().unwrap().unwrap();
            if let Some(prev) = last {
                assert!(f.position > prev, "presentation time went backwards");
            }
            last = Some(f.position);
        }
        assert_eq!(b.audio_clock_position(), Some(Frame(13)));
        b.preview_stop().unwrap();
        assert!(b.audio_clock_position().is_none());
    }

    #[test]
    fn starving_preview_yields_none_rather_than_a_stale_frame() {
        let mut b = MockBackend {
            preview_budget: Some(1),
            ..MockBackend::default()
        };
        b.preview_start(Frame(0), PreviewScale::Full).unwrap();
        assert!(b.next_preview_frame().unwrap().is_some());
        assert!(b.next_preview_frame().unwrap().is_none());
    }

    #[test]
    fn preview_calls_out_of_order_are_user_errors() {
        let mut b = MockBackend::default();
        assert_eq!(b.preview_stop(), Err(BackendError::PreviewNotRunning));
        assert_eq!(
            b.next_preview_frame().unwrap_err(),
            BackendError::PreviewNotRunning
        );
        b.preview_start(Frame(0), PreviewScale::Full).unwrap();
        assert_eq!(
            b.preview_start(Frame(0), PreviewScale::Full),
            Err(BackendError::PreviewAlreadyRunning)
        );
    }

    #[test]
    fn probing_unknown_media_reports_it_offline() {
        let mut b = MockBackend::default();
        let err = b.probe(Path::new("/nope.mkv")).unwrap_err();
        assert_eq!(
            err,
            BackendError::Offline {
                path: "/nope.mkv".into()
            }
        );
    }

    #[test]
    fn render_reports_progress_and_cancels() {
        let mut b = MockBackend::default();
        let tl = fixture(&[("V1", &[(0, 100, "a")])]);
        b.set_timeline(&tl).unwrap();
        b.manual_render = true;
        b.render(RenderJob::new("/tmp/out.mkv", Default::default()))
            .unwrap();
        assert_eq!(b.progress().total, 100);
        b.advance_render(50);
        assert!((b.progress().fraction() - 0.5).abs() < f32::EPSILON);
        b.cancel_render().unwrap();
        assert_eq!(b.progress().state, RenderState::Cancelled);
    }
}
