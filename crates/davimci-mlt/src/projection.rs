//! Timeline -> MLT graph projection, as pure data.
//!
//! This module contains no MLT types and does no I/O: it turns a
//! [`Timeline`] into the *shape* the render graph must have - one playlist
//! per track, blanks for gaps, one entry per clip with its render-time
//! filters. `xml` serialises this shape for the golden tests, and `patch`
//! diffs two of them so an edit becomes playlist mutations rather than a
//! rebuild (spec §10.1).

use davimci_core::{Clip, ClipId, Frame, Timeline, TimelineProps, TrackId, TrackKind};

/// What an entry plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// A conformed media file on disk.
    File(String),
    /// A generated text/subtitle clip (spec §8).
    Text(String),
    /// Offline media: renders as a placeholder so the project stays editable
    /// while export stays blocked (Phase 0).
    Offline { path: String },
    /// A generated clip with no media and no text - a colour card.
    Colour,
}

impl Resource {
    /// The MLT service that plays this resource.
    #[must_use]
    pub fn service(&self) -> &'static str {
        match self {
            Self::File(_) => "avformat",
            Self::Text(_) => "qtext",
            Self::Offline { .. } | Self::Colour => "color",
        }
    }

    /// The MLT `resource` property.
    #[must_use]
    pub fn resource(&self) -> String {
        match self {
            Self::File(p) => p.clone(),
            // The placeholder is deliberately not black: offline media must
            // be visible as a fault, not mistaken for a gap.
            Self::Offline { .. } => "#ff202080".into(),
            Self::Text(_) | Self::Colour => "#ff000000".into(),
        }
    }
}

/// A render-time filter attached to one entry. Never destructive (spec §6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterSpec {
    pub service: String,
    pub props: Vec<(String, String)>,
}

/// One clip, projected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipEntry {
    pub clip: ClipId,
    pub label: String,
    pub resource: Resource,
    /// Inclusive in-point into the source, in timeline frames.
    pub in_point: Frame,
    /// **Inclusive** out-point: MLT's `out` is the last frame, not one past
    /// it, which is the one off-by-one this whole layer exists to contain.
    pub out_point: Frame,
    pub filters: Vec<FilterSpec>,
}

impl ClipEntry {
    /// Number of frames the entry occupies on the timeline.
    #[must_use]
    pub fn length(&self) -> u64 {
        self.out_point.get() + 1 - self.in_point.get()
    }
}

/// One playlist slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Blank { length: Frame },
    Clip(Box<ClipEntry>),
}

impl Entry {
    #[must_use]
    pub fn length(&self) -> u64 {
        match self {
            Self::Blank { length } => length.get(),
            Self::Clip(c) => c.length(),
        }
    }

    /// Filters on this entry; a blank has none.
    #[must_use]
    pub fn filters(&self) -> &[FilterSpec] {
        match self {
            Self::Blank { .. } => &[],
            Self::Clip(c) => &c.filters,
        }
    }

    #[must_use]
    pub fn clip_id(&self) -> Option<ClipId> {
        match self {
            Self::Blank { .. } => None,
            Self::Clip(c) => Some(c.clip),
        }
    }
}

/// One track, projected onto an MLT playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackProjection {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub muted: bool,
    pub entries: Vec<Entry>,
}

impl TrackProjection {
    /// MLT's `hide` bitmask: 1 hides video, 2 hides audio.
    #[must_use]
    pub fn hide(&self) -> u8 {
        match (self.kind, self.muted) {
            // Audio tracks contribute no video, ever.
            (TrackKind::Audio, false) => 1,
            (TrackKind::Audio, true) => 3,
            (_, true) => 2,
            _ => 0,
        }
    }
}

/// The whole timeline, projected onto a tractor of playlists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    pub props: TimelineProps,
    pub tracks: Vec<TrackProjection>,
}

impl Projection {
    /// Project a timeline. Pure: same timeline in, same projection out.
    #[must_use]
    pub fn of(timeline: &Timeline) -> Self {
        let tracks = timeline
            .tracks()
            .iter()
            .map(|track| {
                let mut entries = Vec::new();
                let mut cursor = Frame::ZERO;
                for clip in track.clips() {
                    if clip.start > cursor {
                        entries.push(Entry::Blank {
                            length: clip.start.saturating_sub(cursor),
                        });
                    }
                    entries.push(Entry::Clip(Box::new(project_clip(clip, track.kind))));
                    cursor = clip.end();
                }
                TrackProjection {
                    id: track.id,
                    name: track.name.clone(),
                    kind: track.kind,
                    muted: track.muted || soloed_out(timeline, track.id),
                    entries,
                }
            })
            .collect();
        Self {
            props: timeline.props,
            tracks,
        }
    }

