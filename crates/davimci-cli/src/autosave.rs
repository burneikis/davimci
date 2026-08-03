//! Continuous autosave of the command log (spec 12, plan.md Phase 8).
//!
//! Two rules shape this module. Autosave never touches the project file -
//! it writes only under `<root>/.davimci/autosave/`, so a crash can never
//! damage the thing the user saved. And it stores the *log*, not the state:
//! appending one line per command is cheap enough to do after every edit,
//! which is what makes recovery land on the exact pre-crash timeline rather
//! than on the last periodic snapshot.
//!
//! The file is JSON lines: line 0 is the snapshot the log replays onto, and
//! every later line is one [`EditCommand`]. Undo shortens the log rather than
//! appending to it, so the writer rewrites the file whenever the current log
//! is not an extension of what is already on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use davimci_cmd::{Command, EditCommand, Session};
use davimci_core::Timeline;
use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// The first line of an autosave file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    version: u32,
    /// The project this log belongs to, for the recovery prompt.
    project: Option<String>,
    snapshot: Timeline,
    /// The id cursor at the time the header was written.
    #[serde(default)]
    cursor: Option<u64>,
}

/// One logged command. The id cursor travels with it because replaying
/// pinned commands mints no ids: without it, a recovered session would hand
/// out ids the crashed one had already used.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    #[serde(rename = "c")]
    command: EditCommand,
    #[serde(default)]
    cursor: Option<u64>,
}

/// What a recoverable autosave file promises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    /// Where the autosave lives, so a prompt can name it.
    pub log: PathBuf,
    /// The project it was recorded against, if the timeline had a filename.
    pub project: Option<PathBuf>,
    /// How many commands would be replayed.
    pub commands: usize,
}

/// How to treat a recoverable autosave when opening a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnRecovery {
    /// Replay the autosave log: the session comes back where it crashed.
    Recover,
    /// Ignore and delete it: open the project file as saved.
    #[default]
    Discard,
}

/// The autosave writer for one open timeline.
#[derive(Debug)]
pub struct Autosave {
    path: PathBuf,
    /// The commands already on disk, so a sync can decide append vs rewrite
    /// without re-reading the file.
    written: Vec<EditCommand>,
    /// The snapshot on disk, compared by serialized form.
    header: Option<String>,
    project: Option<PathBuf>,
    enabled: bool,
}

impl Autosave {
    /// An autosave that writes to `path`.
    #[must_use]
    pub fn new(path: PathBuf, project: Option<PathBuf>) -> Self {
        Self {
            path,
            written: Vec::new(),
            header: None,
            project,
            enabled: true,
        }
    }

    /// An autosave that writes nothing, for tests and `--no-autosave`.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            path: PathBuf::new(),
            written: Vec::new(),
            header: None,
            project: None,
            enabled: false,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Retarget after `:w <path>`, so the recovery prompt names the file the
    /// timeline now belongs to.
    pub fn retarget(&mut self, path: PathBuf, project: Option<PathBuf>) {
        if !self.enabled {
            return;
        }
        let _ = self.discard();
        self.path = path;
        self.project = project;
    }

    /// Bring the file in line with the session. Called after every edit,
    /// undo, and redo.
    pub fn sync(&mut self, session: &Session) -> Result<(), CliError> {
        if !self.enabled {
            return Ok(());
        }
        let (snapshot, log) = session
            .history()
            .compacted()
            .unwrap_or_else(|| (session.timeline().clone(), Vec::new()));
        let cursor = session.timeline().id_cursor();
        let header = Header {
            version: davimci_cmd::FORMAT_VERSION,
            project: self.project.as_ref().map(|p| p.display().to_string()),
            snapshot,
            cursor: Some(cursor),
        };
        let header_line = serde_json::to_string(&header).map_err(|e| CliError::Io {
            what: "record",
            path: self.path.display().to_string(),
            reason: e.to_string(),
        })?;

        let extends = self.header.as_deref() == Some(header_line.as_str())
            && log.len() >= self.written.len()
            && log[..self.written.len()] == self.written[..];
        if extends {
            self.append(&log[self.written.len()..], cursor)?;
        } else {
            self.rewrite(&header_line, &log, cursor)?;
        }
        self.header = Some(header_line);
        self.written = log;
        Ok(())
    }

    /// Delete the autosave. Called on `:w` and on a clean close: a file that
    /// survives means the session did not.
    pub fn discard(&mut self) -> Result<(), CliError> {
        self.written.clear();
        self.header = None;
        if !self.enabled {
            return Ok(());
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CliError::io("remove", self.path.display(), &e)),
        }
    }

    fn append(&self, commands: &[EditCommand], cursor: u64) -> Result<(), CliError> {
        if commands.is_empty() {
            return Ok(());
        }
        let mut text = String::new();
        for c in commands {
            text.push_str(&encode(c, cursor, &self.path)?);
            text.push('\n');
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| CliError::io("write", self.path.display(), &e))?;
        f.write_all(text.as_bytes())
            .map_err(|e| CliError::io("write", self.path.display(), &e))
    }

    fn rewrite(&self, header_line: &str, log: &[EditCommand], cursor: u64) -> Result<(), CliError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| CliError::io("create", dir.display(), &e))?;
        }
        let mut text = String::with_capacity(header_line.len() + 64 * log.len());
        text.push_str(header_line);
        text.push('\n');
        for c in log {
            text.push_str(&encode(c, cursor, &self.path)?);
            text.push('\n');
        }
        fs::write(&self.path, text).map_err(|e| CliError::io("write", self.path.display(), &e))
    }
}

fn encode(cmd: &EditCommand, cursor: u64, path: &Path) -> Result<String, CliError> {
    let entry = Entry {
        command: cmd.clone(),
        cursor: Some(cursor),
    };
    serde_json::to_string(&entry).map_err(|e| CliError::Io {
        what: "record",
        path: path.display().to_string(),
        reason: e.to_string(),
    })
}

/// Read an autosave file, without replaying it.
pub fn inspect(path: &Path) -> Option<Recovery> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next()?).ok()?;
    let commands = lines.filter(|l| !l.trim().is_empty()).count();
    if commands == 0 {
        return None;
    }
    Some(Recovery {
        log: path.to_path_buf(),
        project: header.project.map(PathBuf::from),
        commands,
    })
}

/// Replay an autosave file into a session.
///
/// A log that will not replay is corruption, not a hint: it is reported
/// rather than partially applied, so recovery never invents a timeline the
/// user never had.
pub fn replay(path: &Path) -> Result<Session, CliError> {
    let text = fs::read_to_string(path).map_err(|e| CliError::io("read", path.display(), &e))?;
    let mut lines = text.lines();
    let malformed =
        |reason: &str| CliError::Project(davimci_cmd::ProjectError::Malformed(reason.to_string()));
    let first = lines
        .next()
        .ok_or_else(|| malformed("the autosave is empty"))?;
    let header: Header =
        serde_json::from_str(first).map_err(|e| malformed(&format!("bad autosave header: {e}")))?;

    let mut tl = header.snapshot;
    let mut cursor = header.cursor;
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let entry: Entry = serde_json::from_str(line)
            .map_err(|e| malformed(&format!("bad autosave entry: {e}")))?;
        entry.command.apply(&mut tl).map_err(|e| {
            CliError::Project(davimci_cmd::ProjectError::Unreplayable(e.to_string()))
        })?;
        cursor = entry.cursor.or(cursor);
    }
    if let Some(c) = cursor {
        tl.set_id_cursor(c);
    }
    Ok(Session::new(tl))
}
