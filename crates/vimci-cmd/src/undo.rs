//! The undo tree (spec §10.4).
//!
//! History branches: undoing and then editing again keeps the abandoned
//! branch, reachable with `g-`/`g+` and listed by `:undolist`. Nodes hold the
//! command as executed plus its inverse, and every `snapshot_interval`th node
//! also holds a full timeline - the drift guard that bounds what a buggy
//! inverse can cost (plan.md Phase 2).

use serde::{Deserialize, Serialize};
use vimci_core::Timeline;

use crate::command::{Command, EditCommand};
use crate::error::CmdError;

/// Index of a node in the tree. The root is always [`NodeId::ROOT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

impl NodeId {
    pub const ROOT: Self = Self(0);
}

/// The default drift-guard interval (spec §10.4).
pub const DEFAULT_SNAPSHOT_INTERVAL: u64 = 100;

#[derive(Debug, Clone, PartialEq)]
struct Node {
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// Monotonic change number; the root is 0. This is the `g-`/`g+` order.
    seq: u64,
    /// The command that produced this state, and the one that undoes it.
    /// `None` only for the root.
    edit: Option<Edit>,
    snapshot: Option<Timeline>,
    /// The timeline's id cursor in this state. Restoring it is what makes
    /// undo and redo byte-exact rather than merely content-exact.
    id_cursor: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct Edit {
    command: EditCommand,
    inverse: EditCommand,
}

/// One row of `:undolist`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoEntry {
    pub seq: u64,
    pub node: NodeId,
    pub description: String,
    /// Whether this is the state the editor is currently in.
    pub current: bool,
    /// Distance from the root, i.e. how many changes deep this state is.
    pub depth: usize,
}

/// History of one timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct UndoTree {
    nodes: Vec<Node>,
    current: NodeId,
    next_seq: u64,
    snapshot_interval: u64,
}

impl UndoTree {
    /// Start a history rooted at `initial`. The root always keeps its
    /// snapshot, so any state can be rebuilt from scratch.
    #[must_use]
    pub fn new(initial: Timeline) -> Self {
        Self {
            nodes: vec![Node {
                parent: None,
                children: Vec::new(),
                seq: 0,
                edit: None,
                id_cursor: initial.id_cursor(),
                snapshot: Some(initial),
            }],
            current: NodeId::ROOT,
            next_seq: 1,
            snapshot_interval: DEFAULT_SNAPSHOT_INTERVAL,
        }
    }

    /// Take a full snapshot every `n` commands. `0` means never (the root
    /// snapshot still exists).
    pub fn set_snapshot_interval(&mut self, n: u64) {
        self.snapshot_interval = n;
    }

    #[must_use]
    pub fn current(&self) -> NodeId {
        self.current
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len() - 1
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn at_root(&self) -> bool {
        self.current == NodeId::ROOT
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0)
    }

    fn depth(&self, id: NodeId) -> usize {
        let mut d = 0;
        let mut cur = id;
        while let Some(p) = self.node(cur).and_then(|n| n.parent) {
            d += 1;
            cur = p;
        }
        d
    }

