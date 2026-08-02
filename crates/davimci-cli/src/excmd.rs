//! The `:` command set for project lifecycle (spec §12, plan.md Phase 8).
//!
//! Parsing is separated from execution so the grammar can be tested with no
//! filesystem: [`parse`] is a pure function from a command line to an
//! [`ExCommand`], and [`Workspace::run`] is the only part that touches disk.

use std::path::PathBuf;

use davimci_analysis::{FfprobeProber, ImportOptions, Prober};
use davimci_cmd::EditCommand;
use davimci_core::TimelineProps;

use crate::autosave::OnRecovery;
use crate::error::CliError;
use crate::workspace::Workspace;

/// A parsed `:` command.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

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
    let optional_path = || args.first().map(|a| PathBuf::from(*a));

    match head {
        "w" | "write" => Ok(ExCommand::Write(optional_path())),
        "q" | "quit" => Ok(ExCommand::Quit { force: false }),
        "q!" | "quit!" => Ok(ExCommand::Quit { force: true }),
        "wq" | "x" => Ok(ExCommand::WriteQuit(optional_path())),
        "e" | "edit" => Ok(ExCommand::Edit(PathBuf::from(one("e", "<path>")?))),
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
    /// `on_recovery`.
    pub fn run(&mut self, line: &str, on_recovery: OnRecovery) -> Result<ExOutcome, CliError> {
        self.run_command(&parse(line)?, on_recovery)
    }

    /// Run an already-parsed command.
    pub fn run_command(
        &mut self,
        cmd: &ExCommand,
        on_recovery: OnRecovery,
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
