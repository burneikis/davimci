//! The `:` command set for project lifecycle.
//!
//! Parsing is separated from execution so the grammar can be tested with no
//! filesystem: [`parse`] is a pure function from a command line to an
//! [`ExCommand`], and [`Workspace::run`] is the only part that touches disk.

use std::path::PathBuf;

use davimci_analysis::{FfprobeProber, ImportOptions, Prober};
use davimci_cmd::EditCommand;
use davimci_core::{
    ClipProps, DEFAULT_TRANSITION, DEFAULT_TRANSITION_FRAMES, Frame, Selection, TimelineProps,
    Transition,
};

use crate::autosave::OnRecovery;
use crate::error::CliError;
use crate::workspace::Workspace;

/// A parsed `:` command.
///
/// Not `Eq`: gain and fade targets are decibels, and decibels are floats.
#[derive(Debug, Clone, PartialEq)]
pub enum ExCommand {
    /// `:w [path]`
    Write(Option<PathBuf>),
    /// `:q` / `:q!`
    Quit { force: bool },
    /// `:wq` / `:x`
    WriteQuit(Option<PathBuf>),
    /// `:e <path>` - a project file, or a media file to import.
    Edit(PathBuf),
    /// `:new`
    New,
    /// `:ls`
    List,
    /// `:bn`
    BufferNext,
    /// `:bp`
    BufferPrev,
    /// `:b <n>`
    Buffer(usize),
    /// `:relink [old] <new>`
    Relink { old: Option<String>, new: String },
    /// `:group` - link the clips under the playhead into one group.
    Group,
    /// `:ungroup` - break the group holding the clip under the playhead.
    Ungroup,
    /// `:export <path> [--preset <name>]` - render to an explicit file.
    Export {
        path: PathBuf,
        preset: Option<String>,
    },
    /// `:render <preset>` - render with a preset, naming the file after the
    /// project and the preset's container.
    Render { preset: String },
    /// `:presets` - list what `:render` will accept.
    Presets,
    /// `:cancel` - stop the running export.
    CancelRender,
    /// `:gain <db>` - absolute gain on the clip under the playhead.
    Gain(f32),
    /// `:fade in|out <ms>`.
    Fade { end: crate::audio::FadeEnd, ms: u64 },
    /// `:normalize [target_db]`. Needs analysis, so the editor runs it.
    Normalize { target_db: f32 },
    /// `:duck <track> <db>`. Needs analysis, so the editor runs it.
    Duck { track: String, db: f32 },
    /// `:transition <name> [frames]`, on the cut nearest the playhead.
    /// `:transition none` deletes the one that is there. Re-running it on a
    /// cut that already has one replaces it, which is how a transition's type
    /// or duration is changed.
    Transition {
        kind: Option<String>,
        frames: Option<u64>,
    },
    /// `:analyze` - drop every envelope and measure the audio again
    ///. Needs the analyser, so the editor runs it.
    Analyze,
    /// `:<n>` - put the playhead on frame `n`, clamped to the timeline.
    Goto(Frame),
    /// `:set <property> <value>`. The value is parsed and
    /// range-checked at parse time, so an accepted `ExCommand::Set` is one
    /// the model will take.
    Set(crate::setting::Setting),
}

/// Default target for `:normalize`, in dBFS RMS. Conservative on purpose:
/// normalising should not be the thing that introduces clipping.
pub const DEFAULT_NORMALIZE_DB: f32 = -12.0;

/// What running a command produced. Messages are user-facing sentences, never
/// `Debug` output (Phase 0 rule 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExOutcome {
    /// One line for the status line.
    Message(String),
    /// Several lines, as `:ls` produces.
    Lines(Vec<String>),
    /// The last timeline closed; the frontend should exit.
    Quit,
}

