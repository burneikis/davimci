//! Proxy media, wired to a live session.
//!
//! `davimci-analysis` decides *whether* a file needs a proxy and *how* to
//! encode it; this is the layer that runs that decision for an import and
//! resolves a clip's media through the result.
//!
//! A proxy is a decoding detail, not an edit: the timeline always names the
//! original, and the substitution happens on the way to the preview graph.
//! That is what keeps a background encode out of the undo log, out of `.`,
//! out of the project file and out of every export.
//!
//! The mechanism is the host's, because encoding is; the *policy* - whether
//! to proxy at all, above what resolution, in what codec - is not, so it
//! starts off. `:set proxy on|off` is the manual switch and the bundled
//! `proxies` plugin is the standing opinion.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use davimci_analysis::{
    AnalysisCache, JobEvent, JobRunner, MediaInfo, Prober, ProxyMap, ProxyPolicy,
};
use davimci_app::{JobState, JobUpdate};
use davimci_core::{Timeline, TimelineProps};

use crate::error::CliError;

/// A proxy that has finished encoding and is waiting to be swapped in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ready {
    source: String,
    proxy: String,
}

/// The proxy policy, the encodes it started, and which proxy stands in for
/// which original.
pub struct Proxies {
    policy: ProxyPolicy,
    runner: JobRunner,
    cache: AnalysisCache,
    /// Finished encodes, written by job threads and drained on the tick.
    inbox: Arc<Mutex<Vec<Ready>>>,
    map: ProxyMap,
    updates: Vec<JobUpdate>,
    /// Sources with an encode already running, by job. Two encodes of one
    /// file write the same partial container and race each other to rename
    /// it, so a second import of the same media joins the first job instead
    /// of starting one.
    encoding: BTreeMap<u64, String>,
    /// Encodes waiting for a slot, oldest first.
    queued: VecDeque<(String, Option<MediaInfo>, TimelineProps)>,
}

/// How many proxies encode at once. Two keeps a core or two for the editor
/// itself; the rest wait rather than fighting the preview for the machine.
const MAX_CONCURRENT_ENCODES: usize = 2;

impl std::fmt::Debug for Proxies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxies")
            .field("enabled", &self.policy.auto)
            .finish_non_exhaustive()
    }
}

impl Proxies {
    #[must_use]
    pub fn new(project_dir: &Path) -> Self {
        Self {
            // Off until something asks. A proxy policy is a workflow
            // opinion, and generating one costs a whole transcode of the
            // import; a session that never asked must not pay for it.
            policy: ProxyPolicy::disabled(),
            runner: JobRunner::new(),
            cache: AnalysisCache::for_project(project_dir),
            inbox: Arc::new(Mutex::new(Vec::new())),
            map: ProxyMap::default(),
            updates: Vec::new(),
            encoding: BTreeMap::new(),
            queued: VecDeque::new(),
        }
    }

    /// Adopt a policy a config or plugin stated. Whether it is `auto` is
    /// part of it: a plugin that sets thresholds and leaves them off is
    /// stating a preference for later, not a contradiction.
    pub fn set_policy(&mut self, policy: ProxyPolicy) {
        if !policy.auto {
            self.runner.cancel_all();
            self.encoding.clear();
            self.queued.clear();
        }
        self.policy = policy;
    }

    /// The policy in force, so a plugin can amend rather than restate it.
    #[must_use]
    pub fn policy(&self) -> ProxyPolicy {
        self.policy.clone()
    }

    /// `:set proxy on|off`. Turning proxies off stops the encodes that have
    /// not finished; the clips already standing on a proxy keep it until
    /// they are relinked, and export relinks them regardless.
    pub fn set_enabled(&mut self, on: bool) -> String {
        self.policy.auto = on;
        if on {
            "proxy on".into()
        } else {
            self.runner.cancel_all();
            self.encoding.clear();
            self.queued.clear();
            "proxy off".into()
        }
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.policy.auto
    }

