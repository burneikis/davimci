//! The import pipeline (spec §7, plan.md Phase 5).
//!
//! One file in, one undoable edit out. Every audio and subtitle stream in an
//! MKV becomes its own track, so each is independently trimmable, mutable and
//! ripple-deletable, and everything is conformed to the timeline on the way
//! in (spec §7.1).
//!
//! Import is a [`EditCommand::Sequence`], not a direct mutation: undo of an
//! import removes exactly the tracks and clips it added, and redo reproduces
//! the same ids. Track and clip ids are therefore pinned *before* the
//! sequence is built - a clip cannot reference a track whose id is only
//! decided while the sequence runs.

use std::collections::{BTreeMap, BTreeSet};

use davimci_cmd::{EditCommand, Session};
use davimci_core::{Clip, ClipId, Frame, MediaRef, Timeline, TrackId, TrackKind};

use crate::conform::{self, ConformOptions, Conformed};
use crate::error::AnalysisError;
use crate::probe::{MediaInfo, StreamInfo, StreamKind};
use crate::subtitle::Cue;

/// Import settings.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub conform: ConformOptions,
    /// Where on the timeline the clips land.
    pub at: Frame,
    /// Cues per subtitle stream index, from [`crate::subtitle::extract`].
    /// Streams with no cues still get a track, so the mapping is complete.
    pub subtitles: BTreeMap<u32, Vec<Cue>>,
}

/// Which track a source stream ended up on (spec §7 mapping).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamMapping {
    pub stream: u32,
    pub kind: StreamKind,
    pub track: TrackId,
    pub track_name: String,
    pub clips: usize,
}

/// The result of a successful import.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub path: String,
    pub mapping: Vec<StreamMapping>,
    /// Conformed source length, in timeline frames.
    pub length: Frame,
    /// True when the timeline adopted this file's properties (spec §7.1).
    pub set_timeline_props: bool,
}

impl Imported {
    #[must_use]
    pub fn tracks_of(&self, kind: StreamKind) -> Vec<&StreamMapping> {
        self.mapping.iter().filter(|m| m.kind == kind).collect()
    }
}

/// An import, planned but not yet run.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPlan {
    pub command: EditCommand,
    pub result: Imported,
}

/// Probe-to-timeline in one step: reserve ids, plan, execute, report.
///
/// Ids are reserved up front and are not returned if planning fails. That is
/// deliberate and harmless - ids are monotonic and never reused, so skipping
/// a few costs nothing, whereas a track id that changes between plan and
/// apply would make redo non-deterministic.
pub fn import(
    session: &mut Session,
    info: &MediaInfo,
    opts: &ImportOptions,
) -> Result<Imported, AnalysisError> {
    let ids = session.reserve_ids(ids_needed(info, opts));
    let plan = plan(session.timeline(), info, opts, &ids)?;
    session.exec(&plan.command)?;
    Ok(plan.result)
}

/// How many ids [`plan`] will pin. Over-reserving is safe; under-reserving is
/// not, so this counts the worst case: a new track per stream.
#[must_use]
pub fn ids_needed(info: &MediaInfo, opts: &ImportOptions) -> usize {
    let mut n = 0;
    for s in &info.streams {
        n += 1; // the track
        n += match s.kind {
            StreamKind::Subtitle => opts.subtitles.get(&s.index).map_or(0, Vec::len),
            _ => 1,
        };
    }
    n
}