impl ExOutcome {
    fn msg(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

/// One `:` line split into its command name and its arguments, in both the
/// forms the vocabulary needs: whitespace-delimited tokens, and the raw
/// remainder for the commands that take a path.
struct Line<'a> {
    head: &'a str,
    args: Vec<&'a str>,
    tail: &'a str,
}

impl<'a> Line<'a> {
    fn split(line: &'a str) -> Option<Self> {
        let line = line.trim().strip_prefix(':').unwrap_or(line.trim());
        let head = line.split_whitespace().next()?;
        Some(Self {
            head,
            args: line.split_whitespace().skip(1).collect(),
            tail: line[head.len()..].trim(),
        })
    }

    fn usage(&self, usage: &str) -> CliError {
        CliError::usage(self.head, usage)
    }

    /// The single token this command takes.
    fn one(&self, usage: &str) -> Result<String, CliError> {
        match self.args.as_slice() {
            [a] => Ok((*a).to_string()),
            _ => Err(self.usage(usage)),
        }
    }

    /// A path argument is the *rest of the line*, not one whitespace-delimited
    /// token: media filenames contain spaces constantly, and these commands
    /// take exactly one path, so there is nothing else the remainder could be.
    fn optional_path(&self) -> Option<PathBuf> {
        (!self.tail.is_empty()).then(|| PathBuf::from(self.tail))
    }

    fn path(&self, usage: &str) -> Result<PathBuf, CliError> {
        self.optional_path().ok_or_else(|| self.usage(usage))
    }

