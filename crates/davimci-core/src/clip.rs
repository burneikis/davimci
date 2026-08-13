//! Clips and their non-destructive properties.
//!
//! A clip is a window onto a conformed source: a timeline position, a
//! duration, and an in-point into the source. Gain, fades, and transform are
//! *properties* - they are applied as render-time filters and never mutate
//! media.

use serde::{Deserialize, Serialize};

use crate::id::{ClipId, GroupId};
use crate::time::{Fps, Frame};
use crate::transition::Transition;

/// A reference to media on disk, already conformed to the timeline rate.
///
/// `length` is the source duration expressed in *timeline* frames, so handle
/// arithmetic never leaves the timeline's time base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRef {
    pub path: String,
    /// The source's native rate, kept for re-conform and for export relink.
    pub source_fps: Fps,
    /// Source length in timeline frames after conform.
    pub length: Frame,
    /// offline-media policy: still editable, blocks export.
    pub offline: bool,
    /// Which stream of the container this clip plays, as the demuxer numbers
    /// them. `None` means "the file's default", which is all a single-stream
    /// file ever needs. Import puts every stream on its own track, so a
    /// track that does not name its stream would silently play stream zero.
    #[serde(default)]
    pub stream: Option<u32>,
    /// Channel count of that stream, when it is an audio stream. Export needs
    /// it to route each track to its own stream without guessing where an
    /// upmix put the samples.
    #[serde(default)]
    pub channels: Option<u16>,
}

impl MediaRef {
    #[must_use]
    pub fn new(path: impl Into<String>, source_fps: Fps, length: Frame) -> Self {
        Self {
            path: path.into(),
            source_fps,
            length,
            offline: false,
            stream: None,
            channels: None,
        }
    }

    /// The same reference, bound to one stream of the container.
    #[must_use]
    pub fn on_stream(mut self, stream: u32, channels: Option<u16>) -> Self {
        self.stream = Some(stream);
        self.channels = channels;
        self
    }
}

/// Position/scale/opacity for video and overlay clips.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub opacity: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            opacity: 1.0,
        }
    }
}

/// Non-destructive, render-time clip properties.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipProps {
    pub gain_db: f32,
    pub fade_in: Frame,
    pub fade_out: Frame,
    pub transform: Transform,
}

impl Default for ClipProps {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            fade_in: Frame::ZERO,
            fade_out: Frame::ZERO,
            transform: Transform::default(),
        }
    }
}

/// A clip: a window onto a source, placed on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    /// Short name used in status lines and timeline dumps.
    pub label: String,
    /// `None` for generated clips (text/subtitle entries, colour cards).
    pub media: Option<MediaRef>,
    /// Payload for `text` track clips.
    pub text: Option<String>,
    /// Position on the timeline, in timeline frames.
    pub start: Frame,
    /// Duration in timeline frames. Always non-zero.
    pub duration: Frame,
    /// In-point into the conformed source, in timeline frames.
    pub source_in: Frame,
    /// Per-clip linkage group. `None` means unlinked.
    pub group: Option<GroupId>,
    pub props: ClipProps,
    /// Transition on the cut at this clip's start. It belongs to
    /// the incoming clip so that deleting that clip deletes the transition
    /// with it, rather than leaving one attached to a cut that is gone.
    #[serde(default)]
    pub transition_in: Option<Transition>,
}

impl Clip {
    /// A clip with no media, used for text entries and tests.
    #[must_use]
    pub fn generated(id: ClipId, label: impl Into<String>, start: Frame, duration: Frame) -> Self {
        Self {
            id,
            label: label.into(),
            media: None,
            text: None,
            start,
            duration,
            source_in: Frame::ZERO,
            group: None,
            props: ClipProps::default(),
            transition_in: None,
        }
    }

    #[must_use]
    pub fn from_media(
        id: ClipId,
        label: impl Into<String>,
        media: MediaRef,
        start: Frame,
        source_in: Frame,
        duration: Frame,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            media: Some(media),
            text: None,
            start,
            duration,
            source_in,
            group: None,
            props: ClipProps::default(),
            transition_in: None,
        }
    }

    /// First frame *after* the clip. The timeline is half-open: `[start, end)`.
    #[must_use]
    pub fn end(&self) -> Frame {
        Frame(self.start.get() + self.duration.get())
    }

    #[must_use]
    pub fn contains(&self, frame: Frame) -> bool {
        frame >= self.start && frame < self.end()
    }

    /// In-point of the frame after the clip's last source frame.
    #[must_use]
    pub fn source_out(&self) -> Frame {
        Frame(self.source_in.get() + self.duration.get())
    }

    /// Frames available before the in-point. `None` means unbounded
    /// (generated clips have no source to run out of).
    #[must_use]
    pub fn head_handle(&self) -> Option<u64> {
        self.media.as_ref().map(|_| self.source_in.get())
    }

    /// Frames available after the out-point. `None` means unbounded.
    #[must_use]
    pub fn tail_handle(&self) -> Option<u64> {
        self.media
            .as_ref()
            .map(|m| m.length.get().saturating_sub(self.source_out().get()))
    }

    #[must_use]
    pub fn is_offline(&self) -> bool {
        self.media.as_ref().is_some_and(|m| m.offline)
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the values under test are set exactly, so exact equality is the assertion"
)]
mod tests {
    use super::*;

    fn media_clip() -> Clip {
        Clip::from_media(
            ClipId(1),
            "a",
            MediaRef::new("/x.mkv", Fps::FPS_60, Frame(300)),
            Frame(100),
            Frame(50),
            Frame(100),
        )
    }

    #[test]
    fn extent_is_half_open() {
        let c = media_clip();
        assert_eq!(c.end(), Frame(200));
        assert!(c.contains(Frame(100)));
        assert!(c.contains(Frame(199)));
        assert!(!c.contains(Frame(200)));
        assert!(!c.contains(Frame(99)));
    }

    #[test]
    fn handles_come_from_the_source_length() {
        let c = media_clip();
        assert_eq!(c.head_handle(), Some(50));
        assert_eq!(c.tail_handle(), Some(150));
    }

    #[test]
    fn generated_clips_have_unbounded_handles() {
        let c = Clip::generated(ClipId(2), "t", Frame::ZERO, Frame(10));
        assert_eq!(c.head_handle(), None);
        assert_eq!(c.tail_handle(), None);
        assert!(!c.is_offline());
    }

    #[test]
    fn properties_default_to_neutral() {
        let p = ClipProps::default();
        assert_eq!(p.gain_db, 0.0);
        assert_eq!(p.fade_in, Frame::ZERO);
        assert_eq!(p.transform.scale, 1.0);
        assert_eq!(p.transform.opacity, 1.0);
    }
}
