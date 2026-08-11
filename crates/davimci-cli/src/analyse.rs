//! Background analysis, wired to a live session.
//!
//! Measuring is not core, so nothing here runs unasked: an envelope costs a
//! full decode of the source, and a session that never draws a waveform,
//! never jumps by silence and never normalises must not pay for one. Something
//! has to [`Analyser::demand`] measurement first - a waveform lane switched
//! on, a plugin that reads hops, or a command that cannot answer without
//! them. Until then this watches the timeline and does nothing.
//!
//! Once asked, it queues one job per audio source, publishes the resulting
//! envelopes to the view state, and drops them again when a gain or fade
//! changes, since a measurement of the pre-gain signal is no longer a
//! description of what will be heard.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use davimci_analysis::{
    Analysis, AnalysisCache, AnalysisParams, JobEvent, JobRunner, StreamKind,
    pipeline::{AnalysisReady, AnalysisRequest},
};
use davimci_app::{JobState, JobUpdate, Waveform};
use davimci_core::{Timeline, TrackId, TrackKind};

/// The sample rate analysis decodes to. Fixed: the measurements are relative
/// (dBFS over a 10 ms hop), so the rate only has to be consistent.
const ANALYSIS_RATE: u32 = 48_000;

/// What identifies a source for analysis: the file and the stream in it.
type Source = (PathBuf, u32);

/// Analysis of every audio track in the current timeline.
pub struct Analyser {
    runner: JobRunner,
    cache: AnalysisCache,
    params: AnalysisParams,
    /// Results, once they land. Written by job threads, drained on the tick.
    inbox: Arc<Mutex<Vec<AnalysisReady>>>,
    /// The finished analysis per track, for `:normalize` and `:duck`.
    analyses: BTreeMap<TrackId, Analysis>,
    /// What has already been queued, so a repaint does not re-queue it.
    requested: BTreeMap<TrackId, Source>,
    /// Signature of each track's audible properties, to notice a change.
    signatures: BTreeMap<TrackId, u64>,
    /// Who is asking to be measured, by name. Empty means nobody is, and
    /// nothing decodes.
    demands: BTreeSet<String>,
    /// Tracks whose published envelope is stale, waiting to be reported.
    stale: Vec<TrackId>,
    updates: Vec<JobUpdate>,
    labels: BTreeMap<u64, String>,
}

impl std::fmt::Debug for Analyser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analyser")
            .field("analysed", &self.analyses.len())
            .field("queued", &self.requested.len())
            .finish_non_exhaustive()
    }
}

