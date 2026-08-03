//! Audio operations as command builders (plan.md Phase 9e, spec §6.1).
//!
//! Gain, fades, normalisation and ducking are **clip properties**, never
//! destructive edits: everything here returns an [`EditCommand`] that the
//! session applies, so each one is undoable, repeatable and scriptable like
//! any other mutation.
//!
//! Nothing in this module does I/O or touches a backend, so the arithmetic -
//! which is where these get subtly wrong - is unit-testable with no media.

use davimci_analysis::{Analysis, Span};
use davimci_cmd::EditCommand;
use davimci_core::{Clip, ClipId, ClipProps, Fps, Frame, Timeline, TrackId};

use crate::error::CliError;

/// Which end of a clip a fade applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeEnd {
    In,
    Out,
}

impl FadeEnd {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "in" => Some(Self::In),
            "out" => Some(Self::Out),
            _ => None,
        }
    }
}

/// The clip under the playhead, which is what every command here acts on
/// when there is no selection.
pub fn clip_under_playhead(tl: &Timeline, what: &'static str) -> Result<(TrackId, Clip), CliError> {
    let head = tl.playhead();
    let clip = tl
        .track(head.track)
        .and_then(|t| t.clip_at(head.frame))
        .ok_or(CliError::NoClipUnderPlayhead(what))?;
    Ok((head.track, clip.clone()))
}

/// `:gain <db>` - absolute gain, not a step (spec §6.1).
#[must_use]
pub fn gain(track: TrackId, clip: &Clip, db: f32) -> EditCommand {
    EditCommand::SetProps {
        track,
        clip: clip.id,
        props: ClipProps {
            gain_db: db,
            ..clip.props
        },
    }
}

/// `:fade in|out <ms>` - an explicit duration rather than a motion span.
///
/// The fade is clamped to the clip: a two-second fade on a one-second clip is
/// a one-second fade, not an envelope that runs off the end.
#[must_use]
pub fn fade(track: TrackId, clip: &Clip, end: FadeEnd, ms: u64, fps: Fps) -> EditCommand {
    let frames = Frame(frames_for_ms(ms, fps)).min(clip.duration);
    let props = match end {
        FadeEnd::In => ClipProps {
            fade_in: frames,
            ..clip.props
        },
        FadeEnd::Out => ClipProps {
            fade_out: frames,
            ..clip.props
        },
    };
    EditCommand::SetProps {
        track,
        clip: clip.id,
        props,
    }
}

/// The gain that brings a clip's measured loudness to `target_db`.
///
/// Measured from the loudest RMS hop the clip actually uses, so trimming a
/// clip away from a loud section changes what normalising it does - which is
/// the behaviour the user sees and expects.
#[must_use]
pub fn normalize_gain(clip: &Clip, analysis: &Analysis, fps: Fps, target_db: f32) -> Option<f32> {
    let (from, to) = source_ms_range(clip, fps);
    let hop = u64::from(analysis.params.hop_ms.max(1));
    let first = (from / hop) as usize;
    let last = ((to.saturating_sub(1)) / hop) as usize;
    if first >= analysis.hops.len() {
        return None;
    }
    let loudest = analysis.hops[first..=last.min(analysis.hops.len() - 1)]
        .iter()
        .map(|h| h.rms_db)
        .fold(f32::NEG_INFINITY, f32::max);
    loudest.is_finite().then_some(target_db - loudest)
}

/// The spans of a track that are *not* silent, in timeline frames.
///
/// Ducking is defined against another track being audible (spec §6.1), so
/// this is the reference signal: source-time silence mapped back through each
/// clip's own in-point, since analysis measures the source.
#[must_use]
pub fn loud_spans(tl: &Timeline, track: TrackId, analysis: &Analysis) -> Vec<(Frame, Frame)> {
    let fps = tl.props.fps;
    let Some(t) = tl.track(track) else {
        return Vec::new();
    };
    let mut out: Vec<(Frame, Frame)> = Vec::new();
    for clip in t.clips() {
        let (from_ms, to_ms) = source_ms_range(clip, fps);
        for (a_ms, b_ms) in loud_ranges(&analysis.silence, from_ms, to_ms) {
            let a = clip.start.get() + frames_for_ms(a_ms - from_ms, fps);
            let b = clip.start.get() + frames_for_ms(b_ms - from_ms, fps);
            let (a, b) = (a.min(clip.end().get()), b.min(clip.end().get()));
            if b > a {
                push_merged(&mut out, (Frame(a), Frame(b)));
            }
        }
    }
    out
}

