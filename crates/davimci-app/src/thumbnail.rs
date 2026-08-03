//! Clip thumbnails for the timeline (idea.md, spec §15.2).
//!
//! A thumbnail is a picture of a *source* frame, so it stays valid while a
//! clip is moved and goes stale when the clip's in-point changes. Decoding
//! one needs a backend, which this crate does not have, so thumbnails arrive
//! the way waveforms and job progress do: the app says which clips are
//! missing one, and the host publishes pictures whenever it has them.

use std::collections::HashMap;
use std::sync::Arc;

use davimci_core::{ClipId, Frame};

/// A decoded thumbnail: small, RGBA8, tightly packed.
///
/// The pixels are shared rather than copied: a view is assembled every frame
/// and a thumbnail must not be reallocated 60 times a second.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Arc<Vec<u8>>,
    /// The source frame this pictures, so a trim or a slip can tell that the
    /// picture is now of the wrong part of the media.
    pub source_in: Frame,
}

impl std::fmt::Debug for Thumbnail {
    /// The pixels are never printed: a `Debug` of a thumbnail in a status
    /// line or a test failure has to stay readable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbnail")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source_in", &self.source_in)
            .finish()
    }
}

impl Thumbnail {
    #[must_use]
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, source_in: Frame) -> Self {
        Self {
            width,
            height,
            rgba: Arc::new(rgba),
            source_in,
        }
    }

    /// Whether the buffer matches the declared size.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.rgba.len() == (self.width as usize) * (self.height as usize) * 4
    }
}

/// What the app would like a picture of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailRequest {
    pub clip: ClipId,
    /// Timeline frame to decode - inside the clip, so it pictures the clip
    /// rather than whatever precedes it.
    pub at: Frame,
    /// The clip's in-point, carried back on the published thumbnail so a
    /// stale picture is recognisable.
    pub source_in: Frame,
}

/// Every published thumbnail. Absent means "not decoded yet", drawn as a
/// plain clip rather than as a black one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Thumbnails {
    clips: HashMap<ClipId, Thumbnail>,
}

impl Thumbnails {
    pub fn insert(&mut self, clip: ClipId, thumbnail: Thumbnail) {
        self.clips.insert(clip, thumbnail);
    }

    /// The picture for a clip, if there is a current one. A thumbnail whose
    /// `source_in` no longer matches the clip's is stale - the clip was
    /// trimmed or slipped, so the picture is of the wrong frame.
    #[must_use]
    pub fn get(&self, clip: ClipId, source_in: Frame) -> Option<&Thumbnail> {
        self.clips
            .get(&clip)
            .filter(|t| t.source_in == source_in && t.is_well_formed())
    }

    pub fn remove(&mut self, clip: ClipId) {
        self.clips.remove(&clip);
    }

    /// Forget everything not in `keep` - clips deleted by an edit or by a
    /// timeline swap must not hold their pixels forever.
    pub fn retain(&mut self, keep: &[ClipId]) {
        self.clips.retain(|id, _| keep.contains(id));
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.clips.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(source_in: u64) -> Thumbnail {
        Thumbnail::new(2, 2, vec![0u8; 16], Frame(source_in))
    }

    #[test]
    fn a_thumbnail_of_a_different_in_point_is_stale() {
        let mut t = Thumbnails::default();
        t.insert(ClipId(1), thumb(0));
        assert!(t.get(ClipId(1), Frame(0)).is_some());
        assert!(
            t.get(ClipId(1), Frame(30)).is_none(),
            "a trimmed clip must not keep showing its old frame"
        );
    }

    #[test]
    fn a_malformed_thumbnail_is_never_handed_out() {
        let mut t = Thumbnails::default();
        t.insert(ClipId(1), Thumbnail::new(4, 4, vec![0u8; 3], Frame(0)));
        assert!(t.get(ClipId(1), Frame(0)).is_none());
    }

    #[test]
    fn retain_drops_clips_that_are_gone() {
        let mut t = Thumbnails::default();
        t.insert(ClipId(1), thumb(0));
        t.insert(ClipId(2), thumb(0));
        t.retain(&[ClipId(2)]);
        assert_eq!(t.len(), 1);
        assert!(t.get(ClipId(1), Frame(0)).is_none());
    }
}
