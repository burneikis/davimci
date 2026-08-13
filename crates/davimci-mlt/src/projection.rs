//! Timeline -> MLT graph projection, as pure data.
//!
//! This module contains no MLT types and does no I/O: it turns a
//! [`Timeline`] into the *shape* the render graph must have - one playlist
//! per track, blanks for gaps, one entry per clip with its render-time
//! filters. `xml` serialises this shape for the golden tests, and `patch`
//! diffs two of them so an edit becomes playlist mutations rather than a
//! rebuild.

use davimci_core::{Clip, ClipId, Frame, Timeline, TimelineProps, TrackId, TrackKind, Transition};

/// What an entry plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// A conformed media file on disk.
    File(String),
    /// A generated text/subtitle clip.
    Text(String),
    /// Offline media: renders as a placeholder so the project stays editable
    /// while export stays blocked.
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
            // A text card is transparent: the glyphs are burned *onto* the
            // picture, and an opaque card would replace it.
            Self::Text(_) => "#00000000".into(),
            Self::Colour => "#ff000000".into(),
        }
    }
}

/// Which stream of a container an entry plays.
///
/// A multi-stream file becomes one track per stream, so an entry
/// that does not name its stream would play the demuxer's default and every
/// audio track would carry the same samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSelect {
    Audio(u32),
    Video(u32),
}

/// A render-time filter attached to one entry. Never destructive.
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
    /// Inclusive out-point: MLT's `out` is the last frame, not one past
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

    /// Whether `other` plays the same producer over a different span, which
    /// is the only update a live playlist may take as a resize.
    ///
    /// Everything a producer is built from has to be compared here. A text
    /// edit keeps the clip id, the filters and the span and changes only the
    /// payload, so a resize would leave the graph playing the words the edit
    /// replaced until the project is reopened.
    #[must_use]
    pub fn same_producer(&self, other: &Self) -> bool {
        self.clip == other.clip
            && self.resource == other.resource
            && self.stream == other.stream
            && self.channels == other.channels
            && self.filters == other.filters
    }
}

/// The overlap between two clips, projected.
///
/// MLT has no "transition inside a playlist": a transition composites two
/// *tracks*. So the overlap becomes one playlist entry that is itself a
/// two-track tractor - the outgoing clip running into its tail handle, the
/// incoming clip starting early out of its head handle, and the transition
/// planted between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionEntry {
    /// The incoming clip, which is what owns the transition in the model and
    /// what the diff aligns this entry on.
    pub clip: ClipId,
    /// Registry name, e.g. `dissolve`.
    pub kind: String,
    /// MLT transition service.
    pub service: String,
    pub props: Vec<(String, String)>,
    /// The outgoing clip's tail, `a_track`.
    pub from: Box<ClipEntry>,
    /// The incoming clip's head, `b_track`.
    pub to: Box<ClipEntry>,
}

impl TransitionEntry {
    /// Frames the overlap occupies on the timeline. Both sides are the same
    /// length by construction; a mismatch would desynchronise the playlist.
    #[must_use]
    pub fn length(&self) -> u64 {
        self.from.length()
    }
}

/// One playlist slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Blank { length: Frame },
    Clip(Box<ClipEntry>),
    Transition(Box<TransitionEntry>),
}

impl Entry {
    #[must_use]
    pub fn length(&self) -> u64 {
        match self {
            Self::Blank { length } => length.get(),
            Self::Clip(c) => c.length(),
            Self::Transition(t) => t.length(),
        }
    }

    /// Filters on this entry; a blank has none.
    ///
    /// A transition's filters live on its two inner entries, not on the
    /// tractor: gain and fades are clip properties and have to be applied to
    /// the clip they belong to before the two are composited.
    #[must_use]
    pub fn filters(&self) -> &[FilterSpec] {
        match self {
            Self::Blank { .. } | Self::Transition(_) => &[],
            Self::Clip(c) => &c.filters,
        }
    }

    #[must_use]
    pub fn clip_id(&self) -> Option<ClipId> {
        match self {
            Self::Blank { .. } => None,
            Self::Clip(c) => Some(c.clip),
            Self::Transition(t) => Some(t.clip),
        }
    }

