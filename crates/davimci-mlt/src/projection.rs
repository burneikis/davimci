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

/// Which stream of a container an entry plays.
///
/// A multi-stream file becomes one track per stream (spec §7), so an entry
/// that does not name its stream would play the demuxer's default and every
/// audio track would carry the same samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSelect {
    Audio(u32),
    Video(u32),
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
    /// Which stream of the resource to decode, when the resource has more
    /// than one that could match.
    pub stream: Option<StreamSelect>,
    /// Channel count of the selected audio stream, when known. Export routing
    /// needs it to know where the samples land after an upmix.
    pub channels: Option<u16>,
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

/// Where one audio track's samples sit in the export channel bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRoute {
    pub track_index: usize,
    pub start: u16,
    pub channels: u16,
}

/// The channel bus an export uses to keep audio tracks apart.
///
/// The tractor mixes tracks into one frame, so "one stream per track" is
/// really "one channel range per track": each track is routed to its own
/// range before the mix, and the consumer cuts the bus back up into streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioLayout {
    pub total_channels: u16,
    pub routes: Vec<AudioRoute>,
}

/// Channels given to each audio track on the export bus.
pub const CHANNELS_PER_TRACK: u16 = 2;

/// The most audio streams the avformat consumer can be told to write.
pub const MAX_AUDIO_STREAMS: usize = 8;

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

    /// Indexes of the tracks whose audio has to be summed into the mix.
    ///
    /// Track 0 is the accumulator, so every other track needs one `mix`
    /// transition against it; without them a tractor plays the audio of one
    /// track only.
    #[must_use]
    pub fn audio_mix_tracks(&self) -> Vec<usize> {
        (1..self.tracks.len()).collect()
    }

    /// The channel bus for a separate-stream export, or `None` when there is
    /// nothing to keep apart.
    #[must_use]
    pub fn audio_layout(&self) -> Option<AudioLayout> {
        let audio: Vec<usize> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Audio)
            .map(|(i, _)| i)
            .collect();
        if audio.len() < 2 || audio.len() > MAX_AUDIO_STREAMS {
            return None;
        }
        let routes = audio
            .iter()
            .enumerate()
            .map(|(n, &track_index)| AudioRoute {
                track_index,
                start: n as u16 * CHANNELS_PER_TRACK,
                channels: CHANNELS_PER_TRACK,
            })
            .collect::<Vec<_>>();
        Some(AudioLayout {
            total_channels: routes.len() as u16 * CHANNELS_PER_TRACK,
            routes,
        })
    }

    /// Route every audio track to its own channel range, returning the bus.
    ///
    /// Routing filters go *after* the clip's own filters: gain and fades are
    /// clip properties and belong in the source channels, before the samples
    /// are moved somewhere the mix will not touch them.
    pub fn route_audio(&mut self) -> Option<AudioLayout> {
        let layout = self.audio_layout()?;
        for route in &layout.routes {
            let Some(track) = self.tracks.get_mut(route.track_index) else {
                continue;
            };
            for entry in &mut track.entries {
                if let Entry::Clip(c) = entry {
                    let src = c.channels.unwrap_or(2);
                    c.filters
                        .extend(routing_filters(src, layout.total_channels, route.start));
                }
            }
        }
        Some(layout)
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
    let stream = clip
        .media
        .as_ref()
        .and_then(|m| m.stream)
        .map(|s| match kind {
            TrackKind::Audio => StreamSelect::Audio(s),
            _ => StreamSelect::Video(s),
        });
    ClipEntry {
        clip: clip.id,
        label: clip.label.clone(),
        resource,
        stream,
        channels: clip.media.as_ref().and_then(|m| m.channels),
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

/// Where a decoded stream's samples land once the frame is widened to
/// `total` channels.
///
/// This is FFmpeg's default upmix, verified against MLT rather than assumed:
/// a mono stream lands on front-centre (index 2) as soon as the layout has a
/// centre channel, and a stereo stream stays on the front pair.
fn source_positions(src_channels: u16, total: u16) -> Vec<u16> {
    if total <= 2 {
        return (0..src_channels.min(total)).collect();
    }
    match src_channels {
        0 => Vec::new(),
        1 => vec![2],
        _ => vec![0, 1],
    }
}

/// Move a track's samples into the channel range the export gave it.
///
/// Moves are swaps, never copies: a swap leaves silence behind, so a track
/// cannot leak into the range of the track that owns those channels. A mono
/// source is duplicated across its pair once it has been moved.
fn routing_filters(src_channels: u16, total: u16, start: u16) -> Vec<FilterSpec> {
    if total <= CHANNELS_PER_TRACK {
        return Vec::new();
    }
    let mut at = source_positions(src_channels, total);
    let mut out = Vec::new();
    for i in 0..at.len().min(CHANNELS_PER_TRACK as usize) {
        let target = start + i as u16;
        let from = at[i];
        if from == target {
            continue;
        }
        out.push(channel_op(from, target, true));
        // The swap moves whatever was in `target` back to `from`, which
        // matters when a later channel of this same source lived there.
        for slot in at.iter_mut() {
            if *slot == target {
                *slot = from;
            } else if *slot == from {
                *slot = target;
            }
        }
    }
    if at.len() == 1 {
        out.push(channel_op(start, start + 1, false));
    }
    out
}

fn channel_op(from: u16, to: u16, swap: bool) -> FilterSpec {
    FilterSpec {
        service: "channelcopy".into(),
        props: vec![
            ("from".into(), from.to_string()),
            ("to".into(), to.to_string()),
            ("swap".into(), i32::from(swap).to_string()),
        ],
    }
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

    fn ops(f: &[FilterSpec]) -> Vec<(String, String, String)> {
        f.iter()
            .filter(|f| f.service == "channelcopy")
            .map(|f| {
                let get = |k: &str| {
                    f.props
                        .iter()
                        .find(|(n, _)| n == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default()
                };
                (get("from"), get("to"), get("swap"))
            })
            .collect()
    }

    #[test]
    fn a_mono_track_is_swapped_off_centre_then_spread_across_its_pair() {
        // Three mono tracks on a six-channel bus: this is the exact routing
        // verified against MLT for the multitrack fixture.
        assert_eq!(
            ops(&routing_filters(1, 6, 0)),
            vec![
                ("2".into(), "0".into(), "1".into()),
                ("0".into(), "1".into(), "0".into())
            ]
        );
        assert_eq!(
            ops(&routing_filters(1, 6, 2)),
            vec![("2".into(), "3".into(), "0".into())],
            "a mono source already sits on the centre channel"
        );
        assert_eq!(
            ops(&routing_filters(1, 6, 4)),
            vec![
                ("2".into(), "4".into(), "1".into()),
                ("4".into(), "5".into(), "0".into())
            ]
        );
    }

    #[test]
    fn a_stereo_track_moves_as_two_swaps_and_leaves_silence_behind() {
        assert!(ops(&routing_filters(2, 4, 0)).is_empty());
        assert_eq!(
            ops(&routing_filters(2, 4, 2)),
            vec![
                ("0".into(), "2".into(), "1".into()),
                ("1".into(), "3".into(), "1".into())
            ]
        );
    }

    #[test]
    fn routing_never_leaves_two_tracks_sharing_a_channel() {
        // The property that matters: after routing, a track's samples occupy
        // its own pair and nothing else, whatever it started as.
        let total = 8u16;
        for start in [0u16, 2, 4, 6] {
            for src in [1u16, 2] {
                let mut at = source_positions(src, total);
                for f in routing_filters(src, total, start) {
                    let get = |k: &str| -> u16 {
                        f.props
                            .iter()
                            .find(|(n, _)| n == k)
                            .and_then(|(_, v)| v.parse().ok())
                            .unwrap_or_default()
                    };
                    let (from, to, swap) = (get("from"), get("to"), get("swap") == 1);
                    if swap {
                        for slot in at.iter_mut() {
                            if *slot == to {
                                *slot = from;
                            } else if *slot == from {
                                *slot = to;
                            }
                        }
                    } else {
                        at.push(to);
                    }
                }
                at.sort_unstable();
                assert_eq!(
                    at,
                    vec![start, start + 1],
                    "src={src} start={start} left samples outside its own pair"
                );
            }
        }
    }

    #[test]
    fn a_single_audio_track_needs_no_bus() {
        let tl = fixture(&[("V1", &[(0, 10, "a")]), ("A1", &[(0, 10, "b")])]);
        assert_eq!(Projection::of(&tl).audio_layout(), None);
    }

    #[test]
    fn every_audio_track_gets_its_own_channel_pair() {
        let tl = fixture(&[
            ("V1", &[(0, 10, "a")]),
            ("A1", &[(0, 10, "b")]),
            ("A2", &[(0, 10, "c")]),
            ("A3", &[(0, 10, "d")]),
        ]);
        let layout = Projection::of(&tl)
            .audio_layout()
            .expect("three audio tracks");
        assert_eq!(layout.total_channels, 6);
        assert_eq!(
            layout.routes.iter().map(|r| r.start).collect::<Vec<_>>(),
            vec![0, 2, 4]
        );
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