/// Build the import command. Pure: no I/O, no mutation of `tl`.
pub fn plan(
    tl: &Timeline,
    info: &MediaInfo,
    opts: &ImportOptions,
    ids: &[u64],
) -> Result<ImportPlan, AnalysisError> {
    if info.streams.is_empty() {
        return Err(AnalysisError::NoImportableStreams {
            path: info.path.clone(),
        });
    }

    // An empty timeline takes its properties from the first import (§7.1).
    let empty = tl.tracks().iter().all(davimci_core::Track::is_empty);
    let props = if empty {
        conform::props_from(info, tl.props)
    } else {
        tl.props
    };
    let set_timeline_props = props != tl.props;
    let conformed = conform::conform(info, props, opts.conform);

    let mut ids = ids.iter().copied();
    let mut next = || -> Result<u64, AnalysisError> {
        ids.next().ok_or_else(|| AnalysisError::AnalysisFailed {
            path: info.path.clone(),
            reason: "too few ids were reserved for this import".into(),
        })
    };

    let mut cmds: Vec<EditCommand> = Vec::new();
    if set_timeline_props {
        cmds.push(EditCommand::Reconform { props });
    }

    // Track names have to be predicted, because the whole import is one
    // sequence and later steps name tracks that earlier steps create.
    let mut names = NameCursor::new(tl);
    let mut taken: Vec<TrackId> = Vec::new();
    let mut mapping = Vec::new();

    for stream in &info.streams {
        let kind = track_kind(stream.kind);
        let (track, track_name, add) = match reusable(tl, kind, &taken) {
            Some(t) => (t.0, t.1, None),
            None => {
                let id = TrackId(next()?);
                let name = names.next(kind);
                (
                    id,
                    name.clone(),
                    Some(EditCommand::AddTrack {
                        kind,
                        name: Some(name),
                        new_id: Some(id),
                    }),
                )
            }
        };
        taken.push(track);
        if let Some(add) = add {
            cmds.push(add);
        }

        let clips = match stream.kind {
            StreamKind::Subtitle => {
                let cues = opts
                    .subtitles
                    .get(&stream.index)
                    .cloned()
                    .unwrap_or_default();
                subtitle_clips(&cues, opts.at, props.fps, &mut next)?
            }
            _ => vec![media_clip(
                ClipId(next()?),
                info,
                stream,
                &conformed,
                opts.at,
            )],
        };

        mapping.push(StreamMapping {
            stream: stream.index,
            kind: stream.kind,
            track,
            track_name,
            clips: clips.len(),
        });
        for clip in clips {
            cmds.push(EditCommand::Overwrite {
                track,
                at: clip.start,
                new_id: Some(clip.id),
                clip,
            });
        }
    }

    Ok(ImportPlan {
        command: EditCommand::Sequence(cmds),
        result: Imported {
            path: info.path.clone(),
            mapping,
            length: conformed.length,
            set_timeline_props,
        },
    })
}

fn track_kind(kind: StreamKind) -> TrackKind {
    match kind {
        StreamKind::Video => TrackKind::Video,
        StreamKind::Audio => TrackKind::Audio,
        StreamKind::Subtitle => TrackKind::Text,
    }
}

/// An empty track of the right kind that this import has not already used.
/// Importing into a fresh project should fill `V1` and `A1`, not leave them
/// stranded above the new tracks.
fn reusable(tl: &Timeline, kind: TrackKind, taken: &[TrackId]) -> Option<(TrackId, String)> {
    tl.tracks()
        .iter()
        .find(|t| t.kind == kind && t.is_empty() && !taken.contains(&t.id))
        .map(|t| (t.id, t.name.clone()))
}

/// Predicts the names `AddTrack` will generate, given tracks this plan is
/// itself adding.
///
/// Like [`Timeline::next_track_name`], this takes the lowest free index
/// rather than a count: a project that has had tracks removed would
/// otherwise be handed a name that already exists, and `AddTrack` would
/// reject the whole import.
#[derive(Debug)]
struct NameCursor {
    used: BTreeSet<String>,
}

impl NameCursor {
    fn new(tl: &Timeline) -> Self {
        Self {
            used: tl.tracks().iter().map(|t| t.name.clone()).collect(),
        }
    }

    fn next(&mut self, kind: TrackKind) -> String {
        let prefix = kind.prefix();
        let name = (1..)
            .map(|n| format!("{prefix}{n}"))
            .find(|name| !self.used.contains(name))
            .unwrap_or_else(|| format!("{prefix}1"));
        self.used.insert(name.clone());
        name
    }
}

fn media_clip(
    id: ClipId,
    info: &MediaInfo,
    stream: &StreamInfo,
    conformed: &Conformed,
    at: Frame,
) -> Clip {
    let media = MediaRef::new(&info.path, conformed.source_fps, conformed.length);
    Clip::from_media(id, stream.label(), media, at, Frame::ZERO, conformed.length)
}