    /// Record a command that has already been applied to `state`.
    pub fn record(&mut self, command: EditCommand, inverse: EditCommand, state: &Timeline) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let take_snapshot =
            self.snapshot_interval != 0 && seq.is_multiple_of(self.snapshot_interval);
        let id = NodeId(self.nodes.len());
        self.nodes.push(Node {
            parent: Some(self.current),
            children: Vec::new(),
            seq,
            edit: Some(Edit { command, inverse }),
            id_cursor: state.id_cursor(),
            snapshot: take_snapshot.then(|| state.clone()),
        });
        if let Some(parent) = self.nodes.get_mut(self.current.0) {
            parent.children.push(id);
        }
        self.current = id;
    }

    /// Pin the current state, as `:w` does (spec §10.4: snapshot on save).
    pub fn snapshot_now(&mut self, state: &Timeline) {
        if let Some(n) = self.nodes.get_mut(self.current.0) {
            n.snapshot = Some(state.clone());
        }
    }

    /// The inverse that undoes the current state, and the node it lands on.
    fn undo_step(&self) -> Option<(EditCommand, NodeId)> {
        let n = self.node(self.current)?;
        let edit = n.edit.as_ref()?;
        Some((edit.inverse.clone(), n.parent?))
    }

    /// The command that redoes into the newest branch, and the node it lands
    /// on. Vim's rule: redo follows the most recently created branch.
    fn redo_step(&self) -> Option<(EditCommand, NodeId)> {
        let child = *self.node(self.current)?.children.last()?;
        let edit = self.node(child)?.edit.as_ref()?;
        Some((edit.command.clone(), child))
    }

    /// `u`: step back one change.
    pub fn undo(&mut self, tl: &mut Timeline) -> Result<String, CmdError> {
        let (inverse, target) = self.undo_step().ok_or(CmdError::NothingToUndo)?;
        let label = self
            .node(self.current)
            .and_then(|n| n.edit.as_ref())
            .map_or_else(String::new, |e| e.command.describe());
        inverse.apply(tl).map_err(drift)?;
        self.current = target;
        self.reconcile(tl)?;
        Ok(label)
    }

    /// `Ctrl-r`: step forward into the newest branch.
    pub fn redo(&mut self, tl: &mut Timeline) -> Result<String, CmdError> {
        let (command, target) = self.redo_step().ok_or(CmdError::NothingToRedo)?;
        command.apply(tl).map_err(drift)?;
        self.current = target;
        self.reconcile(tl)?;
        Ok(command.describe())
    }

    /// `g-`: the previous state in change order, across branches.
    pub fn time_travel_back(&mut self, tl: &mut Timeline) -> Result<u64, CmdError> {
        let here = self.node(self.current).map_or(0, |n| n.seq);
        let target = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.seq < here)
            .max_by_key(|(_, n)| n.seq)
            .map(|(i, _)| NodeId(i))
            .ok_or(CmdError::NothingToUndo)?;
        self.goto(target, tl)?;
        Ok(self.node(target).map_or(0, |n| n.seq))
    }

    /// `g+`: the next state in change order, across branches.
    pub fn time_travel_forward(&mut self, tl: &mut Timeline) -> Result<u64, CmdError> {
        let here = self.node(self.current).map_or(0, |n| n.seq);
        let target = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.seq > here)
            .min_by_key(|(_, n)| n.seq)
            .map(|(i, _)| NodeId(i))
            .ok_or(CmdError::NothingToRedo)?;
        self.goto(target, tl)?;
        Ok(self.node(target).map_or(0, |n| n.seq))
    }

    /// Move to any state in the tree, walking up to the common ancestor and
    /// back down. Falls back to a snapshot rebuild if a step fails.
    pub fn goto(&mut self, target: NodeId, tl: &mut Timeline) -> Result<(), CmdError> {
        if self.node(target).is_none() {
            return Err(CmdError::ReplayFailed(format!(
                "state {} is not in the history",
                target.0
            )));
        }
        if target == self.current {
            return Ok(());
        }
        let up = self.path_to_root(self.current);
        let down = self.path_to_root(target);
        let ancestor = up
            .iter()
            .find(|n| down.contains(n))
            .copied()
            .unwrap_or(NodeId::ROOT);

        let mut work = tl.clone();
        let mut ok = true;
        for id in up.iter().take_while(|n| **n != ancestor) {
            let Some(inv) = self.node(*id).and_then(|n| n.edit.as_ref()) else {
                ok = false;
                break;
            };
            if inv.inverse.apply(&mut work).is_err() {
                ok = false;
                break;
            }
        }
        if ok {
            let mut descend: Vec<NodeId> = down
                .iter()
                .take_while(|n| **n != ancestor)
                .copied()
                .collect();
            descend.reverse();
            for id in descend {
                let Some(edit) = self.node(id).and_then(|n| n.edit.as_ref()) else {
                    ok = false;
                    break;
                };
                if edit.command.apply(&mut work).is_err() {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            *tl = work;
        } else {
            *tl = self.rebuild(target)?;
        }
        self.current = target;
        self.reconcile(tl)?;
        Ok(())
    }

    /// Rebuild a state from the nearest snapshot, replaying forward.
    ///
    /// This is the drift guard: the root always has a snapshot, so this can
    /// always succeed unless the log itself is unreplayable.
    pub fn rebuild(&self, target: NodeId) -> Result<Timeline, CmdError> {
        let path = self.path_to_root(target);
        let (idx, base) = path
            .iter()
            .enumerate()
            .find_map(|(i, id)| {
                self.node(*id)
                    .and_then(|n| n.snapshot.as_ref())
                    .map(|s| (i, s.clone()))
            })
            .ok_or_else(|| CmdError::ReplayFailed("no snapshot to rebuild from".into()))?;
        let mut tl = base;
        for id in path.iter().take(idx).rev() {
            let Some(edit) = self.node(*id).and_then(|n| n.edit.as_ref()) else {
                return Err(CmdError::ReplayFailed(format!(
                    "state {} has no command",
                    id.0
                )));
            };
            edit.command.apply(&mut tl).map_err(drift)?;
        }
        Ok(tl)
    }

    /// If the state we just moved to has a snapshot, that snapshot is the
    /// truth. Adopting it bounds any drift a wrong inverse introduced to the
    /// commands since the last snapshot.
    fn reconcile(&self, tl: &mut Timeline) -> Result<(), CmdError> {
        let Some(node) = self.node(self.current) else {
            return Err(CmdError::ReplayFailed(
                "the history points at a state that does not exist".into(),
            ));
        };
        tl.set_id_cursor(node.id_cursor);
        if let Some(snap) = node.snapshot.as_ref()
            && *tl != *snap
        {
            *tl = snap.clone();
        }
        Ok(())
    }

    /// `current`, its parent, and so on up to the root.
    fn path_to_root(&self, from: NodeId) -> Vec<NodeId> {
        let mut path = vec![from];
        let mut cur = from;
        while let Some(p) = self.node(cur).and_then(|n| n.parent) {
            path.push(p);
            cur = p;
        }
        path
    }

    /// `:undolist`, in change order.
    #[must_use]
    pub fn undolist(&self) -> Vec<UndoEntry> {
        let mut rows: Vec<UndoEntry> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let edit = n.edit.as_ref()?;
                Some(UndoEntry {
                    seq: n.seq,
                    node: NodeId(i),
                    description: edit.command.describe(),
                    current: NodeId(i) == self.current,
                    depth: self.depth(NodeId(i)),
                })
            })
            .collect();
        rows.sort_by_key(|r| r.seq);
        rows
    }

    /// The commands from the nearest snapshot up to the current state, and
    /// that snapshot - the project file's two halves (spec §10.4).
    #[must_use]
    pub fn compacted(&self) -> Option<(Timeline, Vec<EditCommand>)> {
        let path = self.path_to_root(self.current);
        let (idx, base) = path.iter().enumerate().find_map(|(i, id)| {
            self.node(*id)
                .and_then(|n| n.snapshot.as_ref())
                .map(|s| (i, s.clone()))
        })?;
        let log = path
            .iter()
            .take(idx)
            .rev()
            .filter_map(|id| self.node(*id).and_then(|n| n.edit.as_ref()))
            .map(|e| e.command.clone())
            .collect();
        Some((base, log))
    }
}

