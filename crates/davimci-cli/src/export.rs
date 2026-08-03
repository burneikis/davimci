//! Driving an export (plan.md Phase 8b).
//!
//! The backend already knows how to render; what was missing was everything
//! around it - which preset, to what file, and how the user hears about it.
//! That is this module.
//!
//! An export is a background job. MLT's consumer runs off its own thread,
//! so `start` returns immediately and `poll` reports progress; the editor
//! stays usable while a render runs, which is the whole reason jobs exist in
//! the view state at all.

use std::path::{Path, PathBuf};

use davimci_backend::{
    Container, PresetRegistry, RenderBackend, RenderJob, RenderSettings, RenderState,
};
use davimci_core::{Frame, Timeline, TrackKind};

use crate::error::CliError;

/// The most audio streams the backend can route onto one channel bus.
const MAX_SEPARATE_AUDIO_TRACKS: usize = 8;

/// Whether this timeline's audio can be kept in separate streams, and why not
/// when it cannot.
///
/// Separate streams are built by routing each track onto its own channel
/// pair, which only works for sources whose channel count is known and no
/// wider than a pair. Deciding it here, against the timeline, means the user
/// is told before the render rather than handed a file that quietly mixed.
pub fn audio_stream_plan(timeline: &Timeline) -> Result<(), String> {
    let audio: Vec<_> = timeline
        .tracks()
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .collect();
    if audio.len() < 2 {
        return Err("there is only one audio track".into());
    }
    if audio.len() > MAX_SEPARATE_AUDIO_TRACKS {
        return Err(format!(
            "a file can carry at most {MAX_SEPARATE_AUDIO_TRACKS} separate audio streams"
        ));
    }
    for track in audio {
        for clip in track.clips() {
            // A generated clip has no source and so no layout to get wrong.
            let Some(media) = clip.media.as_ref() else {
                continue;
            };
            match media.channels {
                Some(1 | 2) => {}
                Some(n) => {
                    return Err(format!(
                        "{} has {n} channels and only mono or stereo sources can be routed",
                        clip.label
                    ));
                }
                None => {
                    return Err(format!("the channel count of {} is unknown", clip.label));
                }
            }
        }
    }
    Ok(())
}

/// A job id that will not collide with anything else the app shows. Exports
/// are the only job kind so far, so they count from a fixed base.
const EXPORT_JOB_BASE: u64 = 1000;

/// What the editor needs to tell the user about a running export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportEvent {
    Progress { id: u64, permille: u16 },
    Finished { id: u64, message: String },
    Failed { id: u64, message: String },
    Cancelled { id: u64, message: String },
}

/// The preset registry plus whatever export is running.
#[derive(Debug)]
pub struct Exporter {
    presets: PresetRegistry,
    running: Option<Running>,
    next_id: u64,
}

#[derive(Debug, Clone)]
struct Running {
    id: u64,
    output: PathBuf,
    /// Reported so a finished job can say how long it was.
    total: u64,
}