/// `:duck <track> <db>` - lower this track wherever the reference track is
/// audible.
///
/// Gain is one value per clip, so ducking a *region* means splitting the clip
/// around it and lowering the middle. Every split and every property change
/// is one `Sequence`, so a duck is one `u` away from gone.
pub fn duck_plan(
    tl: &Timeline,
    track: TrackId,
    spans: &[(Frame, Frame)],
    db: f32,
    ids: &mut impl Iterator<Item = u64>,
) -> Result<EditCommand, CliError> {
    let Some(t) = tl.track(track) else {
        return Err(CliError::NoSuchTrack(track.to_string()));
    };
    let mut cmds = Vec::new();
    for clip in t.clips() {
        for (a, b) in spans {
            let start = (*a).max(clip.start);
            let end = (*b).min(clip.end());
            if end <= start {
                continue;
            }
            // The piece to duck is what is left after cutting at both ends.
            // Splitting at the clip's own start or end would be a no-op that
            // the model rejects, so those cuts are skipped rather than made.
            let mut piece = clip.id;
            if start > clip.start {
                let id = ClipId(ids.next().ok_or(CliError::AnalysisNotReady(":duck"))?);
                cmds.push(EditCommand::Split {
                    track,
                    frame: start,
                    new_id: Some(id),
                });
                piece = id;
            }
            if end < clip.end() {
                let id = ClipId(ids.next().ok_or(CliError::AnalysisNotReady(":duck"))?);
                cmds.push(EditCommand::Split {
                    track,
                    frame: end,
                    new_id: Some(id),
                });
            }
            cmds.push(EditCommand::SetProps {
                track,
                clip: piece,
                props: ClipProps {
                    gain_db: clip.props.gain_db + db,
                    ..clip.props
                },
            });
        }
    }
    if cmds.is_empty() {
        return Err(CliError::NoClipUnderPlayhead("duck"));
    }
    Ok(EditCommand::Sequence(cmds))
}

/// How many ids a duck could need: two splits per (clip, span) overlap.
#[must_use]
pub fn duck_ids_needed(tl: &Timeline, track: TrackId, spans: &[(Frame, Frame)]) -> usize {
    tl.track(track).map_or(0, |t| {
        t.clips()
            .iter()
            .map(|c| {
                spans
                    .iter()
                    .filter(|(a, b)| *b > c.start && *a < c.end())
                    .count()
                    * 2
            })
            .sum()
    })
}

/// The clip's window into its source, in milliseconds.
fn source_ms_range(clip: &Clip, fps: Fps) -> (u64, u64) {
    let ms = |f: Frame| (fps.frame_to_nanos(f) / 1_000_000) as u64;
    let start = ms(clip.source_in);
    (start, start + ms(clip.duration).max(1))
}

fn frames_for_ms(ms: u64, fps: Fps) -> u64 {
    (ms as f64 * fps.as_f64() / 1000.0).round() as u64
}

/// Complement of the silence spans, clipped to `[from_ms, to_ms)`.
fn loud_ranges(silence: &[Span], from_ms: u64, to_ms: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut cursor = from_ms;
    for s in silence
        .iter()
        .filter(|s| s.end_ms > from_ms && s.start_ms < to_ms)
    {
        if s.start_ms > cursor {
            out.push((cursor, s.start_ms.min(to_ms)));
        }
        cursor = cursor.max(s.end_ms);
    }
    if cursor < to_ms {
        out.push((cursor, to_ms));
    }
    out.retain(|(a, b)| b > a);
    out
}