    /// Whether this entry is the overlap belonging to `clip`'s head cut.
    #[must_use]
    pub fn is_transition(&self) -> bool {
        matches!(self, Self::Transition(_))
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
                let clips = track.clips();
                let mut entries = Vec::new();
                let mut cursor = Frame::ZERO;
                for (i, clip) in clips.iter().enumerate() {
                    if clip.start > cursor {
                        entries.push(Entry::Blank {
                            length: clip.start.saturating_sub(cursor),
                        });
                    }
                    // The overlap is made of handle frames on both sides, so
                    // it eats into this clip's head and the previous clip's
                    // tail; the entries around it are shortened to match and
                    // the timeline length is unchanged.
                    let incoming = attached(track, i);
                    let outgoing = attached(track, i + 1);
                    // The previous entry has already been shortened by this
                    // transition's head: it saw it as its own `outgoing`.
                    if let Some((prev, t)) = clips.get(i.wrapping_sub(1)).zip(incoming) {
                        entries.push(Entry::Transition(Box::new(project_transition(
                            prev, clip, t, track.kind,
                        ))));
                    }
                    let mut entry = project_clip(clip, track.kind);
                    entry.in_point =
                        Frame(entry.in_point.get() + incoming.map_or(0, Transition::tail));
                    entry.out_point =
                        Frame(entry.out_point.get() - outgoing.map_or(0, Transition::head));
                    entries.push(Entry::Clip(Box::new(entry)));
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

    /// The incoming clip of every projected transition.
    ///
    /// The backend owns one nested tractor per transition and uses this to
    /// know which ones a patch has left behind.
    #[must_use]
    pub fn transition_clips(&self) -> std::collections::BTreeSet<ClipId> {
        self.tracks
            .iter()
            .flat_map(|t| &t.entries)
            .filter_map(|e| match e {
                Entry::Transition(t) => Some(t.clip),
                _ => None,
            })
            .collect()
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

    /// Indexes of the tracks whose picture has to be composited over what is
    /// under them.
    ///
    /// A tractor shows the topmost visible track and drops everything below
    /// it unless a blend is planted, so an overlay or a burned-in subtitle
    /// would *replace* the picture instead of sitting on it. The lowest
    /// visual track is the background and needs none.
    #[must_use]
    pub fn video_blend_tracks(&self) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                matches!(
                    t.kind,
                    TrackKind::Video | TrackKind::Overlay | TrackKind::Text
                )
            })
            .map(|(i, _)| i)
            .skip(1)
            .collect()
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
                start: u16::try_from(n).unwrap_or(u16::MAX) * CHANNELS_PER_TRACK,
                channels: CHANNELS_PER_TRACK,
            })
            .collect::<Vec<_>>();
        Some(AudioLayout {
            total_channels: u16::try_from(routes.len()).unwrap_or(u16::MAX) * CHANNELS_PER_TRACK,
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

    /// Drop every text track, for an export whose subtitles travel as a
    /// sidecar or a muxed stream rather than being painted on.
    /// Returns whether anything was dropped.
    pub fn drop_text_tracks(&mut self) -> bool {
        let before = self.tracks.len();
        self.tracks.retain(|t| t.kind != TrackKind::Text);
        self.tracks.len() != before
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

/// The transition on the cut at `index`, if it is one this build can build.
///
/// Defensive on purpose: a projection must never emit a negative-length
/// entry, so anything the model would refuse is simply not projected.
fn attached(track: &davimci_core::Track, index: usize) -> Option<&Transition> {
    let t = track.clips().get(index)?.transition_in.as_ref()?;
    track.check_transition(index, t).ok().map(|()| t)
}

/// The overlap as a two-track tractor: outgoing tail against incoming head.
fn project_transition(
    prev: &Clip,
    clip: &Clip,
    t: &Transition,
    kind: TrackKind,
) -> TransitionEntry {
    let head = t.head();
    let last = t.duration.get().saturating_sub(1);

    // The outgoing clip runs `tail` frames past its out-point. Saturating,
    // because a clip with no source handles - a generated one, or media that
    // went offline - must project to something rather than trap.
    let mut from = project_clip(prev, kind);
    from.in_point = Frame(prev.source_out().get().saturating_sub(head));
    from.out_point = Frame(from.in_point.get() + last);

    // The incoming clip starts `head` frames before its in-point.
    let mut to = project_clip(clip, kind);
    to.in_point = Frame(clip.source_in.get().saturating_sub(head));
    to.out_point = Frame(to.in_point.get() + last);

    let spec = crate::transitions::spec(&t.kind, kind);
    TransitionEntry {
        clip: clip.id,
        kind: t.kind.clone(),
        service: spec.service,
        props: spec.props,
        from: Box::new(from),
        to: Box::new(to),
    }
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

/// Clip properties become filters; the media is never touched.
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
/// This is `FFmpeg`'s default upmix, verified against MLT rather than assumed:
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
        let target = start + u16::try_from(i).unwrap_or(u16::MAX);
        let from = at[i];
        if from == target {
            continue;
        }
        out.push(channel_op(from, target, true));
        // The swap moves whatever was in `target` back to `from`, which
        // matters when a later channel of this same source lived there.
        for slot in &mut at {
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

    /// Regression: an overlay or a subtitle track used to *replace* the
    /// picture, because a tractor shows its topmost visible track and
    /// nothing planted a blend. Only the lowest visual track goes
    /// unblended - it is the background.
    #[test]
    fn every_visual_track_above_the_first_is_blended_and_no_audio_track_is() {
        let tl = fixture(&[
            ("V1", &[(0, 100, "a")]),
            ("A1", &[(0, 100, "music")]),
            ("O1", &[(0, 50, "logo")]),
            ("T1", &[(0, 30, "cue")]),
        ]);
        let p = Projection::of(&tl);
        let names: Vec<&str> = p
            .video_blend_tracks()
            .into_iter()
            .map(|i| p.tracks[i].name.as_str())
            .collect();
        assert_eq!(names, ["O1", "T1"]);
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

    /// The load-bearing transition property: the overlap is made of handle
    /// frames, so planting one changes no clip's position and no track's
    /// length - it only moves the in and out points around the cut.
    #[test]
    fn a_transition_borrows_handles_and_keeps_the_track_length() {
        let mut tl =
            davimci_core::testing::media_fixture(&[(0, 100, 20, 400), (100, 100, 20, 400)]);
        let track = tl.tracks()[0].id;
        let right = tl.tracks()[0].clips()[1].id;
        let before: u64 = Projection::of(&tl).tracks[0]
            .entries
            .iter()
            .map(Entry::length)
            .sum();

        tl.set_transition(track, right, Some(davimci_core::Transition::of("dissolve")))
            .unwrap();
        let p = Projection::of(&tl);
        let entries = &p.tracks[0].entries;
        assert_eq!(entries.len(), 3, "clip, overlap, clip");
        let total: u64 = entries.iter().map(Entry::length).sum();
        assert_eq!(
            total, before,
            "a transition never changes the timeline length"
        );
        assert_eq!(entries[0].length(), 94, "the outgoing clip gives up six");
        assert_eq!(entries[1].length(), 12);
        assert_eq!(entries[2].length(), 94, "the incoming clip gives up six");

        let Entry::Transition(t) = &entries[1] else {
            panic!("expected the overlap");
        };
        assert_eq!(
            t.clip, right,
            "the overlap is identified by the clip it enters"
        );
        assert_eq!(t.service, "luma");
        // The outgoing side runs into its tail handle, the incoming side
        // starts early out of its head handle.
        assert_eq!(
            (t.from.in_point, t.from.out_point),
            (Frame(114), Frame(125))
        );
        assert_eq!((t.to.in_point, t.to.out_point), (Frame(14), Frame(25)));
    }

    #[test]
    fn an_audio_track_cross_fades_rather_than_wiping() {
        let mut tl =
            davimci_core::testing::media_fixture(&[(0, 100, 20, 400), (100, 100, 20, 400)]);
        // Move both clips onto an audio track by projecting A1 instead: the
        // service choice is the track's medium, not the transition's name.
        let track = tl.tracks()[0].id;
        let right = tl.tracks()[0].clips()[1].id;
        tl.set_transition(
            track,
            right,
            Some(davimci_core::Transition::new("wipe_left", Frame(12))),
        )
        .unwrap();
        let video = Projection::of(&tl);
        let Entry::Transition(t) = &video.tracks[0].entries[1] else {
            panic!("expected the overlap");
        };
        assert_eq!(t.service, "luma");
        assert_eq!(
            crate::transitions::spec("wipe_left", TrackKind::Audio).service,
            "mix"
        );
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
                        for slot in &mut at {
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

    fn cue(text: &str) -> ClipEntry {
        ClipEntry {
            clip: ClipId(1),
            label: "cue".into(),
            resource: Resource::Text(text.into()),
            in_point: Frame(0),
            out_point: Frame(49),
            stream: None,
            channels: None,
            filters: Vec::new(),
        }
    }

    /// Regression: a text edit keeps the clip id, the span and the filters,
    /// so the patch path took it as a resize and the live graph went on
    /// playing the words the edit replaced until the project was reopened.
    #[test]
    fn a_text_edit_is_not_a_resize_but_a_trim_is() {
        let before = cue("hello");
        assert!(!before.same_producer(&cue("hello there")));
        let mut trimmed = before.clone();
        trimmed.out_point = Frame(29);
        assert!(before.same_producer(&trimmed));
    }
}
