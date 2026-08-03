//! The `:` command set for project lifecycle (spec §12, plan.md Phase 8).
//!
//! Parsing is separated from execution so the grammar can be tested with no
//! filesystem: [`parse`] is a pure function from a command line to an
//! [`ExCommand`], and [`Workspace::run`] is the only part that touches disk.

use std::path::PathBuf;

use davimci_analysis::{FfprobeProber, ImportOptions, Prober};
use davimci_cmd::EditCommand;
use davimci_core::{Selection, TimelineProps};

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
    /// `:gain <db>` - absolute gain on the clip under the playhead (§6.1).
    Gain(f32),
    /// `:fade in|out <ms>` (§6.1).
    Fade { end: crate::audio::FadeEnd, ms: u64 },
    /// `:normalize [target_db]` (§6.1). Needs analysis, so the editor runs it.
    Normalize { target_db: f32 },
    /// `:duck <track> <db>` (§6.1). Needs analysis, so the editor runs it.
    Duck { track: String, db: f32 },
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

/// Parse a `:` line. The leading colon is optional.
pub fn parse(line: &str) -> Result<ExCommand, CliError> {
    let line = line.trim().strip_prefix(':').unwrap_or(line.trim());
    let mut parts = line.split_whitespace();
    let Some(head) = parts.next() else {
        return Err(CliError::UnknownCommand(String::new()));
    };
    let args: Vec<&str> = parts.collect();
    let one = |cmd: &str, usage: &str| -> Result<String, CliError> {
        match args.as_slice() {
            [a] => Ok((*a).to_string()),
            _ => Err(CliError::Usage {
                cmd: cmd.to_string(),
                usage: usage.to_string(),
            }),
        }
    };
    // A path argument is the *rest of the line*, not one whitespace-delimited
    // token: media filenames contain spaces constantly, and these commands
    // take exactly one path, so there is nothing else the remainder could be.
    let rest = || {
        let after = line[head.len()..].trim();
        (!after.is_empty()).then(|| after.to_string())
    };
    let one_path = |cmd: &str, usage: &str| -> Result<PathBuf, CliError> {
        rest().map(PathBuf::from).ok_or_else(|| CliError::Usage {
            cmd: cmd.to_string(),
            usage: usage.to_string(),
        })
    };
    let optional_path = || rest().map(PathBuf::from);

    // `--preset <name>` is a trailing flag, so the path before it may contain
    // spaces like any other path argument.
    let split_preset = |usage: &str| -> Result<(PathBuf, Option<String>), CliError> {
        let tail = rest().ok_or_else(|| CliError::Usage {
            cmd: head.to_string(),
            usage: usage.to_string(),
        })?;
        match tail.split_once("--preset") {
            Some((path, name)) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err(CliError::Usage {
                        cmd: head.to_string(),
                        usage: usage.to_string(),
                    });
                }
                Ok((PathBuf::from(path.trim()), Some(name.to_string())))
            }
            None => Ok((PathBuf::from(tail), None)),
        }
    };

    match head {
        "w" | "write" => Ok(ExCommand::Write(optional_path())),
        "q" | "quit" => Ok(ExCommand::Quit { force: false }),
        "q!" | "quit!" => Ok(ExCommand::Quit { force: true }),
        "wq" | "x" => Ok(ExCommand::WriteQuit(optional_path())),
        "e" | "edit" => Ok(ExCommand::Edit(one_path("e", "<path>")?)),
        "export" => {
            let (path, preset) = split_preset("<path> [--preset <name>]")?;
            Ok(ExCommand::Export { path, preset })
        }
        "render" => Ok(ExCommand::Render {
            preset: one("render", "<preset>")?,
        }),
        "gain" => {
            let v = one("gain", "<db>")?;
            v.parse::<f32>()
                .map(ExCommand::Gain)
                .map_err(|_| CliError::Usage {
                    cmd: "gain".into(),
                    usage: "<db>".into(),
                })
        }
        "fade" => match args.as_slice() {
            [dir, ms] => {
                let end = crate::audio::FadeEnd::parse(dir).ok_or_else(|| CliError::Usage {
                    cmd: "fade".into(),
                    usage: "in|out <ms>".into(),
                })?;
                let ms = ms.parse::<u64>().map_err(|_| CliError::Usage {
                    cmd: "fade".into(),
                    usage: "in|out <ms>".into(),
                })?;
                Ok(ExCommand::Fade { end, ms })
            }
            _ => Err(CliError::Usage {
                cmd: "fade".into(),
                usage: "in|out <ms>".into(),
            }),
        },
        "normalize" | "normalise" => match args.as_slice() {
            [] => Ok(ExCommand::Normalize {
                target_db: DEFAULT_NORMALIZE_DB,
            }),
            [db] => db
                .parse::<f32>()
                .map(|target_db| ExCommand::Normalize { target_db })
                .map_err(|_| CliError::Usage {
                    cmd: "normalize".into(),
                    usage: "[target_db]".into(),
                }),
            _ => Err(CliError::Usage {
                cmd: "normalize".into(),
                usage: "[target_db]".into(),
            }),
        },
        "duck" => match args.as_slice() {
            [track, db] => db
                .parse::<f32>()
                .map(|db| ExCommand::Duck {
                    track: (*track).to_string(),
                    db,
                })
                .map_err(|_| CliError::Usage {
                    cmd: "duck".into(),
                    usage: "<track> <db>".into(),
                }),
            _ => Err(CliError::Usage {
                cmd: "duck".into(),
                usage: "<track> <db>".into(),
            }),
        },
        "presets" => Ok(ExCommand::Presets),
        "cancel" => Ok(ExCommand::CancelRender),
        "new" => Ok(ExCommand::New),
        "ls" | "buffers" => Ok(ExCommand::List),
        "bn" | "bnext" => Ok(ExCommand::BufferNext),
        "bp" | "bprev" => Ok(ExCommand::BufferPrev),
        "b" | "buffer" => {
            let n = one("b", "<n>")?;
            n.parse::<usize>()
                .map(ExCommand::Buffer)
                .map_err(|_| CliError::NoSuchBuffer(n))
        }
        "relink" => match args.as_slice() {
            [new] => Ok(ExCommand::Relink {
                old: None,
                new: (*new).to_string(),
            }),
            [old, new] => Ok(ExCommand::Relink {
                old: Some((*old).to_string()),
                new: (*new).to_string(),
            }),
            _ => Err(CliError::Usage {
                cmd: "relink".into(),
                usage: "[old path] <new path>".into(),
            }),
        },
        other => Err(CliError::UnknownCommand(other.to_string())),
    }
}

