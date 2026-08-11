//! Import + analysis, wired together.
//!
//! The order matters and is the whole point of the phase: probe, conform,
//! import, then *return*. Analysis and proxy generation are queued behind the
//! editor, so editing is allowed immediately and predicate motions report
//! `Pending` until the relevant track is ready.

use std::path::{Path, PathBuf};

use davimci_core::TrackId;

use crate::analysis::{Analysis, AnalysisParams, analyze_samples};
use crate::cache::{AnalysisCache, entry_key};
use crate::decode;
use crate::error::AnalysisError;
use crate::jobs::{JobContext, JobId, JobRunner, Phase};
use crate::probe::StreamKind;

/// One track's analysis request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisRequest {
    pub track: TrackId,
    pub path: PathBuf,
    /// Stream index within the container.
    pub stream: u32,
    pub kind: StreamKind,
}

/// A finished analysis, ready to publish into an [`crate::index::AnalysisIndex`].
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisReady {
    pub track: TrackId,
    pub analysis: Analysis,
}

/// Analyse one stream, using the cache when it is valid.
///
/// The cache entry belongs to a stream, not a file: several streams of one
/// container hash alike, so a file-wide key would hand every audio track the
/// first stream's envelope.
///
/// A cache hit skips decoding entirely. A miss, a version bump, or a corrupt
/// entry all recompute; a cache that cannot be *written* is a warning, not a
/// failure, because the analysis itself is still good.
pub fn analyse(
    request: &AnalysisRequest,
    params: AnalysisParams,
    cache: &AnalysisCache,
    sample_rate: u32,
    ctx: Option<&JobContext>,
) -> Result<Analysis, AnalysisError> {
    let check = |ctx: Option<&JobContext>| ctx.map_or(Ok(()), JobContext::check);
    check(ctx)?;
    // Hashing gigabytes is a third of a short analysis, so it owns the first
    // third of the bar rather than reporting nothing until ffmpeg starts.
    let whole = Phase::whole(ctx);
    let hash = entry_key(
        &crate::cache::hash_file(&request.path, whole.slice(0, 300))?,
        request.stream,
        request.kind,
    );
    if let Some(hit) = cache.load(&hash)
        && hit.params == params
    {
        // A hit still cost the file read the hash needed, so the bar is
        // finished rather than abandoned a third of the way along.
        whole.report(1, 1);
        return Ok(hit);
    }

    check(ctx)?;
    let mut analysis = match request.kind {
        StreamKind::Audio => {
            let samples = decode::decode_mono(
                &request.path,
                request.stream,
                sample_rate,
                whole.slice(300, 1000),
            )?;
            check(ctx)?;
            analyze_samples(&samples, sample_rate, params)
        }
        // Video and subtitle streams carry no waveform; a video stream still
        // contributes scene changes.
        _ => Analysis::empty(&hash, params),
    };
    if request.kind == StreamKind::Video {
        check(ctx)?;
        // Scene detection is optional: losing it must not lose the waveform.
        // A cancelled detection is not a loss to absorb, though - it means
        // the editor is closing and this thread is being waited on.
        match decode::scene_changes(
            &request.path,
            decode::SCENE_THRESHOLD,
            whole.slice(300, 1000),
        ) {
            Err(AnalysisError::Cancelled) => return Err(AnalysisError::Cancelled),
            other => analysis.scene_changes = other.unwrap_or_default(),
        }
    }
    hash.clone_into(&mut analysis.source_hash);
    let _ = cache.store(&hash, &analysis);
    whole.report(1, 1);
    Ok(analysis)
}

/// Queue analysis of every imported stream, one job per track.
///
/// Returns the job ids so a frontend can show progress and cancel them; the
/// results arrive on the channel the caller polls, keyed by track.
pub fn queue_analysis(
    runner: &mut JobRunner,
    requests: Vec<AnalysisRequest>,
    params: AnalysisParams,
    cache: &AnalysisCache,
    sample_rate: u32,
    mut publish: impl FnMut(AnalysisReady) + Clone + Send + 'static,
) -> Vec<JobId> {
    let mut ids = Vec::new();
    for request in requests {
        let cache = cache.clone();
        let mut publish = publish.clone();
        let label = format!("analysing audio in {}", short(&request.path));
        ids.push(runner.spawn(label, move |ctx| {
            let analysis = analyse(&request, params, &cache, sample_rate, Some(ctx))?;
            publish(AnalysisReady {
                track: request.track,
                analysis,
            });
            Ok(())
        }));
    }
    let _ = &mut publish;
    ids
}