impl Analyser {
    #[must_use]
    pub fn new(project_dir: &std::path::Path) -> Self {
        Self {
            runner: JobRunner::new(),
            cache: AnalysisCache::for_project(project_dir),
            params: AnalysisParams::default(),
            inbox: Arc::new(Mutex::new(Vec::new())),
            analyses: BTreeMap::new(),
            requested: BTreeMap::new(),
            signatures: BTreeMap::new(),
            demands: BTreeSet::new(),
            stale: Vec::new(),
            updates: Vec::new(),
            labels: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn params(&self) -> AnalysisParams {
        self.params
    }

    /// A track's finished analysis, if it has one.
    #[must_use]
    pub fn analysis(&self, track: TrackId) -> Option<&Analysis> {
        self.analyses.get(&track)
    }

    /// Ask for measurement in `reason`'s name, and say whether that was new.
    ///
    /// A reason is held until it is [released](Self::release), so two askers
    /// cannot switch each other off: the waveform lane going away does not
    /// stop the silence plugin being able to jump.
    pub fn demand(&mut self, reason: &str) -> bool {
        self.demands.insert(reason.to_string())
    }

    /// Withdraw one reason. Measuring stops when the last one goes.
    pub fn release(&mut self, reason: &str) -> bool {
        self.demands.remove(reason)
    }

    /// Whether anything is asking to be measured.
    #[must_use]
    pub fn is_demanded(&self) -> bool {
        !self.demands.is_empty()
    }

    /// Bring analysis in step with the timeline.
    ///
    /// Called after every edit: with something asking, a new audio track is
    /// queued and one whose gain or fades changed is invalidated and queued
    /// again. Both are cheap enough to do on an edit and wrong to do on a
    /// timer. With nothing asking, this is the whole of the work.
    pub fn sync(&mut self, tl: &Timeline) {
        if self.demands.is_empty() {
            return;
        }
        for track in tl.tracks() {
            if track.kind != TrackKind::Audio {
                continue;
            }
            let Some(source) = source_of(track) else {
                continue;
            };
            let signature = audible_signature(track);
            let changed = self.signatures.insert(track.id, signature) != Some(signature);
            let known = self.requested.get(&track.id) == Some(&source);
            if known && !changed {
                continue;
            }
            if changed && known {
                // The old measurement described the pre-gain signal.
                self.analyses.remove(&track.id);
                self.stale.push(track.id);
            }
            self.requested.insert(track.id, source.clone());
            self.queue(track.id, source);
        }
    }

    /// Re-run analysis for every audio track (`:analyze`).
    ///
    /// Asking to re-measure is itself a demand: `:analyze` in a session that
    /// has never measured anything has to measure rather than report that
    /// there is nothing to do. Everything held is dropped first, so `sync`
    /// sees every track as new work.
    pub fn reanalyse(&mut self, tl: &Timeline) -> usize {
        self.demand("command");
        self.stale.extend(self.requested.keys().copied());
        self.analyses.clear();
        self.requested.clear();
        self.signatures.clear();
        self.sync(tl);
        self.requested.len()
    }

    fn queue(&mut self, track: TrackId, (path, stream): Source) {
        let request = AnalysisRequest {
            track,
            path: path.clone(),
            stream,
            kind: StreamKind::Audio,
        };
        let inbox = Arc::clone(&self.inbox);
        let params = self.params;
        let cache = self.cache.clone();
        let label = format!(
            "analysing {}",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into()
            )
        );
        let id = self.runner.spawn(label.clone(), move |ctx| {
            let analysis = davimci_analysis::pipeline::analyse(
                &request,
                params,
                &cache,
                ANALYSIS_RATE,
                Some(ctx),
            )?;
            if let Ok(mut q) = inbox.lock() {
                q.push(AnalysisReady { track, analysis });
            }
            Ok(())
        });
        self.labels.insert(id.0, label);
    }

    /// Collect finished work: job updates for the status line, and the
    /// envelopes the view state draws.
    pub fn poll(&mut self) -> (Vec<JobUpdate>, Vec<(TrackId, Waveform)>) {
        for event in self.runner.poll() {
            let id = event.job().0;
            match event {
                JobEvent::Started { label, .. } => {
                    self.updates.push(JobUpdate::Started { id, label });
                }
                JobEvent::Progress { done, total, .. } => {
                    let permille = done
                        .checked_mul(1000)
                        .and_then(|n| n.checked_div(total))
                        .unwrap_or(0)
                        .min(999) as u16;
                    self.updates.push(JobUpdate::Progress { id, permille });
                }
                JobEvent::Finished { .. } => self.updates.push(JobUpdate::Finished {
                    id,
                    state: JobState::Done,
                }),
                JobEvent::Cancelled { .. } => self.updates.push(JobUpdate::Finished {
                    id,
                    state: JobState::Cancelled,
                }),
                // Phase 0: a failed analysis degrades locally. Editing
                // continues; that track simply has no envelope.
                JobEvent::Failed { .. } => self.updates.push(JobUpdate::Finished {
                    id,
                    state: JobState::Failed,
                }),
            }
        }
        let ready: Vec<AnalysisReady> = self
            .inbox
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        let mut waves = Vec::new();
        for r in ready {
            let peaks: Vec<f32> = r.analysis.hops.iter().map(|h| h.peak_db).collect();
            waves.push((r.track, Waveform::from_db(r.analysis.params.hop_ms, &peaks)));
            self.analyses.insert(r.track, r.analysis);
        }
        (std::mem::take(&mut self.updates), waves)
    }

    /// Tracks whose published envelope no longer describes what is heard.
    pub fn take_stale(&mut self) -> Vec<TrackId> {
        std::mem::take(&mut self.stale)
    }

    /// Stop everything: closing a project cancels the work it started.
    pub fn cancel_all(&mut self) {
        self.runner.cancel_all();
    }
}

/// The file and stream an audio track plays, when every clip on it agrees.
///
/// Analysis is per source, so a track holding two different files has no
/// single envelope; that is a lane davimci does not draw rather than one it
/// draws wrongly.
fn source_of(track: &davimci_core::Track) -> Option<Source> {
    let mut found: Option<Source> = None;
    for clip in track.clips() {
        let media = clip.media.as_ref()?;
        if media.offline {
            return None;
        }
        let source = (PathBuf::from(&media.path), media.stream.unwrap_or(0));
        match &found {
            Some(existing) if *existing != source => return None,
            _ => found = Some(source),
        }
    }
    found
}

/// A cheap fingerprint of everything on a track that changes what is heard.
fn audible_signature(track: &davimci_core::Track) -> u64 {
    let mut h: u64 = 14_695_981_039_346_656_037;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(1_099_511_628_211);
    };
    for clip in track.clips() {
        mix(clip.id.0);
        mix(clip.props.gain_db.to_bits().into());
        mix(clip.props.fade_in.get());
        mix(clip.props.fade_out.get());
        mix(clip.source_in.get());
        mix(clip.duration.get());
    }
    h
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_core::testing::{clip_ids, multi_audio_fixture, track_id};
    use davimci_core::{ClipProps, Frame};