fn subtitle_clips(
    cues: &[Cue],
    at: Frame,
    fps: davimci_core::Fps,
    next: &mut impl FnMut() -> Result<u64, AnalysisError>,
) -> Result<Vec<Clip>, AnalysisError> {
    let mut out: Vec<Clip> = Vec::new();
    for cue in cues {
        // Everything below is in timeline space, `at` included: comparing a
        // cue-relative start against a placed clip's end used to clamp every
        // cue after the first past its own end and drop it.
        let start = conform::frame_at_ms(cue.start_ms, fps).saturating_add(at);
        let end = conform::frame_at_ms(cue.end_ms, fps).saturating_add(at);
        // A cue shorter than a frame still has to be visible for one.
        let end = end.max(Frame(start.get() + 1));
        // Overlapping cues cannot share a track; a later one starts where the
        // previous ended rather than being dropped.
        let start = out.last().map_or(start, |c| start.max(c.end()));
        if start >= end {
            continue;
        }
        let mut clip = Clip::generated(
            ClipId(next()?),
            cue.text.lines().next().unwrap_or_default(),
            start,
            Frame(end.get() - start.get()),
        );
        clip.text = Some(cue.text.clone());
        out.push(clip);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::probe::tests::multitrack;
    use crate::subtitle::parse_srt;
    use davimci_core::{Fps, TimelineProps};

    fn session() -> Session {
        Session::new(Timeline::new(TimelineProps::default()))
    }

    fn with_subs(opts: &mut ImportOptions) {
        opts.subtitles.insert(
            4,
            parse_srt("1\n00:00:01,000 --> 00:00:03,000\nsubtitle track 1\n"),
        );
        opts.subtitles.insert(
            5,
            parse_srt("1\n00:00:02,000 --> 00:00:04,000\nsubtitle track 2\n"),
        );
    }

    #[test]
    fn every_stream_becomes_its_own_track() {
        let mut s = session();
        let mut opts = ImportOptions::default();
        with_subs(&mut opts);
        let imported = import(&mut s, &multitrack(), &opts).unwrap();

        assert_eq!(imported.tracks_of(StreamKind::Video).len(), 1);
        assert_eq!(imported.tracks_of(StreamKind::Audio).len(), 3);
        assert_eq!(imported.tracks_of(StreamKind::Subtitle).len(), 2);

        // The default V1/A1 are reused, the rest are new.
        let names: Vec<&str> = imported
            .mapping
            .iter()
            .map(|m| m.track_name.as_str())
            .collect();
        assert_eq!(names, vec!["V1", "A1", "A2", "A3", "T1", "T2"]);

        // Every mapping points at a track that actually exists and holds the
        // clips it claims.
        for m in &imported.mapping {
            let t = s.timeline().track(m.track).unwrap();
            assert_eq!(t.name, m.track_name);
            assert_eq!(t.clips().len(), m.clips, "{} clip count", m.track_name);
        }
        s.timeline().assert_invariants();
    }

    #[test]
    fn the_stream_to_track_mapping_is_exact() {
        let mut s = session();
        let mut opts = ImportOptions::default();
        with_subs(&mut opts);
        let imported = import(&mut s, &multitrack(), &opts).unwrap();
        let pairs: Vec<(u32, &str)> = imported
            .mapping
            .iter()
            .map(|m| (m.stream, m.track_name.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (0, "V1"),
                (1, "A1"),
                (2, "A2"),
                (3, "A3"),
                (4, "T1"),
                (5, "T2")
            ]
        );
    }

    #[test]
    fn audio_tracks_are_labelled_from_stream_metadata() {
        let mut s = session();
        let imported = import(&mut s, &multitrack(), &ImportOptions::default()).unwrap();
        let labels: Vec<String> = imported
            .tracks_of(StreamKind::Audio)
            .iter()
            .map(|m| {
                s.timeline().track(m.track).unwrap().clips()[0]
                    .label
                    .clone()
            })
            .collect();
        assert_eq!(labels, vec!["dialogue", "music", "effects"]);
    }

    #[test]
    fn subtitle_text_lands_on_the_text_track() {
        let mut s = session();
        let mut opts = ImportOptions::default();
        with_subs(&mut opts);
        let imported = import(&mut s, &multitrack(), &opts).unwrap();
        let t1 = imported.tracks_of(StreamKind::Subtitle)[0];
        let clips = s.timeline().track(t1.track).unwrap().clips();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].text.as_deref(), Some("subtitle track 1"));
        // 1s to 3s at 30fps (the file's own rate, adopted by the timeline).
        assert_eq!(clips[0].start, Frame(30));
        assert_eq!(clips[0].duration, Frame(60));
    }

    #[test]
    fn the_first_import_sets_the_timeline_properties() {
        let mut s = session();
        let imported = import(&mut s, &multitrack(), &ImportOptions::default()).unwrap();
        assert!(imported.set_timeline_props);
        assert_eq!(s.timeline().props.fps, Fps::FPS_30);
        assert_eq!(s.timeline().props.resolution.width, 640);
        // 5s at 30fps.
        assert_eq!(imported.length, Frame(150));
    }

    #[test]
    fn a_later_import_conforms_to_the_timeline_instead() {
        let mut s = session();
        import(&mut s, &multitrack(), &ImportOptions::default()).unwrap();
        let props = s.timeline().props;

        let mut second = multitrack();
        second.path = "/fixtures/other.mkv".into();
        if let Some(v) = second.streams.first_mut() {
            v.fps = Some(Fps::FPS_60);
            v.frames = Some(300);
        }
        let opts = ImportOptions {
            at: Frame(600),
            ..ImportOptions::default()
        };
        let imported = import(&mut s, &second, &opts).unwrap();

        assert!(!imported.set_timeline_props);
        assert_eq!(s.timeline().props, props, "the timeline rate is fixed");
        // 300 frames of 60fps is 150 frames of the 30fps timeline.
        assert_eq!(imported.length, Frame(150));
    }

    #[test]
    fn an_import_is_one_undoable_step() {
        let mut s = session();
        let before = s.timeline().dump();
        let tracks_before = s.timeline().tracks().len();
        let mut opts = ImportOptions::default();
        with_subs(&mut opts);
        import(&mut s, &multitrack(), &opts).unwrap();
        assert!(s.timeline().tracks().len() > tracks_before);

        s.undo().unwrap();
        assert_eq!(s.timeline().dump(), before);
        assert_eq!(s.timeline().tracks().len(), tracks_before);

        s.redo().unwrap();
        assert_eq!(s.timeline().tracks().len(), tracks_before + 4);
    }

    #[test]
    fn overlapping_cues_are_sequenced_rather_than_dropped() {
        let cues = parse_srt(
            "1\n00:00:01,000 --> 00:00:03,000\none\n\n2\n00:00:02,000 --> 00:00:04,000\ntwo\n",
        );
        let mut n = 100;
        let mut next = || {
            n += 1;
            Ok(n)
        };
        let clips = subtitle_clips(&cues, Frame::ZERO, Fps::FPS_30, &mut next).unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].end(), clips[1].start);
    }

    /// Regression: the overlap check compared a cue-relative start against an
    /// already-placed clip's end, so importing subtitles anywhere but frame
    /// zero clamped every cue after the first past its own end and silently
    /// dropped it.
    #[test]
    fn cues_keep_their_spacing_when_imported_away_from_frame_zero() {
        let cues = parse_srt(
            "1\n00:00:01,000 --> 00:00:02,000\none\n\n2\n00:00:03,000 --> 00:00:04,000\ntwo\n",
        );
        let mut n = 100;
        let mut next = || {
            n += 1;
            Ok(n)
        };
        let at = Frame(600);
        let clips = subtitle_clips(&cues, at, Fps::FPS_30, &mut next).unwrap();
        assert_eq!(clips.len(), 2, "no cue may be dropped by the offset");
        // 1-2s and 3-4s at 30fps, shifted by `at`.
        assert_eq!((clips[0].start, clips[0].end()), (Frame(630), Frame(660)));
        assert_eq!((clips[1].start, clips[1].end()), (Frame(690), Frame(720)));
    }

    #[test]
    fn importing_a_file_with_no_streams_is_rejected_before_anything_changes() {
        let mut s = session();
        let before = s.timeline().dump();
        let mut info = multitrack();
        info.streams.clear();
        assert!(import(&mut s, &info, &ImportOptions::default()).is_err());
        assert_eq!(s.timeline().dump(), before);
    }
}
