//! A presenter host that writes frames to memory.
//!
//! Used by tests and by the parity harness: it is the same [`Presenter`] the
//! GUI and TUI drive, so what it records is what they draw.

use davimci_backend::{RenderBackend, VideoFrame};
use davimci_core::{Fps, Resolution};

use crate::error::PresentError;
use crate::presenter::{Host, Presentation, Presenter};

/// Records every presentation instead of showing it.
#[derive(Debug)]
pub struct HeadlessPresenter {
    presenter: Presenter,
    frames: Vec<Presentation>,
    /// Cap on retained presentations, so a long playback test does not grow
    /// without bound.
    capacity: usize,
}

impl HeadlessPresenter {
    #[must_use]
    pub fn new(surface: Resolution, fps: Fps) -> Self {
        Self {
            presenter: Presenter::new(Host::Embedded, surface, fps),
            frames: Vec::new(),
            capacity: 4096,
        }
    }

    #[must_use]
    pub fn with_host(host: Host, surface: Resolution, fps: Fps) -> Self {
        Self {
            presenter: Presenter::new(host, surface, fps),
            frames: Vec::new(),
            capacity: 4096,
        }
    }

    pub fn presenter_mut(&mut self) -> &mut Presenter {
        &mut self.presenter
    }

    #[must_use]
    pub fn presenter(&self) -> &Presenter {
        &self.presenter
    }

    pub fn tick(&mut self, backend: &mut dyn RenderBackend) -> Result<(), PresentError> {
        let p = self.presenter.present(backend)?;
        self.record(p);
        Ok(())
    }

    pub fn show(&mut self, frame: VideoFrame) -> Result<(), PresentError> {
        let p = self.presenter.present_frame(frame)?;
        self.record(p);
        Ok(())
    }

    fn record(&mut self, p: Presentation) {
        if self.frames.len() == self.capacity {
            self.frames.remove(0);
        }
        self.frames.push(p);
    }

    #[must_use]
    pub fn frames(&self) -> &[Presentation] {
        &self.frames
    }

    #[must_use]
    pub fn last(&self) -> Option<&Presentation> {
        self.frames.last()
    }

    /// The sequence of timeline positions actually shown, which is what a
    /// pacing test asserts on.
    #[must_use]
    pub fn positions(&self) -> Vec<Option<davimci_core::Frame>> {
        self.frames.iter().map(|f| f.position).collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_backend::{MockBackend, PreviewScale};
    use davimci_core::Frame;

    #[test]
    fn playback_records_one_presentation_per_tick() {
        let mut b = MockBackend::new(Resolution {
            width: 4,
            height: 2,
        });
        b.preview_start(Frame::ZERO, PreviewScale::Full).unwrap();
        let mut h = HeadlessPresenter::new(
            Resolution {
                width: 8,
                height: 4,
            },
            Fps::FPS_60,
        );
        for _ in 0..5 {
            h.tick(&mut b).unwrap();
        }
        assert_eq!(h.frames().len(), 5);
        assert_eq!(
            h.positions(),
            (0..5).map(|i| Some(Frame(i))).collect::<Vec<_>>()
        );
    }
}