    /// Whether two projections describe the same set of playlists in the same
    /// order. A false here means the graph must be rebuilt rather than patched.
    #[must_use]
    pub fn same_shape(&self, other: &Self) -> bool {
        self.props == other.props
            && self.tracks.len() == other.tracks.len()
            && self
                .tracks
                .iter()
                .zip(&other.tracks)
                .all(|(a, b)| a.id == b.id && a.kind == b.kind)
    }
}

/// Solo is exclusive across the timeline: any solo mutes every non-solo track.
fn soloed_out(timeline: &Timeline, track: TrackId) -> bool {
    let any_solo = timeline.tracks().iter().any(|t| t.solo);
    any_solo && !timeline.tracks().iter().any(|t| t.id == track && t.solo)
}

fn project_clip(clip: &Clip, kind: TrackKind) -> ClipEntry {
    let resource = match (&clip.media, &clip.text) {
        (Some(m), _) if m.offline => Resource::Offline {
            path: m.path.clone(),
        },
        (Some(m), _) => Resource::File(m.path.clone()),
        (None, Some(t)) => Resource::Text(t.clone()),
        (None, None) => Resource::Colour,
    };
    ClipEntry {
        clip: clip.id,
        label: clip.label.clone(),
        resource,
        in_point: clip.source_in,
        // Half-open [start, end) on our side, inclusive `out` on MLT's.
        out_point: Frame(clip.source_in.get() + clip.duration.get() - 1),
        filters: filters_for(clip, kind),
    }
}

/// Clip properties become filters; the media is never touched (spec §6.1).
fn filters_for(clip: &Clip, kind: TrackKind) -> Vec<FilterSpec> {
    let mut out = Vec::new();
    let props = &clip.props;
    let last = clip.duration.get().saturating_sub(1);

    if props.gain_db != 0.0 {
        out.push(FilterSpec {
            service: "volume".into(),
            props: vec![("level".into(), format!("{:.4}", props.gain_db))],
        });
    }

    // Fades are envelopes, so they are one animated property rather than a
    // pair of in/out filters that would fight over the same value.
    if props.fade_in.get() > 0 {
        let end = props.fade_in.get().saturating_sub(1).min(last);
        out.push(fade_filter(kind, fade_value(kind, 0, end, true)));
    }
    if props.fade_out.get() > 0 {
        let start = last.saturating_sub(props.fade_out.get().saturating_sub(1));
        out.push(fade_filter(kind, fade_value(kind, start, last, false)));
    }

    let t = props.transform;
    if kind != TrackKind::Audio
        && (t.x != 0.0 || t.y != 0.0 || (t.scale - 1.0).abs() > f32::EPSILON)
    {
        out.push(FilterSpec {
            service: "qtblend".into(),
            props: vec![(
                "rect".into(),
                format!(
                    "{:.4} {:.4} {:.4}% {:.4}% 1",
                    t.x,
                    t.y,
                    t.scale * 100.0,
                    t.scale * 100.0
                ),
            )],
        });
    }
    if (t.opacity - 1.0).abs() > f32::EPSILON && kind != TrackKind::Audio {
        out.push(FilterSpec {
            service: "brightness".into(),
            props: vec![("alpha".into(), format!("{:.4}", t.opacity))],
        });
    }
    out
}

fn fade_filter(kind: TrackKind, animation: String) -> FilterSpec {
    let (service, prop) = match kind {
        TrackKind::Audio => ("volume", "level"),
        _ => ("brightness", "alpha"),
    };
    FilterSpec {
        service: service.into(),
        props: vec![(prop.into(), animation)],
    }
}

