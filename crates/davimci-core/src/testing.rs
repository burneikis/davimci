//! Fixture builders for model tests. Test-only; no media, no I/O.

use crate::clip::{Clip, MediaRef};
use crate::id::{ClipId, TrackId};
use crate::time::{Fps, Frame};
use crate::timeline::Timeline;
use crate::track::TrackKind;

fn kind_of(name: &str) -> TrackKind {
    match name.as_bytes().first() {
        Some(b'A') => TrackKind::Audio,
        Some(b'T') => TrackKind::Text,
        Some(b'O') => TrackKind::Overlay,
        _ => TrackKind::Video,
    }
}

/// `(start, duration, label)` for one generated clip.
pub type ClipSpec<'a> = (u64, u64, &'a str);

/// `(track name, clips)` for one track.
pub type TrackSpec<'a> = (&'a str, &'a [ClipSpec<'a>]);

/// Build a timeline from `(track name, [(start, duration, label)])`.
///
/// Clips are generated (no media), so handle limits do not apply - use
/// [`media_fixture`] when a test needs handles.
#[must_use]
pub fn fixture(spec: &[TrackSpec<'_>]) -> Timeline {
    let mut tl = Timeline::default();
    for (name, clips) in spec {
        let id = match tl.track_by_name(name).map(|t| t.id) {
            Some(id) => id,
            None => {
                let id = tl.add_track(kind_of(name));
                if let Some(t) = tl.track_mut(id) {
                    t.name = (*name).to_string();
                }
                id
            }
        };
        for (start, dur, label) in *clips {
            let cid = tl.new_clip_id();
            let clip = Clip::generated(cid, *label, Frame(*start), Frame(*dur));
            if let Some(t) = tl.track_mut(id) {
                t.insert_sorted(clip);
            }
        }
    }
    tl
}

/// Build a single-video-track timeline of media clips, from
/// `(start, duration, source_in, source_length)`. Labels are `m0`, `m1`, ...
#[must_use]
pub fn media_fixture(spec: &[(u64, u64, u64, u64)]) -> Timeline {
    let mut tl = Timeline::default();
    let Some(v1) = tl.track_by_name("V1").map(|t| t.id) else {
        return tl;
    };
    for (i, (start, dur, src_in, src_len)) in spec.iter().enumerate() {
        let cid = tl.new_clip_id();
        let media = MediaRef::new(format!("/media/{i}.mkv"), Fps::FPS_60, Frame(*src_len));
        let clip = Clip::from_media(
            cid,
            format!("m{i}"),
            media,
            Frame(*start),
            Frame(*src_in),
            Frame(*dur),
        );
        if let Some(t) = tl.track_mut(v1) {
            t.insert_sorted(clip);
        }
    }
    tl
}

/// A video track plus `audio` audio tracks, each holding one media clip off
/// the same container but a different stream - the shape a multi-stream file
/// imports into.
#[must_use]
pub fn multi_audio_fixture(audio: usize, channels: Option<u16>) -> Timeline {
    let mut tl = media_fixture(&[(0, 100, 0, 600)]);
    // A default timeline ships with an empty A1; the caller asked for an
    // exact number of audio tracks, so it goes.
    let empty: Vec<TrackId> = tl
        .tracks()
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && t.clips().is_empty())
        .map(|t| t.id)
        .collect();
    for id in empty {
        let _ = tl.remove_track(id);
    }
    for n in 0..audio {
        let track = tl.add_track(TrackKind::Audio);
        let cid = tl.new_clip_id();
        let mut media = MediaRef::new("/media/multi.mkv", Fps::FPS_60, Frame(600));
        media.stream = Some(n as u32 + 1);
        media.channels = channels;
        let clip = Clip::from_media(
            cid,
            format!("a{n}"),
            media,
            Frame::ZERO,
            Frame::ZERO,
            Frame(100),
        );
        if let Some(t) = tl.track_mut(track) {
            t.insert_sorted(clip);
        }
    }
    tl
}

/// Id of a track by name, or `TrackId(0)` if absent (which every primitive
/// rejects, so a typo in a fixture fails loudly rather than silently).
#[must_use]
pub fn track_id(tl: &Timeline, name: &str) -> TrackId {
    tl.track_by_name(name).map_or(TrackId(0), |t| t.id)
}

/// Clip ids on a track, in timeline order.
#[must_use]
pub fn clip_ids(tl: &Timeline, name: &str) -> Vec<ClipId> {
    tl.track_by_name(name)
        .map(|t| t.clips().iter().map(|c| c.id).collect())
        .unwrap_or_default()
}