/// Append a span, merging it with the previous one when they touch: two
/// adjacent clips of one take are one loud region, not two.
fn push_merged(out: &mut Vec<(Frame, Frame)>, span: (Frame, Frame)) {
    match out.last_mut() {
        Some(last) if span.0 <= last.1 => last.1 = last.1.max(span.1),
        _ => out.push(span),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use davimci_analysis::{AnalysisParams, Hop};
    use davimci_core::testing::{clip_ids, media_fixture, track_id};

    fn analysis(hops: &[(f32, f32)], silence: &[(u64, u64)]) -> Analysis {
        Analysis {
            version: 1,
            source_hash: "h".into(),
            params: AnalysisParams::default(),
            sample_rate: 48_000,
            duration_ms: hops.len() as u64 * 10,
            hops: hops
                .iter()
                .map(|(peak, rms)| Hop {
                    peak_db: *peak,
                    rms_db: *rms,
                })
                .collect(),
            silence: silence
                .iter()
                .map(|(a, b)| Span {
                    start_ms: *a,
                    end_ms: *b,
                })
                .collect(),
            scene_changes: Vec::new(),
        }
    }

    #[test]
    fn a_fade_is_clamped_to_the_clip_it_is_on() {
        // An envelope longer than the clip would run off the end of the media.
        let tl = media_fixture(&[(0, 60, 0, 600)]);
        let clip = tl.tracks()[0].clips()[0].clone();
        let cmd = fade(tl.tracks()[0].id, &clip, FadeEnd::In, 5_000, Fps::FPS_60);
        let EditCommand::SetProps { props, .. } = cmd else {
            panic!("expected SetProps");
        };
        assert_eq!(props.fade_in, Frame(60), "clamped to the clip's length");
    }

    #[test]
    fn gain_is_absolute_rather_than_a_step() {
        let mut tl = media_fixture(&[(0, 60, 0, 600)]);
        let track = tl.tracks()[0].id;
        let clip = clip_ids(&tl, "V1")[0];
        tl.set_clip_props(
            track,
            clip,
            ClipProps {
                gain_db: -6.0,
                ..ClipProps::default()
            },
        )
        .unwrap();
        let c = tl.find_clip(clip).unwrap().1.clone();
        let EditCommand::SetProps { props, .. } = gain(track, &c, -3.0) else {
            panic!("expected SetProps");
        };
        assert_eq!(props.gain_db, -3.0, "set, not added to");
    }

    #[test]
    fn normalising_measures_only_the_part_of_the_source_the_clip_uses() {
        // Hop 0..30 is quiet, 30..60 is loud; a clip that starts at source
        // frame 0 for 30 frames (500 ms at 60fps) must not see the loud part.
        let mut hops = vec![(-30.0f32, -30.0f32); 50];
        for h in hops.iter_mut().skip(50) {
            *h = (0.0, 0.0);
        }
        let a = analysis(&hops, &[]);
        let tl = media_fixture(&[(0, 30, 0, 600)]);
        let clip = &tl.tracks()[0].clips()[0];
        let g = normalize_gain(clip, &a, Fps::FPS_60, -12.0).expect("measurable");
        assert!((g - 18.0).abs() < 0.01, "expected +18 dB, got {g}");
    }

    #[test]
    fn loud_spans_are_the_complement_of_silence_in_timeline_time() {
        // Silence 0..500 ms then sound: at 60 fps that is frame 30 onwards.
        let a = analysis(&[(0.0, 0.0); 100], &[(0, 500)]);
        let tl = media_fixture(&[(0, 60, 0, 600)]);
        let track = track_id(&tl, "V1");
        assert_eq!(loud_spans(&tl, track, &a), vec![(Frame(30), Frame(60))]);
    }

    #[test]
    fn a_clip_offset_into_its_source_maps_silence_through_its_in_point() {
        // The clip starts 500 ms into the source, where the sound starts, so
        // the whole clip is loud even though the source begins in silence.
        let a = analysis(&[(0.0, 0.0); 100], &[(0, 500)]);
        let tl = media_fixture(&[(0, 60, 30, 600)]);
        let track = track_id(&tl, "V1");
        assert_eq!(loud_spans(&tl, track, &a), vec![(Frame(0), Frame(60))]);
    }

    #[test]
    fn ducking_splits_around_the_span_and_lowers_only_the_middle() {
        let tl = media_fixture(&[(0, 100, 0, 600)]);
        let track = track_id(&tl, "V1");
        let spans = [(Frame(20), Frame(50))];
        let mut ids = (100..).map(|n| n as u64);
        let plan = duck_plan(&tl, track, &spans, -12.0, &mut ids).unwrap();
        let EditCommand::Sequence(cmds) = plan else {
            panic!("a duck is one undo step");
        };
        assert_eq!(cmds.len(), 3, "two cuts and one gain change: {cmds:?}");
        assert!(matches!(
            cmds[0],
            EditCommand::Split {
                frame: Frame(20),
                ..
            }
        ));
        assert!(matches!(
            cmds[1],
            EditCommand::Split {
                frame: Frame(50),
                ..
            }
        ));
        match &cmds[2] {
            EditCommand::SetProps { clip, props, .. } => {
                assert_eq!(*clip, ClipId(100), "the middle piece is the one ducked");
                assert_eq!(props.gain_db, -12.0);
            }
            other => panic!("expected SetProps, got {other:?}"),
        }
    }

    #[test]
    fn a_span_covering_a_whole_clip_needs_no_cuts() {
        let tl = media_fixture(&[(0, 100, 0, 600)]);
        let track = track_id(&tl, "V1");
        let mut ids = (100..).map(|n| n as u64);
        let plan = duck_plan(&tl, track, &[(Frame(0), Frame(100))], -6.0, &mut ids).unwrap();
        let EditCommand::Sequence(cmds) = plan else {
            panic!("expected a sequence");
        };
        assert_eq!(cmds.len(), 1, "no split is a no-op the model would reject");
    }

    #[test]
    fn a_duck_that_touches_nothing_is_refused_before_it_mutates() {
        let tl = media_fixture(&[(0, 100, 0, 600)]);
        let track = track_id(&tl, "V1");
        let mut ids = (100..).map(|n| n as u64);
        assert!(duck_plan(&tl, track, &[(Frame(200), Frame(300))], -6.0, &mut ids).is_err());
    }
}