    fn number<T: std::str::FromStr>(&self, text: &str, usage: &str) -> Result<T, CliError> {
        text.parse().map_err(|_| self.usage(usage))
    }
}

/// Parse a `:` line. The leading colon is optional.
pub fn parse(line: &str) -> Result<ExCommand, CliError> {
    let Some(line) = Line::split(line) else {
        return Err(CliError::UnknownCommand(String::new()));
    };

    // `:1234` is a frame, exactly as `:1234` is a line in vim; a number is
    // never a command name, so this cannot shadow one.
    if line.head.chars().all(|c| c.is_ascii_digit()) {
        return Ok(ExCommand::Goto(Frame(
            line.number(line.head, "a frame number")?,
        )));
    }

    match line.head {
        "w" | "write" => Ok(ExCommand::Write(line.optional_path())),
        "q" | "quit" => Ok(ExCommand::Quit { force: false }),
        "q!" | "quit!" => Ok(ExCommand::Quit { force: true }),
        "wq" | "x" => Ok(ExCommand::WriteQuit(line.optional_path())),
        "e" | "edit" => Ok(ExCommand::Edit(line.path("<path>")?)),
        "export" => parse_export(&line),
        "render" => Ok(ExCommand::Render {
            preset: line.one("<preset>")?,
        }),
        "gain" => Ok(ExCommand::Gain(line.number(&line.one("<db>")?, "<db>")?)),
        "fade" => parse_fade(&line),
        "normalize" | "normalise" => parse_normalize(&line),
        "duck" => parse_duck(&line),
        "transition" => parse_transition(&line),
        "set" | "se" => match line.args.as_slice() {
            [prop, value] => crate::setting::parse(prop, value).map(ExCommand::Set),
            _ => Err(line.usage("<property> <value>")),
        },
        "analyze" | "analyse" => Ok(ExCommand::Analyze),
        "presets" => Ok(ExCommand::Presets),
        "cancel" => Ok(ExCommand::CancelRender),
        "new" => Ok(ExCommand::New),
        "ls" | "buffers" => Ok(ExCommand::List),
        "bn" | "bnext" => Ok(ExCommand::BufferNext),
        "bp" | "bprev" => Ok(ExCommand::BufferPrev),
        "b" | "buffer" => {
            let n = line.one("<n>")?;
            n.parse::<usize>()
                .map(ExCommand::Buffer)
                .map_err(|_| CliError::NoSuchBuffer(n))
        }
        "relink" => parse_relink(&line),
        "group" => Ok(ExCommand::Group),
        "ungroup" => Ok(ExCommand::Ungroup),
        other => Err(CliError::UnknownCommand(other.to_string())),
    }
}

/// `--preset <name>` is a trailing flag, so the path before it may contain
/// spaces like any other path argument.
fn parse_export(line: &Line<'_>) -> Result<ExCommand, CliError> {
    const USAGE: &str = "<path> [--preset <name>]";
    if line.tail.is_empty() {
        return Err(line.usage(USAGE));
    }
    let Some((path, name)) = line.tail.split_once("--preset") else {
        return Ok(ExCommand::Export {
            path: PathBuf::from(line.tail),
            preset: None,
        });
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(line.usage(USAGE));
    }
    Ok(ExCommand::Export {
        path: PathBuf::from(path.trim()),
        preset: Some(name.to_string()),
    })
}

fn parse_fade(line: &Line<'_>) -> Result<ExCommand, CliError> {
    const USAGE: &str = "in|out <ms>";
    let [dir, ms] = line.args.as_slice() else {
        return Err(line.usage(USAGE));
    };
    let end = crate::audio::FadeEnd::parse(dir).ok_or_else(|| line.usage(USAGE))?;
    Ok(ExCommand::Fade {
        end,
        ms: line.number(ms, USAGE)?,
    })
}

fn parse_normalize(line: &Line<'_>) -> Result<ExCommand, CliError> {
    const USAGE: &str = "[target_db]";
    match line.args.as_slice() {
        [] => Ok(ExCommand::Normalize {
            target_db: DEFAULT_NORMALIZE_DB,
        }),
        [db] => Ok(ExCommand::Normalize {
            target_db: line.number(db, USAGE)?,
        }),
        _ => Err(line.usage(USAGE)),
    }
}

fn parse_duck(line: &Line<'_>) -> Result<ExCommand, CliError> {
    const USAGE: &str = "<track> <db>";
    let [track, db] = line.args.as_slice() else {
        return Err(line.usage(USAGE));
    };
    Ok(ExCommand::Duck {
        track: (*track).to_string(),
        db: line.number(db, USAGE)?,
    })
}

fn parse_transition(line: &Line<'_>) -> Result<ExCommand, CliError> {
    const USAGE: &str = "<name|none> [frames]";
    match line.args.as_slice() {
        [] => Ok(ExCommand::Transition {
            kind: Some(DEFAULT_TRANSITION.to_string()),
            frames: None,
        }),
        ["none"] => Ok(ExCommand::Transition {
            kind: None,
            frames: None,
        }),
        [name] => Ok(ExCommand::Transition {
            kind: Some((*name).to_string()),
            frames: None,
        }),
        [name, n] => Ok(ExCommand::Transition {
            kind: Some((*name).to_string()),
            frames: Some(line.number(n, USAGE)?),
        }),
        _ => Err(line.usage(USAGE)),
    }
}

fn parse_relink(line: &Line<'_>) -> Result<ExCommand, CliError> {
    match line.args.as_slice() {
        [new] => Ok(ExCommand::Relink {
            old: None,
            new: (*new).to_string(),
        }),
        [old, new] => Ok(ExCommand::Relink {
            old: Some((*old).to_string()),
            new: (*new).to_string(),
        }),
        _ => Err(line.usage("[old path] <new path>")),
    }
}

/// Every `:` name this crate accepts, and the arguments each one takes, for
/// the command line's completion. The vocabulary lives here, next
/// to [`parse`], so a command that exists is a command that can be completed,
/// and an argument that parses is an argument that can be completed.
///
/// Contexts a vocabulary cannot enumerate - paths, numbers, preset names the
/// host installs - are simply absent, which suggests nothing rather than
/// suggesting command names in an argument position.
#[must_use]
pub fn vocabulary() -> davimci_app::CommandVocabulary {
    vocabulary_with(&crate::setting::CurrentSettings::default())
}

/// As [`vocabulary`], but with the session's current values folded in, so a
/// free-form `:set` offers what it holds now instead of offering nothing.
#[must_use]
pub fn vocabulary_with(
    current: &crate::setting::CurrentSettings,
) -> davimci_app::CommandVocabulary {
    let mut v = davimci_app::CommandVocabulary::new(command_names())
        .with_arguments(
            "set",
            crate::setting::PROPERTIES
                .iter()
                .map(|p| (*p).to_string())
                .collect(),
        )
        .with_arguments("fade", words(&["in", "out"]));
    let transitions: Vec<String> = davimci_mlt::transitions::names()
        .into_iter()
        .map(str::to_string)
        .chain(std::iter::once("none".to_string()))
        .chain(davimci_mlt::transitions::registered_names())
        .collect();
    for prop in crate::setting::PROPERTIES {
        let mut candidates = current.candidates(prop);
        if *prop == "transition.type" {
            let extra: Vec<String> = transitions
                .iter()
                .filter(|t| !candidates.contains(t))
                .cloned()
                .collect();
            candidates.extend(extra);
        }
        if !candidates.is_empty() {
            v = v.with_arguments(&format!("set {prop}"), candidates);
        }
    }
    v.with_arguments("transition", transitions)
}

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(|s| (*s).to_string()).collect()
}