fn short(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::index::AnalysisIndex;
    use davimci_core::Fps;
    use davimci_motion::predicate::{Answer, Predicate, PredicateIndex};
    use davimci_motion::target::Direction;
    use std::sync::{Arc, Mutex};

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("davimci-pipe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A request pointing at a file that is not audio, so `analyse` takes the
    /// no-decode path: enough to exercise cache and job plumbing without
    /// requiring ffmpeg in the default suite.
    fn request(path: &Path) -> AnalysisRequest {
        AnalysisRequest {
            track: TrackId(3),
            path: path.to_path_buf(),
            stream: 0,
            kind: StreamKind::Subtitle,
        }
    }

    #[test]
    fn a_second_analysis_of_the_same_bytes_hits_the_cache() {
        let dir = tmpdir("hit");
        let media = dir.join("m.srt");
        std::fs::write(&media, b"1\n00:00:01,000 --> 00:00:02,000\nhi\n").unwrap();
        let cache = AnalysisCache::for_project(&dir);
        let params = AnalysisParams::default();

        let first = analyse(&request(&media), params, &cache, 48_000, None).unwrap();
        assert!(
            cache.load(&first.source_hash).is_some(),
            "nothing was cached"
        );
        let second = analyse(&request(&media), params, &cache, 48_000, None).unwrap();
        assert_eq!(first, second);

        // Different parameters must not reuse the entry.
        let other = AnalysisParams {
            hop_ms: 20,
            ..params
        };
        let third = analyse(&request(&media), other, &cache, 48_000, None).unwrap();
        assert_eq!(third.params.hop_ms, 20);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_streams_of_one_file_do_not_share_a_cache_entry() {
        // Regression: the key was the file's content hash, so every audio
        // track imported from one container drew stream 0's envelope.
        let dir = tmpdir("streams");
        let media = dir.join("m.srt");
        std::fs::write(&media, b"1\n00:00:01,000 --> 00:00:02,000\nhi\n").unwrap();
        let cache = AnalysisCache::for_project(&dir);
        let params = AnalysisParams::default();

        // Kept on the no-decode path: the key is what is under test, not the
        // decoder.
        let r0 = request(&media);
        let mut r1 = r0.clone();
        r1.stream = 1;

        let a0 = analyse(&r0, params, &cache, 48_000, None).unwrap();
        let a1 = analyse(&r1, params, &cache, 48_000, None).unwrap();
        assert_ne!(
            a0.source_hash, a1.source_hash,
            "both streams were filed under one key"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn analysing_a_missing_file_is_offline_media_not_a_crash() {
        let dir = tmpdir("offline");
        let cache = AnalysisCache::for_project(&dir);
        let err = analyse(
            &request(&dir.join("gone.wav")),
            AnalysisParams::default(),
            &cache,
            48_000,
            None,
        )
        .unwrap_err();
        assert_eq!(
            davimci_core::Classify::class(&err),
            davimci_core::ErrorClass::OfflineMedia
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Predicate motions stay `Pending` while a job is in
    /// flight, and only become answerable when its result is published.
    #[test]
    fn a_queued_analysis_publishes_and_only_then_answers() {
        let dir = tmpdir("queue");
        let media = dir.join("m.srt");
        std::fs::write(&media, b"1\n00:00:01,000 --> 00:00:02,000\nhi\n").unwrap();
        let cache = AnalysisCache::for_project(&dir);

        let index = Arc::new(Mutex::new(AnalysisIndex::new(Fps::FPS_60)));
        let track = TrackId(3);
        index.lock().unwrap().set_pending(track);

        let predicate = Predicate::SceneChange { track };
        let query = |idx: &AnalysisIndex| {
            idx.find(&predicate, davimci_core::Frame::ZERO, Direction::Forward)
        };
        assert_eq!(query(&index.lock().unwrap()), Answer::Pending);

        let sink = Arc::clone(&index);
        let mut runner = JobRunner::new();
        let publish = move |ready: AnalysisReady| {
            if let Ok(mut idx) = sink.lock() {
                idx.insert(ready.track, &ready.analysis);
            }
        };
        let ids = queue_analysis(
            &mut runner,
            vec![request(&media)],
            AnalysisParams::default(),
            &cache,
            48_000,
            publish,
        );
        assert_eq!(ids.len(), 1);
        let events = runner.join();
        assert!(
            events.iter().any(crate::jobs::JobEvent::is_terminal),
            "the job never reported a result"
        );
        // Now analysed: no scene changes in a subtitle stream is a definite
        // answer, not an unfinished one.
        assert_eq!(query(&index.lock().unwrap()), Answer::NoMatch);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