/// A failure while replaying history is corruption, not a user error.
fn drift(e: CmdError) -> CmdError {
    match e {
        CmdError::ReplayFailed(_) => e,
        other => CmdError::ReplayFailed(other.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use vimci_core::Frame;
    use vimci_core::testing::{fixture, track_id};

    fn split(tl: &Timeline, frame: u64) -> EditCommand {
        EditCommand::Split {
            track: track_id(tl, "V1"),
            frame: Frame(frame),
            new_id: None,
        }
    }

    /// Apply `cmd` and record it, as a session would.
    fn exec(tree: &mut UndoTree, tl: &mut Timeline, cmd: &EditCommand) {
        let e = cmd.apply(tl).unwrap();
        tree.record(e.applied, e.inverse, tl);
    }

    fn scene() -> (UndoTree, Timeline) {
        let tl = fixture(&[("V1", &[(0, 300, "a")])]);
        (UndoTree::new(tl.clone()), tl)
    }

    #[test]
    fn a_fresh_tree_holds_only_the_root() {
        let (tree, _) = scene();
        assert!(tree.is_empty());
        assert!(tree.at_root());
        assert!(tree.undolist().is_empty());
    }

    #[test]
    fn undolist_reports_change_order_depth_and_position() {
        let (mut tree, mut tl) = scene();
        let (a, b) = (split(&tl, 100), split(&tl, 200));
        exec(&mut tree, &mut tl, &a);
        exec(&mut tree, &mut tl, &b);
        tree.undo(&mut tl).unwrap();

        let rows = tree.undolist();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].seq, 1);
        assert_eq!(rows[0].depth, 1);
        assert!(rows[0].current, "undo left us on the first change");
        assert_eq!(rows[1].seq, 2);
        assert!(!rows[1].current);
        assert!(rows[1].description.starts_with("split at 200"));
    }

    #[test]
    fn a_snapshot_lets_a_state_be_rebuilt_from_scratch() {
        let (mut tree, mut tl) = scene();
        tree.set_snapshot_interval(2);
        let cmd = split(&tl, 100);
        exec(&mut tree, &mut tl, &cmd);
        let cmd = split(&tl, 200);
        exec(&mut tree, &mut tl, &cmd);
        let rebuilt = tree.rebuild(tree.current()).unwrap();
        assert_eq!(rebuilt, tl);
    }

    #[test]
    fn compacting_gives_the_snapshot_plus_the_log_since_it() {
        let (mut tree, mut tl) = scene();
        tree.set_snapshot_interval(0);
        let cmd = split(&tl, 100);
        exec(&mut tree, &mut tl, &cmd);
        let cmd = split(&tl, 200);
        exec(&mut tree, &mut tl, &cmd);
        let (snapshot, log) = tree.compacted().unwrap();
        // Only the root is snapshotted, so the log carries both edits.
        assert_eq!(log.len(), 2);
        let mut replayed = snapshot;
        for cmd in &log {
            cmd.apply(&mut replayed).unwrap();
        }
        assert_eq!(replayed.dump(), tl.dump());
    }

    #[test]
    fn goto_walks_across_branches() {
        let (mut tree, mut tl) = scene();
        let cmd = split(&tl, 100);
        exec(&mut tree, &mut tl, &cmd);
        let first_branch = tree.current();
        let at_100 = tl.dump();
        tree.undo(&mut tl).unwrap();
        let cmd = split(&tl, 200);
        exec(&mut tree, &mut tl, &cmd);
        let second_branch = tree.current();

        tree.goto(first_branch, &mut tl).unwrap();
        assert_eq!(tl.dump(), at_100);
        tree.goto(second_branch, &mut tl).unwrap();
        assert_eq!(tl.dump(), "V1:[a 0-200][a 200-300]\nA1: -\n");
    }

    #[test]
    fn goto_rejects_a_state_that_is_not_in_the_history() {
        let (mut tree, mut tl) = scene();
        assert!(matches!(
            tree.goto(NodeId(42), &mut tl),
            Err(CmdError::ReplayFailed(_))
        ));
    }
}