impl Workspace {
    /// Parse and run a `:` line, answering any recovery prompt with
    /// `on_recovery`, against no selection.
    pub fn run(&mut self, line: &str, on_recovery: OnRecovery) -> Result<ExOutcome, CliError> {
        self.run_selected(line, on_recovery, None)
    }

    /// Parse and run a `:` line against the user's selection (spec §6.1).
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
            // them: no backend, no analysis, just an undoable edit (§6.1).
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
            ExCommand::Normalize { .. } | ExCommand::Duck { .. } => {
                Err(CliError::AnalysisNotReady("this command"))
            }
            ExCommand::Relink { old, new } => self.relink(old.as_deref(), new),
        }
    }

    /// `:e <path>`: a davimci project opens as a project, anything else is
    /// imported as media into a fresh timeline (spec §12).
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

    /// `:relink` (Phase 0 offline-media policy): point clips at a file that
    /// moved. One undoable command, so a mistaken relink is `u` away.
    fn relink(&mut self, old: Option<&str>, new: &str) -> Result<ExOutcome, CliError> {
        // The new file's existence decides the offline flag - this is the
        // only layer that may ask the filesystem.
        let offline = !std::path::Path::new(new).exists();
        let targets: Vec<_> = match old {
            Some(old) => self
                .current()
                .timeline()
                .tracks()
                .iter()
                .flat_map(|t| t.clips())
                .filter(|c| c.media.as_ref().is_some_and(|m| m.path == old))
                .map(|c| c.id)
                .collect(),
            None => {
                let tl = self.current().timeline();
                let head = tl.playhead();
                let clip = tl
                    .track(head.track)
                    .and_then(|t| t.clip_at(head.frame))
                    .filter(|c| c.media.is_some())
                    .ok_or(CliError::NothingToRelink)?;
                vec![clip.id]
            }
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
