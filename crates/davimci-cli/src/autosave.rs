//! Continuous autosave of the command log.
//!
//! Two rules shape this module. Autosave never touches the project file -
//! it writes only under `<root>/.davimci/autosave/`, so a crash can never
//! damage the thing the user saved. And it stores the *log*, not the state:
//! appending one line per command is cheap enough to do after every edit,
//! which is what makes recovery land on the exact pre-crash timeline rather
//! than on the last periodic snapshot.
//!
//! The file is JSON lines: line 0 is the root state the history is rebuilt
//! from, and every later line is one tree record - a node, carrying the edge
//! it hangs off, or a move of the current position. Recording the edge is
//! what lets recovery rebuild the *tree* rather than a line, so `g-`/`g+`
//! reach the same branches after a crash as before it. Nodes are never
//! removed by undo, so the file is append-only in the common case.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use davimci_cmd::{EditCommand, NodeId, SavedHistory, SavedNode, Session};
use davimci_core::Timeline;
use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// The first line of an autosave file: the state every node hangs off.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    version: u32,
    /// The project this log belongs to, for the recovery prompt.
    project: Option<String>,
    snapshot: Timeline,
    /// The id cursor of the root state.
    #[serde(default)]
    cursor: Option<u64>,
}

/// One line after the header.
///
/// A node carries its parent, so the tree's shape is on disk and not
/// inferred; `Current` records a move that created no node, which is what an
/// undo, a redo and `g-`/`g+` are.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
enum Record {
    #[serde(rename = "n")]
    Node {
        id: NodeId,
        parent: NodeId,
        seq: u64,
        command: EditCommand,
        inverse: EditCommand,
        id_cursor: u64,
    },
    #[serde(rename = "@")]
    Current { node: NodeId, next_seq: u64 },
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
    /// How many nodes are already on disk, so a sync appends only what is
    /// new without re-reading the file.
    written: usize,
    /// The current node as last recorded, so a pure undo writes one line.
    current: Option<NodeId>,
    /// The header on disk, compared by serialized form.
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
            written: 0,
            current: None,
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
            written: 0,
            current: None,
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
        let Some(history) = session.saved_history() else {
            return Ok(());
        };
        let header = Header {
            version: davimci_cmd::FORMAT_VERSION,
            project: self.project.as_ref().map(|p| p.display().to_string()),
            snapshot: history.root.clone(),
            cursor: Some(history.root_id_cursor),
        };
        let header_line = self.encode(&header)?;

        // A different root means a different history: nothing on disk can be
        // reused, so the file is written again from scratch.
        let extends = self.header.as_deref() == Some(header_line.as_str())
            && history.nodes.len() >= self.written;
        let mut lines = Vec::new();
        if !extends {
            self.written = 0;
            self.current = None;
        }
        for (i, node) in history.nodes.iter().enumerate().skip(self.written) {
            lines.push(self.encode(&record_of(i, node))?);
        }
        if self.current != Some(history.current) || !lines.is_empty() {
            lines.push(self.encode(&Record::Current {
                node: history.current,
                next_seq: history.next_seq,
            })?);
        }
        if extends {
            self.append(&lines)?;
        } else {
            self.rewrite(&header_line, &lines)?;
        }
        self.header = Some(header_line);
        self.written = history.nodes.len();
        self.current = Some(history.current);
        Ok(())
    }

    fn encode<T: Serialize>(&self, value: &T) -> Result<String, CliError> {
        serde_json::to_string(value).map_err(|e| CliError::Io {
            what: "record",
            path: self.path.display().to_string(),
            reason: e.to_string(),
        })
    }

    /// Delete the autosave. Called on `:w` and on a clean close: a file that
    /// survives means the session did not.
    pub fn discard(&mut self) -> Result<(), CliError> {
        self.written = 0;
        self.current = None;
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

    fn append(&self, lines: &[String]) -> Result<(), CliError> {
        if lines.is_empty() {
            return Ok(());
        }
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
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

    fn rewrite(&self, header_line: &str, lines: &[String]) -> Result<(), CliError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| CliError::io("create", dir.display(), &e))?;
        }
        let mut text = String::with_capacity(header_line.len() + 64 * lines.len());
        text.push_str(header_line);
        text.push('\n');
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(&self.path, text).map_err(|e| CliError::io("write", self.path.display(), &e))
    }
}

/// A saved node as it goes on disk. `nodes[i]` is `NodeId(i + 1)`, exactly
/// as in the project format.
fn record_of(index: usize, node: &SavedNode) -> Record {
    Record::Node {
        id: NodeId(index + 1),
        parent: node.parent.unwrap_or(NodeId::ROOT),
        seq: node.seq,
        command: node.command.clone(),
        inverse: node.inverse.clone(),
        id_cursor: node.id_cursor,
    }
}

/// Read an autosave file, without replaying it.
pub fn inspect(path: &Path) -> Option<Recovery> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next()?).ok()?;
    let commands = lines
        .filter_map(|l| serde_json::from_str::<Record>(l).ok())
        .filter(|r| matches!(r, Record::Node { .. }))
        .count();
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
    let malformed =
        |reason: &str| CliError::Project(davimci_cmd::ProjectError::Malformed(reason.to_string()));
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // A crash can cut the last record in half. That record is the only one
    // allowed to be incomplete, and dropping it costs the one edit that was
    // still being written.
    let truncated_tail = !text.ends_with('\n');
    if truncated_tail
        && lines
            .last()
            .is_some_and(|l| serde_json::from_str::<Record>(l).is_err())
    {
        lines.pop();
    }
    let mut lines = lines.into_iter();
    let first = lines
        .next()
        .ok_or_else(|| malformed("the autosave is empty"))?;
    let header: Header =
        serde_json::from_str(first).map_err(|e| malformed(&format!("bad autosave header: {e}")))?;

    let mut nodes: Vec<SavedNode> = Vec::new();
    let mut current = NodeId::ROOT;
    let mut next_seq = 1;
    for line in lines {
        let record: Record = serde_json::from_str(line)
            .map_err(|e| malformed(&format!("bad autosave entry: {e}")))?;
        match record {
            Record::Node {
                id,
                parent,
                seq,
                command,
                inverse,
                id_cursor,
            } => {
                if id.0 != nodes.len() + 1 {
                    return Err(malformed("the autosave records states out of order"));
                }
                next_seq = next_seq.max(seq + 1);
                nodes.push(SavedNode {
                    parent: Some(parent),
                    seq,
                    command,
                    inverse,
                    id_cursor,
                });
            }
            Record::Current { node, next_seq: n } => {
                // A truncated node record can leave `current` pointing past
                // the states that survived; land on the last one instead of
                // refusing to recover at all.
                current = NodeId(node.0.min(nodes.len()));
                next_seq = next_seq.max(n);
            }
        }
    }
    let saved = SavedHistory {
        root: header.snapshot,
        root_id_cursor: header.cursor.unwrap_or_default(),
        nodes,
        current,
        next_seq,
    };
    Session::restored(saved)
        .map_err(|e| CliError::Project(davimci_cmd::ProjectError::Unreplayable(e.to_string())))
}