    /// Queue the proxy an imported file needs, if it needs one.
    ///
    /// Whether a file qualifies is a question about its streams and costs
    /// nothing, so it is answered here. *Where* the proxy goes is keyed by a
    /// hash of the file's contents, which is a full read of what may be
    /// gigabytes - that, and the encode, belong on the worker, or an import
    /// stalls the session for as long as it takes to read the media.
    pub fn queue_for_import(&mut self, info: &MediaInfo, props: TimelineProps) -> Option<String> {
        if !self.policy.needs_proxy(info.video()?) {
            return None;
        }
        self.enqueue(info.path.clone(), Some(info.clone()), props)
    }

    /// Queue the proxy a source the project *already* references needs.
    ///
    /// A proxy is queued on import, but a project opened a second time
    /// imports nothing: its media is already on the timeline. Without this,
    /// a session whose encode was interrupted - or which was authored before
    /// the policy said so - never gets the proxy it is entitled to.
    ///
    /// The file is probed on the worker, not here: opening a project must
    /// not stall on one `ffprobe` per clip.
    pub fn queue_for_source(&mut self, source: &str, props: TimelineProps) -> Option<String> {
        if !self.policy.auto || self.map.proxy_for(source).is_some() {
            return None;
        }
        self.enqueue(source.to_string(), None, props)
    }

    /// Start an encode, or hold it until one of the running ones is done.
    ///
    /// `info` is what a probe already found, so an import does not pay for a
    /// second one; a sweep of an open project passes `None` and the worker
    /// probes.
    fn enqueue(
        &mut self,
        source: String,
        info: Option<MediaInfo>,
        props: TimelineProps,
    ) -> Option<String> {
        if self.encoding.values().any(|s| *s == source)
            || self.queued.iter().any(|(s, _, _)| *s == source)
        {
            return None;
        }
        let label = format!("encoding a proxy for {}", file_name(&source));
        // Encodes are the heaviest thing davimci runs. Opening a project of
        // fifty heavy sources must not start fifty ffmpegs and leave the
        // machine unable to play back what it is editing.
        if self.encoding.len() >= MAX_CONCURRENT_ENCODES {
            self.queued.push_back((source, info, props));
            return Some(label);
        }
        self.spawn(source, info, props, label.clone());
        Some(label)
    }

    fn spawn(
        &mut self,
        source: String,
        info: Option<MediaInfo>,
        props: TimelineProps,
        label: String,
    ) {
        let inbox = Arc::clone(&self.inbox);
        let policy = self.policy.clone();
        let root = self.cache.root().to_path_buf();
        let path = source.clone();
        let id = self.runner.spawn(label, move |ctx| {
            let info = match info {
                Some(info) => info,
                None => davimci_analysis::FfprobeProber.probe(Path::new(&path))?,
            };
            // The policy is re-read here because a swept source has not been
            // judged yet: only an import knows its streams up front.
            let qualifies = info.video().is_some_and(|video| policy.needs_proxy(video));
            if !qualifies {
                return Ok(());
            }
            let conformed = davimci_analysis::conform::conform(
                &info,
                props,
                davimci_analysis::conform::ConformOptions::default(),
            );
            // Content-hashing gigabytes is minutes before ffmpeg is even
            // started, so it owns the first slice of the bar.
            let whole = davimci_analysis::Phase::whole(Some(ctx));
            let hash =
                davimci_analysis::cache::hash_file(Path::new(&info.path), whole.slice(0, 150))?;
            let Some(spec) =
                davimci_analysis::proxy::plan_proxy(&info, &conformed, &policy, &root, &hash)
            else {
                return Ok(());
            };
            let ready = Ready {
                source: spec.source.clone(),
                proxy: spec.path.display().to_string(),
            };
            // An encode that has already been done is reused: the cache is
            // keyed by content, so a re-import of the same file is free. A
            // cached file that will not decode is thrown away rather than
            // handed to the preview, which is what a truncated container
            // from an interrupted encode looks like.
            if spec.path.is_file() && !davimci_analysis::proxy::is_usable(&spec.path) {
                let _ = std::fs::remove_file(&spec.path);
            }
            if !spec.path.is_file() {
                davimci_analysis::proxy::generate(&spec, whole.slice(150, 1000))?;
            }
            if let Ok(mut q) = inbox.lock() {
                q.push(ready);
            }
            Ok(())
        });
        self.encoding.insert(id.0, source);
    }