fn command_names() -> Vec<String> {
    [
        "w",
        "write",
        "q",
        "q!",
        "quit",
        "quit!",
        "wq",
        "x",
        "e",
        "edit",
        "export",
        "render",
        "presets",
        "cancel",
        "gain",
        "fade",
        "normalize",
        "normalise",
        "duck",
        "transition",
        "analyze",
        "analyse",
        "set",
        "new",
        "ls",
        "buffers",
        "bn",
        "bnext",
        "bp",
        "bprev",
        "b",
        "buffer",
        "relink",
        "group",
        "ungroup",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

impl Workspace {
    /// Parse and run a `:` line, answering any recovery prompt with
    /// `on_recovery`, against no selection.
    pub fn run(&mut self, line: &str, on_recovery: OnRecovery) -> Result<ExOutcome, CliError> {
        self.run_selected(line, on_recovery, None)
    }

    /// Parse and run a `:` line against the user's selection.
    /// `None` means the clip-property commands fall back to the playhead.
    pub fn run_selected(
        &mut self,
        line: &str,
        on_recovery: OnRecovery,
        selection: Option<&Selection>,
    ) -> Result<ExOutcome, CliError> {
        self.run_command_selected(&parse(line)?, on_recovery, selection)
    }

    /// Run an already-parsed command against no selection.
    pub fn run_command(
        &mut self,
        cmd: &ExCommand,
        on_recovery: OnRecovery,
    ) -> Result<ExOutcome, CliError> {
        self.run_command_selected(cmd, on_recovery, None)
    }

    /// Run an already-parsed command against the user's selection.
    pub fn run_command_selected(
        &mut self,
        cmd: &ExCommand,
        on_recovery: OnRecovery,
        selection: Option<&Selection>,
    ) -> Result<ExOutcome, CliError> {
        match cmd {
            ExCommand::Write(path) => {
                let saved = self.write(path.clone())?;
                Ok(ExOutcome::msg(format!("wrote {}", saved.display())))
            }
            ExCommand::Quit { force } => {
                self.close(*force)?;
                Ok(if self.should_quit() {
                    ExOutcome::Quit
                } else {
                    ExOutcome::msg(format!("now editing {}", self.current().name()))
                })
            }
            ExCommand::WriteQuit(path) => {
                self.write(path.clone())?;
                self.close(false)?;
                Ok(if self.should_quit() {
                    ExOutcome::Quit
                } else {
                    ExOutcome::msg(format!("now editing {}", self.current().name()))
                })
            }
            ExCommand::Edit(path) => self.edit(path, on_recovery),
            // Exporting needs a render backend, which a workspace has no
            // business owning; the editor intercepts these before they
            // arrive here (see `Editor::command`).
            ExCommand::Export { .. }
            | ExCommand::Render { .. }
            | ExCommand::Presets
            | ExCommand::CancelRender => Err(CliError::ExportFailed {
                reason: "exporting needs a running editor, and this session has no render backend"
                    .into(),
            }),
            ExCommand::New => {
                self.new_timeline(TimelineProps::default());
                Ok(ExOutcome::msg("new timeline"))
            }
            // Navigation, not an edit: the playhead never enters the undo
            // log, and a frame past the end lands on the end, as `:$` does.
            ExCommand::Goto(frame) => {
                let tl = self.current().timeline();
                let target = (*frame).min(tl.duration());
                let track = tl.playhead().track;
                self.with_session(|s| s.set_playhead(target, track))?;
                Ok(ExOutcome::msg(format!("frame {}", target.get())))
            }
            ExCommand::List => Ok(ExOutcome::Lines(self.list())),
            ExCommand::BufferNext => {
                self.next_buffer()?;
                Ok(ExOutcome::msg(self.current().name()))
            }
            ExCommand::BufferPrev => {
                self.prev_buffer()?;
                Ok(ExOutcome::msg(self.current().name()))
            }
            ExCommand::Buffer(n) => {
                self.goto_buffer_id(*n)?;
                Ok(ExOutcome::msg(self.current().name()))
            }
            // Gain and fades are clip properties, so the workspace can run
            // them: no backend, no analysis, just an undoable edit.
            ExCommand::Gain(db) => {
                let clips =
                    crate::audio::targets(self.current().timeline(), selection, "set gain on")?;
                // One `Sequence`, so a selection-wide change is one `u`.
                let cmds = clips
                    .iter()
                    .map(|(track, clip)| crate::audio::gain(*track, clip, *db))
                    .collect();
                self.exec(&EditCommand::Sequence(cmds))?;
                Ok(ExOutcome::msg(format!(
                    "{} gain {db:+} dB",
                    crate::audio::describe(&clips)
                )))
            }
            ExCommand::Fade { end, ms } => {
                let fps = self.current().timeline().props.fps;
                let clips = crate::audio::targets(self.current().timeline(), selection, "fade")?;
                let cmds = clips
                    .iter()
                    .map(|(track, clip)| crate::audio::fade(*track, clip, *end, *ms, fps))
                    .collect();
                self.exec(&EditCommand::Sequence(cmds))?;
                Ok(ExOutcome::msg(format!(
                    "{} fade {} {ms} ms",
                    crate::audio::describe(&clips),
                    match end {
                        crate::audio::FadeEnd::In => "in",
                        crate::audio::FadeEnd::Out => "out",
                    }
                )))
            }
            // These two measure the audio, so they need the analysis the
            // editor owns; it intercepts them before they arrive here.
            ExCommand::Normalize { .. } | ExCommand::Duck { .. } | ExCommand::Analyze => {
                Err(CliError::AnalysisNotReady("this command"))
            }
            // A transition is an undoable edit on the timeline and needs
            // neither backend nor analysis, so it runs here.
            ExCommand::Transition { kind, frames } => self.transition(kind.as_deref(), *frames),
            ExCommand::Set(setting) => match Self::editor_only(setting) {
                Some(name) => Err(CliError::Usage {
                    cmd: name.into(),
                    usage: "on|off needs a running editor".into(),
                }),
                None => self.set(setting, selection),
            },
            ExCommand::Relink { old, new } => self.relink(old.as_deref(), new),
            ExCommand::Group => self.group(),
            ExCommand::Ungroup => self.ungroup(),
        }
    }

    /// `:e <path>`: a davimci project opens as a project, anything else is
    /// imported as media into a fresh timeline.
    fn edit(
        &mut self,
        path: &std::path::Path,
        on_recovery: OnRecovery,
    ) -> Result<ExOutcome, CliError> {
        if !path.exists() {
            return Err(CliError::Io {
                what: "open",
                path: path.display().to_string(),
                reason: "no such file".into(),
            });
        }
        if is_project_file(path) {
            self.open_project(path, on_recovery)?;
            return Ok(ExOutcome::msg(format!(
                "opened {}{}",
                path.display(),
                if self.current().is_dirty() {
                    " (recovered)"
                } else {
                    ""
                }
            )));
        }
        self.import_media(path, &FfprobeProber)
    }

    /// Import a media file into a new timeline. The prober is injected so the
    /// path is testable without ffprobe present.
    pub fn import_media(
        &mut self,
        path: &std::path::Path,
        prober: &dyn Prober,
    ) -> Result<ExOutcome, CliError> {
        let info = prober.probe(path)?;
        self.new_timeline(TimelineProps::default());
        let imported =
            self.with_session(|s| davimci_analysis::import(s, &info, &ImportOptions::default()))?;
        self.sync_autosave()?;
        Ok(ExOutcome::msg(format!(
            "imported {} ({} tracks)",
            imported.path,
            imported.mapping.len()
        )))
    }

    /// `:set <property> <value>` on the selection, or on the clip under the
    /// playhead when nothing is selected.
    ///
    /// Every setter is one command, so a selection-wide change is one `u`.
    fn set(
        &mut self,
        setting: &crate::setting::Setting,
        selection: Option<&Selection>,
    ) -> Result<ExOutcome, CliError> {
        use crate::setting::{Setting, TransformField};
        let fps = self.current().timeline().props.fps;
        let clip_props = |clip: &davimci_core::Clip| -> ClipProps {
            let mut props = clip.props;
            match setting {
                Setting::Transform(field, v) => match field {
                    TransformField::X => props.transform.x = *v,
                    TransformField::Y => props.transform.y = *v,
                    TransformField::Scale => props.transform.scale = *v,
                    TransformField::Opacity => props.transform.opacity = *v,
                },
                Setting::Gain(db) => props.gain_db = *db,
                Setting::Fade(end, ms) => {
                    // Clamped to the clip, exactly as `:fade` is: the two
                    // spellings must not mean different things.
                    let frames = Frame(crate::audio::frames_for_ms(*ms, fps)).min(clip.duration);
                    match end {
                        crate::audio::FadeEnd::In => props.fade_in = frames,
                        crate::audio::FadeEnd::Out => props.fade_out = frames,
                    }
                }
                _ => {}
            }
            props
        };
        match setting {
            Setting::Transform(..) | Setting::Gain(_) | Setting::Fade(..) => {
                let clips = crate::audio::targets(self.current().timeline(), selection, "set")?;
                let cmds = clips
                    .iter()
                    .map(|(track, clip)| EditCommand::SetProps {
                        track: *track,
                        clip: clip.id,
                        props: clip_props(clip),
                    })
                    .collect();
                self.exec(&EditCommand::Sequence(cmds))?;
                Ok(ExOutcome::msg(format!(
                    "{} {}",
                    crate::audio::describe(&clips),
                    describe_setting(setting)
                )))
            }
            Setting::TransitionDuration(_) | Setting::TransitionType(_) => {
                let tl = self.current().timeline();
                let head = tl.playhead();
                // The transition under the playhead first, then the one on
                // the nearest cut: the user standing inside an overlap means
                // that overlap.
                let (clip, existing) = tl
                    .transition_at(head.track, head.frame)
                    .map(|(clip, t)| (clip, t.clone()))
                    .or_else(|| {
                        let (clip, _) = tl.nearest_cut(head.track, head.frame)?;
                        let t = tl.track(head.track)?.clip(clip)?.transition_in.clone()?;
                        Some((clip, t))
                    })
                    .ok_or(CliError::NoTransitionTarget {
                        what: "transition to change",
                    })?;
                let transition = match setting {
                    Setting::TransitionDuration(frames) => {
                        Transition::new(&existing.kind, Frame(*frames))
                    }
                    Setting::TransitionType(kind) => Transition::new(kind, existing.duration),
                    _ => existing,
                };
                self.exec(&EditCommand::SetTransition {
                    track: head.track,
                    clip,
                    transition: Some(transition),
                })?;
                Ok(ExOutcome::msg(format!(
                    "transition {}",
                    describe_setting(setting)
                )))
            }
            Setting::TimelineFps(fps) => {
                let props = TimelineProps {
                    fps: *fps,
                    ..self.current().timeline().props
                };
                self.exec(&EditCommand::Reconform { props })?;
                Ok(ExOutcome::msg(format!("timeline conformed to {props}")))
            }
            Setting::TimelineResolution(resolution) => {
                let props = TimelineProps {
                    resolution: *resolution,
                    ..self.current().timeline().props
                };
                self.exec(&EditCommand::Reconform { props })?;
                Ok(ExOutcome::msg(format!("timeline conformed to {props}")))
            }
            // Handled by the editor; unreachable through this path.
            Setting::Preview(_) => Err(CliError::UnknownProperty("preview".into())),
            Setting::Proxy(_) => Err(CliError::UnknownProperty("proxy".into())),
            Setting::Decode(_) => Err(CliError::UnknownProperty("decode".into())),
            Setting::PreviewHeight(_) => Err(CliError::UnknownProperty("previewheight".into())),
            Setting::PreviewProtocol(_) => Err(CliError::UnknownProperty("previewprotocol".into())),
            Setting::Numbers(_) => Err(CliError::UnknownProperty("numbers".into())),
            Setting::VisualStart(_) => Err(CliError::UnknownProperty("visualstart".into())),
        }
    }

    /// The name of the `:set` property only a running editor can apply, if this
    /// is one of them.
    ///
    /// One belongs to the preview and one to the importer; the editor intercepts
    /// both before they arrive here, so reaching the workspace means there is no
    /// editor to apply them to.
    fn editor_only(setting: &crate::setting::Setting) -> Option<&'static str> {
        use crate::setting::Setting;
        match setting {
            Setting::Preview(_) => Some("set preview"),
            Setting::Proxy(_) => Some("set proxy"),
            Setting::Decode(_) => Some("set decode"),
            _ => None,
        }
    }

    /// `:relink` (Phase 0 offline-media policy): point clips at a file that
    /// moved. One undoable command, so a mistaken relink is `u` away.
    /// `:transition [name] [frames]` on the cut nearest the playhead.
    ///
    /// Deleting looks for the transition *under* the playhead first, because
    /// the overlap straddles its cut and the user is usually standing in it;
    /// creating always takes the nearest cut.
    fn transition(
        &mut self,
        kind: Option<&str>,
        frames: Option<u64>,
    ) -> Result<ExOutcome, CliError> {
        let tl = self.current().timeline();
        let head = tl.playhead();
        let found = match kind {
            Some(_) => tl.nearest_cut(head.track, head.frame),
            None => tl
                .transition_at(head.track, head.frame)
                .map(|(clip, _)| (clip, head.frame)),
        };
        let Some((clip, _)) = found else {
            return Err(CliError::NoTransitionTarget {
                what: if kind.is_some() {
                    "cut to put a transition on"
                } else {
                    "transition to remove"
                },
            });
        };
        let transition =
            kind.map(|k| Transition::new(k, Frame(frames.unwrap_or(DEFAULT_TRANSITION_FRAMES))));
        let described = transition.as_ref().map_or_else(
            || "transition removed".to_string(),
            |t| format!("{}-frame {} added", t.duration.get(), t.kind),
        );
        self.exec(&EditCommand::SetTransition {
            track: head.track,
            clip,
            transition,
        })?;
        Ok(ExOutcome::msg(described))
    }

    /// `:group`: link every clip the playhead stands in, across all tracks.
    ///
    /// The clips have to be frame-aligned, which is the model's rule for a
    /// group and the reason this takes the whole column rather than a range.
    fn group(&mut self) -> Result<ExOutcome, CliError> {
        let tl = self.current().timeline();
        let frame = tl.playhead().frame;
        let clips: Vec<_> = tl
            .tracks()
            .iter()
            .filter_map(|t| t.clip_at(frame))
            .map(|c| c.id)
            .collect();
        if clips.len() < 2 {
            return Err(CliError::NoClipUnderPlayhead("group with another"));
        }
        let n = clips.len();
        self.exec(&EditCommand::Link { clips, group: None })?;
        Ok(ExOutcome::msg(format!("{n} clips grouped")))
    }

    /// `:ungroup`: every member leaves, not just the one under the playhead -
    /// a group of one is a group in name only.
    fn ungroup(&mut self) -> Result<ExOutcome, CliError> {
        let tl = self.current().timeline();
        let head = tl.playhead();
        let group = tl
            .track(head.track)
            .and_then(|t| t.clip_at(head.frame))
            .and_then(|c| c.group)
            .ok_or(CliError::NoClipUnderPlayhead("ungroup"))?;
        let members = tl.group_members(group);
        let n = members.len();
        let cmds = members
            .into_iter()
            .map(|(_, clip)| EditCommand::SetGroup { clip, group: None })
            .collect();
        self.exec(&EditCommand::Sequence(cmds))?;
        Ok(ExOutcome::msg(format!("{n} clips ungrouped")))
    }

    fn relink(&mut self, old: Option<&str>, new: &str) -> Result<ExOutcome, CliError> {
        // The new file's existence decides the offline flag - this is the
        // only layer that may ask the filesystem.
        let offline = !std::path::Path::new(new).exists();
        let targets: Vec<_> = if let Some(old) = old {
            self.current()
                .timeline()
                .tracks()
                .iter()
                .flat_map(davimci_core::Track::clips)
                .filter(|c| c.media.as_ref().is_some_and(|m| m.path == old))
                .map(|c| c.id)
                .collect()
        } else {
            let tl = self.current().timeline();
            let head = tl.playhead();
            let clip = tl
                .track(head.track)
                .and_then(|t| t.clip_at(head.frame))
                .filter(|c| c.media.is_some())
                .ok_or(CliError::NothingToRelink)?;
            vec![clip.id]
        };
        if targets.is_empty() {
            return Err(CliError::NoClipUsesPath(old.unwrap_or(new).to_string()));
        }
        let n = targets.len();
        let cmd = EditCommand::Sequence(
            targets
                .into_iter()
                .map(|clip| EditCommand::Relink {
                    clip,
                    path: new.to_string(),
                    offline,
                })
                .collect(),
        );
        self.exec(&cmd)?;
        Ok(ExOutcome::msg(if offline {
            format!("relinked {n} clip(s) to {new}, which is still missing")
        } else {
            format!("relinked {n} clip(s) to {new}")
        }))
    }
}

/// What a `:set` did, as a status-line phrase.
fn describe_setting(setting: &crate::setting::Setting) -> String {
    use crate::setting::Setting;
    match setting {
        Setting::Transform(field, v) => format!("{} = {v}", field.name()),
        Setting::Gain(db) => format!("gain {db:+} dB"),
        Setting::Fade(crate::audio::FadeEnd::In, ms) => format!("fade in {ms} ms"),
        Setting::Fade(crate::audio::FadeEnd::Out, ms) => format!("fade out {ms} ms"),
        Setting::TransitionDuration(frames) => format!("{frames} frames"),
        Setting::TransitionType(kind) => format!("set to {kind}"),
        Setting::TimelineFps(fps) => format!("{fps}"),
        Setting::TimelineResolution(r) => format!("{r}"),
        Setting::Preview(on) => format!("preview {}", if *on { "on" } else { "off" }),
        Setting::Proxy(on) => format!("proxy {}", if *on { "on" } else { "off" }),
        Setting::Decode(policy) => format!("decode {policy}"),
        Setting::PreviewHeight(height) => height.describe(),
        Setting::PreviewProtocol(p) => format!("preview protocol {}", p.name()),
        Setting::Numbers(n) => n.describe().to_string(),
        Setting::VisualStart(v) => v.describe().to_string(),
    }
}

/// A project is JSON with a davimci `snapshot` in it. Sniffing the content
/// rather than the extension means `:e` works whatever the file is called.
fn is_project_file(path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("snapshot").cloned())
        .is_some()
}
