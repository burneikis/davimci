//! Proxy media, wired to a live session.
//!
//! `davimci-analysis` decides *whether* a file needs a proxy and *how* to
//! encode it; this is the layer that runs that decision for an import and
//! resolves a clip's media through the result.
//!
//! A proxy is a decoding detail, not an edit: the timeline always names the
//! original, and the substitution happens on the way to the preview graph.
//! That is what keeps a background encode out of the undo log, out of `.`,
//! out of the project file and out of every export. `:set proxy on|off` is
//! the switch.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use davimci_analysis::{AnalysisCache, JobEvent, JobRunner, MediaInfo, ProxyMap, ProxyPolicy};
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
}

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
            policy: ProxyPolicy::default(),
            runner: JobRunner::new(),
            cache: AnalysisCache::for_project(project_dir),
            inbox: Arc::new(Mutex::new(Vec::new())),
            map: ProxyMap::default(),
            updates: Vec::new(),
        }
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
        let name = file_name(&info.path);
        let inbox = Arc::clone(&self.inbox);
        let policy = self.policy.clone();
        let root = self.cache.root().to_path_buf();
        let info = info.clone();
        self.runner.spawn(format!("proxy {name}"), move |ctx| {
            let conformed = davimci_analysis::conform::conform(
                &info,
                props,
                davimci_analysis::conform::ConformOptions::default(),
            );
            let hash = davimci_analysis::cache::content_hash(Path::new(&info.path))?;
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
            // keyed by content, so a re-import of the same file is free.
            if !spec.path.is_file() {
                davimci_analysis::proxy::generate(&spec, Some(ctx))?;
            }
            if let Ok(mut q) = inbox.lock() {
                q.push(ready);
            }
            Ok(())
        });
        Some(format!("encoding a proxy for {name}"))
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
                JobEvent::Finished { .. } => self.updates.push(JobUpdate::Finished {
                    id,
                    state: JobState::Done,
                }),
                JobEvent::Cancelled { .. } => self.updates.push(JobUpdate::Finished {
                    id,
                    state: JobState::Cancelled,
                }),
                // A proxy that will not encode costs the session its faster
                // decode and nothing else: the original is still there.
                JobEvent::Failed { .. } => self.updates.push(JobUpdate::Finished {
                    id,
                    state: JobState::Failed,
                }),
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
}