    fn adopt(&mut self, ready: &Ready) {
        self.map.insert(ready.proxy.clone(), ready.source.clone());
    }

    /// Job progress for the status line, and the proxies that landed since
    /// the last call as `(original, proxy)` pairs for the caller to relink.
    pub fn poll(&mut self) -> (Vec<JobUpdate>, Vec<(String, String)>) {
        for event in self.runner.poll() {
            let id = event.job().0;
            match event {
                JobEvent::Started { label, .. } => {
                    self.updates.push(JobUpdate::Started { id, label });
                }
                _ if event.is_terminal() => {
                    self.encoding.remove(&id);
                    // A slot came free; whatever was waiting for it starts.
                    if let Some((source, info, props)) = self.queued.pop_front() {
                        let label = format!("encoding a proxy for {}", file_name(&source));
                        self.spawn(source, info, props, label);
                    }
                    self.updates.push(JobUpdate::Finished {
                        id,
                        state: terminal_state(&event),
                    });
                }
                JobEvent::Progress { done, total, .. } => {
                    let permille = u16::try_from(
                        done.checked_mul(1000)
                            .and_then(|n| n.checked_div(total))
                            .unwrap_or(0)
                            .min(999),
                    )
                    .unwrap_or(999);
                    self.updates.push(JobUpdate::Progress { id, permille });
                }
                // Every terminal event is handled above.
                _ => {}
            }
        }
        let ready: Vec<Ready> = self
            .inbox
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        let mut swaps = Vec::new();
        for r in ready {
            swaps.push((r.source.clone(), r.proxy.clone()));
            self.adopt(&r);
        }
        (std::mem::take(&mut self.updates), swaps)
    }

    /// The timeline as the *preview* decodes it: every source that has a
    /// finished proxy replaced by that proxy.
    ///
    /// The substitution lives here and nowhere else. The timeline the
    /// session holds always names the original, so a proxy is never
    /// recorded in the undo log, never repeated by `.`, never written to the
    /// project file and never exported. Borrowed until there is something to
    /// swap, so a session without proxies pays nothing.
    #[must_use]
    pub fn with_proxies<'a>(&self, tl: &'a Timeline) -> Cow<'a, Timeline> {
        if self.map.is_empty() {
            return Cow::Borrowed(tl);
        }
        let swaps: Vec<(davimci_core::ClipId, String)> = tl
            .tracks()
            .iter()
            // Video tracks only. A proxy is encoded without audio, so an
            // audio clip pointed at one plays silence: the substitution that
            // makes the picture cheap must not take the sound away.
            .filter(|t| t.kind == davimci_core::TrackKind::Video)
            .flat_map(davimci_core::Track::clips)
            .filter_map(|c| {
                let media = c.media.as_ref()?;
                let proxy = self.map.proxy_for(&media.path)?;
                Path::new(proxy)
                    .is_file()
                    .then(|| (c.id, proxy.to_string()))
            })
            .collect();
        if swaps.is_empty() {
            return Cow::Borrowed(tl);
        }
        let mut out = tl.clone();
        for (clip, proxy) in swaps {
            // A proxy that will not relink leaves the clip on its original,
            // which decodes slower and is otherwise identical.
            let _ = out.set_media_source(clip, &proxy, false);
        }
        Cow::Owned(out)
    }

    /// The built-in `BeforeExport` check: no clip may resolve to a proxy.
    pub fn check_export(&self, tl: &Timeline) -> Result<(), CliError> {
        davimci_analysis::export_guard(tl, &self.map).map_err(CliError::from)
    }

    /// Stop every encode: closing a project cancels the work it started.
    pub fn cancel_all(&mut self) {
        self.runner.cancel_all();
        self.encoding.clear();
        self.queued.clear();
    }
}