impl Default for Exporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Exporter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            presets: PresetRegistry::with_builtins(),
            running: None,
            next_id: EXPORT_JOB_BASE,
        }
    }

    #[must_use]
    pub fn presets(&self) -> &PresetRegistry {
        &self.presets
    }

    pub fn presets_mut(&mut self) -> &mut PresetRegistry {
        &mut self.presets
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Lines for `:presets`.
    #[must_use]
    pub fn list_presets(&self) -> Vec<String> {
        self.presets.all().map(|p| p.summary()).collect()
    }

    /// Start an export. `preset` of `None` infers one from the output file's
    /// extension, so `:export cut.webm` does what it looks like.
    pub fn start(
        &mut self,
        backend: &mut dyn RenderBackend,
        output: &Path,
        preset: Option<&str>,
        timeline: &Timeline,
    ) -> Result<String, CliError> {
        let duration = timeline.duration();
        let (resolution, fps) = (timeline.props.resolution, timeline.props.fps);
        // One at a time: two consumers writing at once would fight over the
        // graph, and the second would silently produce a broken file.
        if let Some(r) = &self.running {
            return Err(CliError::ExportBusy {
                output: r.output.display().to_string(),
            });
        }
        if duration == Frame::ZERO {
            return Err(CliError::NothingToExport);
        }

        let preset = match preset {
            Some(name) => self.presets.get(name)?,
            None => {
                let ext = output
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.presets.for_extension(&ext)
            }
        };
        // A path with no extension gets the preset's; a path that names a
        // different one is left alone, because the user was explicit.
        let output = with_extension_if_missing(output, preset.container);
        let mut settings: RenderSettings = preset.settings(resolution, fps);
        // The preset asks; the timeline decides. A source that cannot be
        // routed mixes, and says so, rather than producing a file whose audio
        // silently collapsed to one stream.
        let plan = audio_stream_plan(timeline);
        let multitrack = settings.separate_audio_tracks && plan.is_ok();
        let mixed_reason = match (settings.separate_audio_tracks, &plan) {
            (true, Err(reason)) if audio_track_count(timeline) > 1 => Some(reason.clone()),
            _ => None,
        };
        settings.separate_audio_tracks = multitrack;

        backend
            .render(RenderJob::new(output.clone(), settings))
            .map_err(|e| CliError::ExportFailed {
                reason: e.to_string(),
            })?;

        let id = self.next_id;
        self.next_id += 1;
        self.running = Some(Running {
            id,
            output: output.clone(),
            total: duration.get(),
        });
        Ok(format!(
            "exporting {} frames to {} as {}{}",
            duration.get(),
            output.display(),
            preset.name,
            match (multitrack, &mixed_reason) {
                (true, _) => " (audio tracks stay separate)".to_string(),
                (false, Some(reason)) => {
                    format!(" (audio tracks mixed to one stream: {reason})")
                }
                (false, None) => String::new(),
            }
        ))
    }

    /// Poll the backend. Called on every tick; returns nothing when no export
    /// is running, which is the usual case.
    pub fn poll(&mut self, backend: &dyn RenderBackend) -> Option<ExportEvent> {
        let running = self.running.clone()?;
        let p = backend.progress();
        match p.state {
            RenderState::Running => Some(ExportEvent::Progress {
                id: running.id,
                permille: permille(p.rendered, p.total.max(running.total)),
            }),
            RenderState::Done => {
                self.running = None;
                Some(ExportEvent::Finished {
                    id: running.id,
                    message: format!("exported {}", running.output.display()),
                })
            }
            RenderState::Cancelled => {
                self.running = None;
                Some(ExportEvent::Cancelled {
                    id: running.id,
                    message: format!("export of {} cancelled", running.output.display()),
                })
            }
            RenderState::Failed(reason) => {
                self.running = None;
                Some(ExportEvent::Failed {
                    id: running.id,
                    message: format!(
                        "the export of {} failed: {reason}",
                        running.output.display()
                    ),
                })
            }
            RenderState::Idle => None,
        }
    }

    /// Stop the running export. A cancelled export leaves a partial file on
    /// disk on purpose - deleting a user's file is not this program's call.
    pub fn cancel(&mut self, backend: &mut dyn RenderBackend) -> Result<String, CliError> {
        let running = self.running.clone().ok_or(CliError::NoExportRunning)?;
        backend
            .cancel_render()
            .map_err(|e| CliError::ExportFailed {
                reason: e.to_string(),
            })?;
        self.running = None;
        Ok(format!(
            "cancelled the export of {}; the partial file was left in place",
            running.output.display()
        ))
    }

    /// The job id of the running export, for tests and the status line.
    #[must_use]
    pub fn running_id(&self) -> Option<u64> {
        self.running.as_ref().map(|r| r.id)
    }
}

fn audio_track_count(timeline: &Timeline) -> usize {
    timeline
        .tracks()
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .count()
}

/// Progress in tenths of a percent, never rounding up to 1000 before the
/// backend says the render is done.
fn permille(done: u64, total: u64) -> u16 {
    if total == 0 {
        return 0;
    }
    let p = done.saturating_mul(1000) / total;
    u16::try_from(p.min(999)).unwrap_or(999)
}

/// Give the output the preset's extension when it has none.
fn with_extension_if_missing(path: &Path, container: Container) -> PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension(container.extension())
    }
}

