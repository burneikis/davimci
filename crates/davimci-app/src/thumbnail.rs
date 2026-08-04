//! Clip thumbnails for the timeline.
//!
//! A clip is drawn as a filmstrip: several pictures across its width, each
//! one of the media at *that* point, so a strip reads as the shot changing
//! rather than as one frame stamped repeatedly. Each picture is therefore
//! identified by the source frame it shows, which is what keeps it valid
//! while a clip is moved and what makes it stale when the clip is slipped.
//!
//! Decoding needs a backend, which this crate does not have, so thumbnails
//! arrive the way waveforms and job progress do: the app says which pictures
//! are missing, and the host publishes them whenever it has them.

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
    /// The source frame this pictures.
    pub source: Frame,
}

impl std::fmt::Debug for Thumbnail {
    /// The pixels are never printed: a `Debug` of a thumbnail in a status
    /// line or a test failure has to stay readable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbnail")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source", &self.source)
            .finish()
    }
}

impl Thumbnail {
    #[must_use]
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, source: Frame) -> Self {
        Self {
            width,
            height,
            rgba: Arc::new(rgba),
            source,
        }
    }

    /// Whether the buffer matches the declared size.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.rgba.len() == (self.width as usize) * (self.height as usize) * 4
    }
}

/// One picture the app would like: a point in a clip, named both in timeline
/// time (what the backend seeks to) and in source time (what identifies the
/// picture afterwards).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailRequest {
    pub clip: ClipId,
    /// Timeline frame to decode - inside the clip, so it pictures the clip
    /// rather than whatever precedes it.
    pub at: Frame,
    /// The source frame `at` resolves to, carried back on the published
    /// thumbnail so the picture can be recognised later.
    pub source: Frame,
}

/// Every published thumbnail, by clip and source frame. Absent means "not
/// decoded yet", drawn as a plain clip rather than as a black one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Thumbnails {
    pictures: HashMap<(ClipId, Frame), Thumbnail>,
}

impl Thumbnails {
    pub fn insert(&mut self, clip: ClipId, thumbnail: Thumbnail) {
        self.pictures.insert((clip, thumbnail.source), thumbnail);
    }

    /// The picture of `source` in `clip`, if one has been decoded.
    #[must_use]
    pub fn get(&self, clip: ClipId, source: Frame) -> Option<&Thumbnail> {
        self.pictures
            .get(&(clip, source))
            .filter(|t| t.is_well_formed())
    }

    /// Forget every picture not in `keep`.
    ///
    /// The app asks for exactly the pictures it would draw, so anything else
    /// is a clip that scrolled away, was deleted, or was trimmed until this
    /// frame is no longer the one under that column - pixels nobody will
    /// look at again.
    pub fn retain(&mut self, keep: &[(ClipId, Frame)]) {
        self.pictures.retain(|key, _| keep.contains(key));
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pictures.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pictures.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(source: u64) -> Thumbnail {
        Thumbnail::new(2, 2, vec![0u8; 16], Frame(source))
    }

    #[test]
    fn a_picture_is_found_by_the_source_frame_it_shows() {
        let mut t = Thumbnails::default();
        t.insert(ClipId(1), thumb(0));
        t.insert(ClipId(1), thumb(30));
        assert!(t.get(ClipId(1), Frame(0)).is_some());
        assert!(t.get(ClipId(1), Frame(30)).is_some());
        assert!(
            t.get(ClipId(1), Frame(60)).is_none(),
            "a frame nobody decoded must not answer with another one"
        );
        assert!(t.get(ClipId(2), Frame(0)).is_none());
    }

    #[test]
    fn a_malformed_thumbnail_is_never_handed_out() {
        let mut t = Thumbnails::default();
        t.insert(ClipId(1), Thumbnail::new(4, 4, vec![0u8; 3], Frame(0)));
        assert!(t.get(ClipId(1), Frame(0)).is_none());
    }

    #[test]
    fn retain_drops_pictures_that_are_no_longer_wanted() {
        let mut t = Thumbnails::default();
        t.insert(ClipId(1), thumb(0));
        t.insert(ClipId(1), thumb(30));
        t.insert(ClipId(2), thumb(0));
        t.retain(&[(ClipId(1), Frame(30))]);
        assert_eq!(t.len(), 1);
        assert!(t.get(ClipId(1), Frame(30)).is_some());
    }
}