    fn tmpdir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("davimci-analyse-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_track_of_two_different_files_has_no_single_envelope() {
        // Drawing one source's waveform under another's audio is worse than
        // drawing nothing.
        let mut tl = multi_audio_fixture(1, Some(2));
        let track = tl
            .tracks()
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .unwrap()
            .id;
        assert!(source_of(tl.track(track).unwrap()).is_some());
        let clip = tl.track(track).unwrap().clips()[0].id;
        tl.set_media_source(clip, "/media/other.mkv", false)
            .unwrap();
        let cid = tl.new_clip_id();
        let mut second = tl.track(track).unwrap().clips()[0].clone();
        second.id = cid;
        second.start = Frame::ZERO;
        second.media.as_mut().unwrap().path = "/media/third.mkv".into();
        tl.restore(track, Frame(200), &[second], Frame(100), false)
            .unwrap();
        assert_eq!(source_of(tl.track(track).unwrap()), None);
    }

    #[test]
    fn changing_gain_marks_the_envelope_stale() {
        // Gain invalidates the analysis for that clip.
        let dir = tmpdir("stale");
        let mut a = Analyser::new(&dir);
        a.demand("test");
        let mut tl = multi_audio_fixture(1, Some(2));
        a.sync(&tl);
        assert!(a.take_stale().is_empty(), "nothing was published yet");

        let track = tl
            .tracks()
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .unwrap()
            .id;
        let clip = clip_ids(&tl, "A1")[0];
        tl.set_clip_props(
            track,
            clip,
            ClipProps {
                gain_db: -6.0,
                ..ClipProps::default()
            },
        )
        .unwrap();
        a.sync(&tl);
        assert_eq!(a.take_stale(), vec![track], "gain left a stale envelope");
        a.cancel_all();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `:analyze`: every envelope is dropped and re-queued, so a
    /// predicate motion answers `Pending` again until the work lands.
    #[test]
    fn analyze_drops_every_envelope_and_requeues_the_work() {
        let dir = tmpdir("reanalyse");
        let mut a = Analyser::new(&dir);
        a.demand("test");
        let tl = multi_audio_fixture(1, Some(2));
        a.sync(&tl);
        let track = tl
            .tracks()
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .unwrap()
            .id;
        let _ = a.take_stale();
        assert_eq!(a.reanalyse(&tl), 1, ":analyze re-queued the audio track");
        assert!(
            a.analysis(track).is_none(),
            "the old envelope survived :analyze"
        );
        assert_eq!(a.take_stale(), vec![track], "the view was not invalidated");
        a.cancel_all();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_timeline_is_not_analysed_twice() {
        let dir = tmpdir("once");
        let mut a = Analyser::new(&dir);
        a.demand("test");
        let tl = multi_audio_fixture(1, Some(2));
        a.sync(&tl);
        let queued = a.requested.len();
        a.sync(&tl);
        a.sync(&tl);
        assert_eq!(a.requested.len(), queued);
        assert!(a.take_stale().is_empty());
        a.cancel_all();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Measuring is not core: a session nothing asks of decodes nothing,
    /// however much audio the timeline holds.
    #[test]
    fn nothing_is_measured_until_something_asks() {
        let dir = tmpdir("unasked");
        let mut a = Analyser::new(&dir);
        let tl = multi_audio_fixture(1, Some(2));
        a.sync(&tl);
        assert!(!a.is_demanded());
        assert!(a.requested.is_empty(), "an unasked session decoded audio");

        assert!(a.demand("waveform"));
        a.sync(&tl);
        assert_eq!(a.requested.len(), 1, "asking did not queue the work");

        // One asker leaving does not stop another: the demands are held by
        // name, not counted.
        assert!(a.demand("silence"));
        assert!(a.release("waveform"));
        assert!(a.is_demanded());
        assert!(a.release("silence"));
        assert!(!a.is_demanded());
        a.cancel_all();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_video_track_is_never_queued_for_a_waveform() {
        let dir = tmpdir("video");
        let mut a = Analyser::new(&dir);
        a.demand("test");
        let tl = multi_audio_fixture(2, Some(2));
        a.sync(&tl);
        let video = track_id(&tl, "V1");
        assert!(!a.requested.contains_key(&video));
        assert_eq!(a.requested.len(), 2);
        a.cancel_all();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