/// The default output name for `:render <preset>`: the project's name with
/// the preset's container extension (spec 12).
#[must_use]
pub fn default_output(project: Option<&Path>, container: Container) -> PathBuf {
    let stem = project
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".to_string());
    PathBuf::from(format!("{stem}.{}", container.extension()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use davimci_backend::MockBackend;
    use davimci_core::Resolution;
    use davimci_core::testing::multi_audio_fixture as tl_with_audio;

    fn backend() -> MockBackend {
        MockBackend::new(Resolution::HD_1080)
    }

    fn timeline() -> Timeline {
        tl_with_audio(2, Some(1))
    }

    fn start(e: &mut Exporter, b: &mut MockBackend, out: &str) -> Result<String, CliError> {
        e.start(b, Path::new(out), None, &timeline())
    }

    #[test]
    fn an_export_names_the_file_and_the_preset() {
        let (mut e, mut b) = (Exporter::new(), backend());
        let msg = start(&mut e, &mut b, "/tmp/out.mkv").unwrap();
        assert!(msg.contains("/tmp/out.mkv"), "{msg}");
        assert!(e.is_running());
    }

    #[test]
    fn matroska_says_that_audio_tracks_stay_separate() {
        // M3 is defined by a multi-audio MKV, so the user is told it happened.
        let (mut e, mut b) = (Exporter::new(), backend());
        let msg = start(&mut e, &mut b, "/tmp/out.mkv").unwrap();
        assert!(msg.contains("audio tracks stay separate"), "{msg}");
        let mut e2 = Exporter::new();
        let msg = start(&mut e2, &mut backend(), "/tmp/out.mp4").unwrap();
        assert!(!msg.contains("stay separate"), "{msg}");
    }

    #[test]
    fn a_source_that_cannot_be_routed_mixes_and_says_why() {
        // Mixing behind the user's back is the failure this message exists to
        // prevent: the file is still written, but it is not a multi-audio one.
        let mut e = Exporter::new();
        let tl = tl_with_audio(2, Some(6));
        let msg = e
            .start(&mut backend(), Path::new("/tmp/out.mkv"), None, &tl)
            .unwrap();
        assert!(msg.contains("mixed to one stream"), "{msg}");
        assert!(msg.contains("6 channels"), "{msg}");
    }

    #[test]
    fn one_audio_track_is_not_reported_as_a_mixdown() {
        // Nothing was merged, so there is nothing to warn about.
        let mut e = Exporter::new();
        let tl = tl_with_audio(1, Some(2));
        let msg = e
            .start(&mut backend(), Path::new("/tmp/out.mkv"), None, &tl)
            .unwrap();
        assert!(!msg.contains("mixed to one stream"), "{msg}");
        assert!(!msg.contains("stay separate"), "{msg}");
    }

    #[test]
    fn nine_audio_tracks_are_more_than_a_file_can_carry() {
        let err = audio_stream_plan(&tl_with_audio(9, Some(2))).unwrap_err();
        assert!(err.contains("at most 8"), "{err}");
    }

    #[test]
    fn the_extension_picks_the_preset_when_none_is_named() {
        let (mut e, mut b) = (Exporter::new(), backend());
        let msg = start(&mut e, &mut b, "/tmp/clip.webm").unwrap();
        assert!(msg.contains("webm"), "{msg}");
    }

    #[test]
    fn a_missing_extension_is_filled_in_from_the_preset() {
        let (mut e, mut b) = (Exporter::new(), backend());
        let msg = e
            .start(&mut b, Path::new("/tmp/out"), Some("mp4"), &timeline())
            .unwrap();
        assert!(msg.contains("/tmp/out.mp4"), "{msg}");
    }

    #[test]
    fn a_second_export_is_refused_while_one_runs() {
        // Two consumers on one graph produce one broken file, silently.
        let (mut e, mut b) = (Exporter::new(), backend());
        start(&mut e, &mut b, "/tmp/a.mkv").unwrap();
        let err = start(&mut e, &mut b, "/tmp/b.mkv").unwrap_err();
        assert!(err.to_string().contains("/tmp/a.mkv"), "{err}");
    }

    #[test]
    fn an_empty_timeline_is_refused_before_a_file_is_opened() {
        let (mut e, mut b) = (Exporter::new(), backend());
        let err = e
            .start(
                &mut b,
                Path::new("/tmp/out.mkv"),
                None,
                &Timeline::default(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("nothing to export"), "{err}");
    }

    #[test]
    fn an_unknown_preset_is_refused_with_the_list_of_real_ones() {
        let (mut e, mut b) = (Exporter::new(), backend());
        let err = e
            .start(
                &mut b,
                Path::new("/tmp/out.mkv"),
                Some("youtube"),
                &timeline(),
            )
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("youtube"), "{msg}");
        assert!(msg.contains("mkv"), "{msg}");
    }

    #[test]
    fn cancelling_reports_that_the_partial_file_was_kept() {
        // Deleting a user's file is not this program's decision.
        let (mut e, mut b) = (Exporter::new(), backend());
        start(&mut e, &mut b, "/tmp/out.mkv").unwrap();
        let msg = e.cancel(&mut b).unwrap();
        assert!(msg.contains("left in place"), "{msg}");
        assert!(!e.is_running());
    }

    #[test]
    fn cancelling_nothing_says_so() {
        let (mut e, mut b) = (Exporter::new(), backend());
        assert!(e.cancel(&mut b).is_err());
    }

    #[test]
    fn progress_never_reaches_full_before_the_backend_says_done() {
        assert_eq!(permille(0, 100), 0);
        assert_eq!(permille(50, 100), 500);
        // 100/100 while still Running must not print as 100%.
        assert_eq!(permille(100, 100), 999);
        assert_eq!(permille(1, 0), 0);
    }

    #[test]
    fn a_default_output_is_named_after_the_project() {
        let out = default_output(Some(Path::new("/p/my cut.davimci")), Container::Mkv);
        assert_eq!(out, PathBuf::from("my cut.mkv"));
        assert_eq!(
            default_output(None, Container::Mp4),
            PathBuf::from("untitled.mp4")
        );
    }
}