/// An MLT animation string for a fade. Audio fades in decibels, video in
/// normalised alpha, so the endpoints differ by medium.
fn fade_value(kind: TrackKind, from: u64, to: u64, rising: bool) -> String {
    let (silent, full) = match kind {
        TrackKind::Audio => ("-60", "0"),
        _ => ("0", "1"),
    };
    if rising {
        format!("{from}={silent};{to}={full}")
    } else {
        format!("{from}={full};{to}={silent}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_core::ClipProps;
    use davimci_core::testing::fixture;

    #[test]
    fn gaps_become_blanks_and_clips_keep_frame_counts() {
        let tl = fixture(&[("V1", &[(0, 100, "a"), (150, 50, "b")])]);
        let p = Projection::of(&tl);
        let v1 = &p.tracks[0];
        assert_eq!(v1.entries.len(), 3);
        assert_eq!(v1.entries[0].length(), 100);
        assert!(matches!(v1.entries[1], Entry::Blank { length } if length == Frame(50)));
        assert_eq!(v1.entries[2].length(), 50);
    }

    #[test]
    fn out_point_is_inclusive() {
        let tl = fixture(&[("V1", &[(0, 1, "a")])]);
        let p = Projection::of(&tl);
        let Entry::Clip(c) = &p.tracks[0].entries[0] else {
            panic!("expected a clip");
        };
        assert_eq!(c.in_point, Frame(0));
        assert_eq!(c.out_point, Frame(0), "a one-frame clip is in=out=0");
        assert_eq!(c.length(), 1);
    }

    #[test]
    fn offline_media_projects_to_a_visible_placeholder() {
        let mut tl = davimci_core::testing::media_fixture(&[(0, 10, 0, 100)]);
        let clip = tl.tracks()[0].clips()[0].id;
        tl.set_media_offline(clip, true).unwrap();
        let p = Projection::of(&tl);
        let Entry::Clip(c) = &p.tracks[0].entries[0] else {
            panic!("expected a clip");
        };
        assert_eq!(
            c.resource,
            Resource::Offline {
                path: "/media/0.mkv".into()
            }
        );
        assert_eq!(c.resource.service(), "color");
    }

    #[test]
    fn audio_tracks_never_contribute_video() {
        let tl = fixture(&[("V1", &[(0, 10, "a")]), ("A1", &[(0, 10, "b")])]);
        let p = Projection::of(&tl);
        assert_eq!(p.tracks[0].hide(), 0, "V1 shows video");
        assert_eq!(p.tracks[1].hide(), 1, "A1 contributes no video");
    }

    #[test]
    fn solo_mutes_every_other_track() {
        let mut tl = fixture(&[("A1", &[(0, 10, "a")]), ("A2", &[(0, 10, "b")])]);
        let a1 = davimci_core::testing::track_id(&tl, "A1");
        tl.set_track_solo(a1, true).unwrap();
        let p = Projection::of(&tl);
        let by = |name: &str| {
            p.tracks
                .iter()
                .find(|t| t.name == name)
                .expect("track present")
        };
        assert!(!by("A1").muted);
        assert!(by("A2").muted, "a solo elsewhere mutes this track");
        assert_eq!(by("A2").hide(), 3);
    }

    #[test]
    fn gain_becomes_a_volume_filter_and_zero_gain_becomes_nothing() {
        let mut tl = fixture(&[("A1", &[(0, 10, "a")])]);
        let track = davimci_core::testing::track_id(&tl, "A1");
        let clip = davimci_core::testing::clip_ids(&tl, "A1")[0];
        let a1_index = 1;
        assert!(
            Projection::of(&tl).tracks[a1_index].entries[0]
                .filters()
                .is_empty()
        );
        tl.set_clip_props(
            track,
            clip,
            ClipProps {
                gain_db: -6.0,
                ..ClipProps::default()
            },
        )
        .unwrap();
        let p = Projection::of(&tl);
        let f = p.tracks[a1_index].entries[0].filters();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].service, "volume");
        assert_eq!(f[0].props[0].1, "-6.0000");
    }

    #[test]
    fn fades_project_as_envelopes_over_the_clip() {
        let mut tl = fixture(&[("A1", &[(0, 100, "a")])]);
        let track = davimci_core::testing::track_id(&tl, "A1");
        let clip = davimci_core::testing::clip_ids(&tl, "A1")[0];
        tl.set_clip_props(
            track,
            clip,
            ClipProps {
                fade_in: Frame(10),
                fade_out: Frame(20),
                ..ClipProps::default()
            },
        )
        .unwrap();
        let p = Projection::of(&tl);
        let f = p.tracks[1].entries[0].filters();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].props[0].1, "0=-60;9=0");
        assert_eq!(f[1].props[0].1, "80=0;99=-60");
    }
}