/// How a job ended, for the status line. A proxy that will not encode costs
/// the session its faster decode and nothing else: the original is still
/// there.
fn terminal_state(event: &JobEvent) -> JobState {
    match event {
        JobEvent::Cancelled { .. } => JobState::Cancelled,
        JobEvent::Failed { .. } => JobState::Failed,
        _ => JobState::Done,
    }
}

fn file_name(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_analysis::{StreamInfo, StreamKind};
    use davimci_core::{Fps, Resolution};

    fn dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("davimci-proxy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn uhd(path: &str) -> MediaInfo {
        MediaInfo {
            path: path.into(),
            duration_seconds: 2.0,
            streams: vec![StreamInfo {
                index: 0,
                kind: StreamKind::Video,
                codec: "h264".into(),
                title: None,
                language: None,
                fps: Some(Fps::FPS_25),
                resolution: Some(Resolution {
                    width: 3840,
                    height: 2160,
                }),
                sample_rate: None,
                channels: None,
                frames: Some(50),
                bit_depth: Some(8),
            }],
        }
    }

    /// Regression: importing one file twice ran two encodes of it, and both
    /// wrote the same partial container and raced to rename it.
    #[test]
    fn a_second_import_of_the_same_file_joins_the_encode_already_running() {
        let root = dir("twice");
        let source = root.join("uhd.mkv");
        std::fs::write(&source, b"not really media, but it hashes").unwrap();
        let info = uhd(&source.display().to_string());
        let props = TimelineProps::default();

        let mut proxies = Proxies::new(&root);
        proxies.set_enabled(true);
        assert!(proxies.queue_for_import(&info, props).is_some());
        assert!(
            proxies.queue_for_import(&info, props).is_none(),
            "the same source was queued for a second encode"
        );
        proxies.cancel_all();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The switch is the whole point of the setting: with proxies off, a 4K
    /// import plans nothing at all.
    #[test]
    fn set_proxy_off_stops_a_qualifying_import_from_planning_one() {
        let root = dir("switch");
        let source = root.join("uhd.mkv");
        std::fs::write(&source, b"not really media, but it hashes").unwrap();
        let info = uhd(&source.display().to_string());
        let props = TimelineProps::default();

        let mut proxies = Proxies::new(&root);
        proxies.set_enabled(false);
        assert!(
            proxies.queue_for_import(&info, props).is_none(),
            "the switch is inert"
        );
        proxies.set_enabled(true);
        assert!(
            proxies.queue_for_import(&info, props).is_some(),
            "4K wants a proxy"
        );
        proxies.cancel_all();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A proxy policy is a workflow opinion, so a session that stated none
    /// transcodes nothing however heavy the import is.
    #[test]
    fn a_fresh_session_proxies_nothing_until_asked() {
        let root = dir("unasked");
        let source = root.join("uhd.mkv");
        std::fs::write(&source, b"not really media, but it hashes").unwrap();
        let info = uhd(&source.display().to_string());

        let mut proxies = Proxies::new(&root);
        assert!(!proxies.enabled());
        assert!(
            proxies
                .queue_for_import(&info, TimelineProps::default())
                .is_none(),
            "an unasked session started a transcode"
        );
        proxies.cancel_all();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression: the proxy path is keyed by a hash of the file's
    /// contents, and computing it on the import thread froze the session for
    /// as long as it took to read the media - seconds, on the multi-gigabyte
    /// files that are exactly the ones wanting a proxy. Queueing must not
    /// touch the bytes, which shows as a file that cannot be read at all
    /// still being queued.
    #[test]
    fn queueing_a_proxy_never_reads_the_source_on_the_calling_thread() {
        let root = dir("noread");
        let info = uhd(&root.join("not-on-disk.mkv").display().to_string());
        let mut proxies = Proxies::new(&root);
        proxies.set_enabled(true);
        assert!(
            proxies
                .queue_for_import(&info, TimelineProps::default())
                .is_some(),
            "queueing read the source instead of deferring it to the worker"
        );
        proxies.cancel_all();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The substitution is the preview's alone: the session's timeline still
    /// names the original, so nothing an export, a save or an undo looks at
    /// has ever seen a proxy.
    #[test]
    fn a_proxy_reaches_the_preview_and_no_further() {
        let root = dir("preview");
        let proxy = root.join("abc.proxy.mov");
        std::fs::write(&proxy, b"stand-in").unwrap();
        let proxy = proxy.display().to_string();

        let mut proxies = Proxies::new(&root);
        let mut tl = davimci_core::testing::multi_audio_fixture(1, Some(1));
        let track = tl
            .tracks()
            .iter()
            .find(|t| t.kind == davimci_core::TrackKind::Video)
            .unwrap()
            .id;
        let clip = tl.track(track).unwrap().clips()[0].id;
        tl.set_media_source(clip, "/media/uhd.mkv", false).unwrap();

        let media_of = |tl: &Timeline| {
            tl.track(track).unwrap().clips()[0]
                .media
                .as_ref()
                .map(|m| m.path.clone())
                .unwrap()
        };

        // Before the encode lands, nothing is substituted and nothing is
        // cloned.
        assert!(matches!(proxies.with_proxies(&tl), Cow::Borrowed(_)));

        proxies.adopt(&Ready {
            source: "/media/uhd.mkv".into(),
            proxy: proxy.clone(),
        });
        assert_eq!(media_of(&proxies.with_proxies(&tl)), proxy);
        assert_eq!(
            media_of(&tl),
            "/media/uhd.mkv",
            "the substitution reached the timeline the session holds"
        );
        // The guard sees the timeline an export ships, which is that one.
        proxies.check_export(&tl).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression: the preview went silent once a proxy landed. Audio clips
    /// off the same file were relinked to it too, and a proxy is encoded
    /// with `-an`: there was nothing there to play.
    #[test]
    fn an_audio_clip_keeps_its_original_when_the_video_takes_a_proxy() {
        let root = dir("audio");
        let proxy = root.join("abc.proxy.mov");
        std::fs::write(&proxy, b"stand-in").unwrap();
        let proxy = proxy.display().to_string();

        let mut proxies = Proxies::new(&root);
        let mut tl = davimci_core::testing::multi_audio_fixture(1, Some(1));
        // One file, playing on both a video and an audio track, as any
        // imported clip with sound does.
        let ids: Vec<(
            davimci_core::TrackId,
            davimci_core::ClipId,
            davimci_core::TrackKind,
        )> = tl
            .tracks()
            .iter()
            .filter_map(|t| t.clips().first().map(|c| (t.id, c.id, t.kind)))
            .collect();
        for (_, clip, _) in &ids {
            tl.set_media_source(*clip, "/media/uhd.mkv", false).unwrap();
        }
        proxies.adopt(&Ready {
            source: "/media/uhd.mkv".into(),
            proxy: proxy.clone(),
        });

        let previewed = proxies.with_proxies(&tl);
        for (track, _, kind) in ids {
            let path = previewed.track(track).unwrap().clips()[0]
                .media
                .as_ref()
                .map(|m| m.path.clone())
                .unwrap();
            match kind {
                davimci_core::TrackKind::Video => {
                    assert_eq!(path, proxy, "the picture did not take the proxy");
                }
                _ => assert_eq!(
                    path, "/media/uhd.mkv",
                    "the sound was taken from a video-only proxy"
                ),
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
