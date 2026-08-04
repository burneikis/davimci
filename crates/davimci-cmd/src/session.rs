//! One editable timeline plus its history.
//!
//! The timeline is not exposed mutably: every write goes through
//! [`Session::exec`], so undo, `.`-repeat, macros, and the project format all
//! see the same command log.

use davimci_core::{Frame, Mark, Register, Timeline, TrackId};

use crate::command::{Command, EditCommand};
use crate::error::CmdError;
use crate::macros::MacroRecorder;
use crate::undo::{NodeId, SavedHistory, UndoEntry, UndoTree};

/// An open timeline and everything that has been done to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    timeline: Timeline,
    history: UndoTree,
    /// The `.` register: the last edit, in a form that can be re-applied.
    last_edit: Option<EditCommand>,
    macros: MacroRecorder,
}

impl Session {
    #[must_use]
    pub fn new(timeline: Timeline) -> Self {
        Self {
            history: UndoTree::new(timeline.clone()),
            timeline,
            last_edit: None,
            macros: MacroRecorder::new(),
        }
    }

    /// Reopen a session with the history it was saved with.
    ///
    /// Undo does not stop at the save point: reopening a project and pressing
    /// `u` steps back through what was done before it was saved, the same way
    /// vim's persistent undo does.
    pub fn restored(saved: SavedHistory) -> Result<Self, CmdError> {
        let (history, timeline) = UndoTree::restore(saved)?;
        Ok(Self {
            history,
            timeline,
            last_edit: None,
            macros: MacroRecorder::new(),
        })
    }

    /// This session's history, ready to save.
    #[must_use]
    pub fn saved_history(&self) -> Option<SavedHistory> {
        self.history.save()
    }

    #[must_use]
    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    #[must_use]
    pub fn history(&self) -> &UndoTree {
        &self.history
    }

    #[must_use]
    pub fn macros(&self) -> &MacroRecorder {
        &self.macros
    }

    /// Move the playhead / change track focus. Not a `Command`: motions are
    /// navigation, not edits, and are never undoable.
    pub fn set_playhead(&mut self, frame: Frame, track: TrackId) -> Result<(), CmdError> {
        self.timeline.set_playhead_frame(frame);
        self.timeline.focus_track(track)?;
        Ok(())
    }

    /// `m<char>`: set a mark at the playhead. Also not a `Command` - marks
    /// are bookkeeping, not timeline content, and are not undoable in vim
    /// either.
    pub fn set_mark(&mut self, name: char, frame: Frame, track: Option<TrackId>) {
        self.timeline.marks.insert(name, Mark { frame, track });
    }

    /// Put content in a named register. Like marks, registers are
    /// bookkeeping rather than timeline content, and they are
    /// global across open timelines - so the workspace, not a command,
    /// decides what they hold.
    pub fn set_register(&mut self, name: char, register: Register) {
        self.timeline.registers.insert(name, register);
    }

    /// Reserve ids for a caller that must pin them before it can build its
    /// commands - see [`davimci_core::Timeline::reserve_ids`]. Not a write to
    /// the timeline's content: the id cursor is bookkeeping, and undo
    /// reconciles it either way.
    pub fn reserve_ids(&mut self, n: usize) -> Vec<u64> {
        self.timeline.reserve_ids(n)
    }

    pub fn macros_mut(&mut self) -> &mut MacroRecorder {
        &mut self.macros
    }

    /// Drift-guard interval: a full snapshot every `n` commands.
    pub fn set_snapshot_interval(&mut self, n: u64) {
        self.history.set_snapshot_interval(n);
    }

    /// Run a command and record it. A rejected command mutates nothing and
    /// never enters the log.
    pub fn exec(&mut self, command: &EditCommand) -> Result<String, CmdError> {
        let effect = command.apply(&mut self.timeline)?;
        let label = effect.applied.describe();
        self.history
            .record(effect.applied, effect.inverse, &self.timeline);
        self.last_edit = Some(command.clone());
        Ok(label)
    }

    /// `.`: run the last edit again, minting fresh clips where it created
    /// any.
    pub fn repeat(&mut self) -> Result<String, CmdError> {
        let last = self
            .last_edit
            .clone()
            .ok_or(CmdError::NothingToRepeat)?
            .for_repeat();
        self.exec(&last)
    }

    /// The command `.` would repeat.
    #[must_use]
    pub fn last_edit(&self) -> Option<&EditCommand> {
        self.last_edit.as_ref()
    }

    /// `u`.
    pub fn undo(&mut self) -> Result<String, CmdError> {
        self.history.undo(&mut self.timeline)
    }

    /// `Ctrl-r`.
    pub fn redo(&mut self) -> Result<String, CmdError> {
        self.history.redo(&mut self.timeline)
    }

    /// `g-`.
    pub fn time_travel_back(&mut self) -> Result<u64, CmdError> {
        self.history.time_travel_back(&mut self.timeline)
    }

    /// `g+`.
    pub fn time_travel_forward(&mut self) -> Result<u64, CmdError> {
        self.history.time_travel_forward(&mut self.timeline)
    }

    /// Jump to any state in the tree.
    pub fn goto(&mut self, node: NodeId) -> Result<(), CmdError> {
        self.history.goto(node, &mut self.timeline)
    }

    /// `:undolist`.
    #[must_use]
    pub fn undolist(&self) -> Vec<UndoEntry> {
        self.history.undolist()
    }

    /// Snapshot the current state, as `:w` does.
    pub fn mark_saved(&mut self) {
        self.history.snapshot_now(&self.timeline);
    }

    /// Test hook for the drift guard: record a command with an inverse that
    /// is deliberately wrong, so undo produces a corrupt state that the next
    /// snapshot must correct.
    #[cfg(test)]
    pub(crate) fn exec_with_inverse(
        &mut self,
        command: &EditCommand,
        inverse: EditCommand,
    ) -> Result<(), CmdError> {
        let effect = command.apply(&mut self.timeline)?;
        self.history.record(effect.applied, inverse, &self.timeline);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_core::Frame;
    use davimci_core::testing::{fixture, track_id};

    fn scene() -> Session {
        Session::new(fixture(&[("V1", &[(0, 300, "a")])]))
    }

    fn split(s: &Session, frame: u64) -> EditCommand {
        EditCommand::Split {
            track: track_id(s.timeline(), "V1"),
            frame: Frame(frame),
            new_id: None,
        }
    }

    fn state(s: &Session) -> String {
        serde_json::to_string(s.timeline()).unwrap()
    }

    #[test]
    fn undo_restores_byte_identical_state() {
        let mut s = scene();
        let start = state(&s);
        let cmds = [split(&s, 100), split(&s, 200), split(&s, 50)];
        for c in &cmds {
            s.exec(c).unwrap();
        }
        assert_eq!(
            s.timeline().dump(),
            "V1:[a 0-50][a 50-100][a 100-200][a 200-300]\nA1: -\n"
        );
        for _ in 0..cmds.len() {
            s.undo().unwrap();
        }
        assert_eq!(state(&s), start);
        assert_eq!(s.undo(), Err(CmdError::NothingToUndo));
    }

    #[test]
    fn redo_reproduces_byte_identical_state() {
        let mut s = scene();
        s.exec(&split(&s, 100)).unwrap();
        s.exec(&split(&s, 200)).unwrap();
        let done = state(&s);
        s.undo().unwrap();
        s.undo().unwrap();
        s.redo().unwrap();
        s.redo().unwrap();
        assert_eq!(state(&s), done);
        assert_eq!(s.redo(), Err(CmdError::NothingToRedo));
    }

    #[test]
    fn redo_after_branching_follows_the_newest_branch() {
        let mut s = scene();
        s.exec(&split(&s, 100)).unwrap();
        s.undo().unwrap();
        s.exec(&split(&s, 200)).unwrap();
        let newest = state(&s);
        s.undo().unwrap();
        s.redo().unwrap();
        assert_eq!(state(&s), newest, "redo must take the newest branch");
        assert_eq!(s.undolist().len(), 2, "the abandoned branch is kept");
    }

    #[test]
    fn time_travel_visits_states_in_change_order_across_branches() {
        let mut s = scene();
        s.exec(&split(&s, 100)).unwrap(); // seq 1
        s.undo().unwrap();
        s.exec(&split(&s, 200)).unwrap(); // seq 2, on a new branch
        assert_eq!(s.time_travel_back(), Ok(1));
        assert_eq!(s.timeline().dump(), "V1:[a 0-100][a 100-300]\nA1: -\n");
        assert_eq!(s.time_travel_back(), Ok(0));
        assert_eq!(s.timeline().dump(), "V1:[a 0-300]\nA1: -\n");
        assert_eq!(s.time_travel_back(), Err(CmdError::NothingToUndo));
        assert_eq!(s.time_travel_forward(), Ok(1));
        assert_eq!(s.time_travel_forward(), Ok(2));
        assert_eq!(s.timeline().dump(), "V1:[a 0-200][a 200-300]\nA1: -\n");
        assert_eq!(s.time_travel_forward(), Err(CmdError::NothingToRedo));
    }

    #[test]
    fn a_rejected_command_never_enters_the_log() {
        let mut s = scene();
        let before = state(&s);
        // Frame 0 is already a cut, so there is nothing to split.
        assert!(s.exec(&split(&s, 0)).is_err());
        assert_eq!(state(&s), before);
        assert!(s.undolist().is_empty());
        assert_eq!(s.undo(), Err(CmdError::NothingToUndo));
    }

    #[test]
    fn repeat_reruns_the_last_edit() {
        let mut s = scene();
        s.exec(&split(&s, 100)).unwrap();
        assert!(s.last_edit().is_some());
        // `.` needs a fresh target; the repeat form mints its own clip id.
        s.exec(&split(&s, 200)).unwrap();
        let before_repeat = s.timeline().dump();
        assert!(
            s.repeat().is_err(),
            "repeating an impossible split is rejected"
        );
        assert_eq!(s.timeline().dump(), before_repeat);

        let mut s = scene();
        assert_eq!(s.repeat(), Err(CmdError::NothingToRepeat));
        s.exec(&split(&s, 100)).unwrap();
        s.undo().unwrap();
        s.repeat().unwrap();
        assert_eq!(s.timeline().dump(), "V1:[a 0-100][a 100-300]\nA1: -\n");
    }

    /// A deliberately corrupt inverse must cost at most the
    /// commands since the last snapshot, never the project.
    #[test]
    fn a_snapshot_bounds_the_damage_from_a_bad_inverse() {
        let mut s = scene();
        s.set_snapshot_interval(2);
        s.exec(&split(&s, 100)).unwrap();
        s.exec(&split(&s, 200)).unwrap(); // seq 2: snapshotted
        let snapshotted = state(&s);

        // A third edit whose "inverse" does something unrelated.
        let bogus = EditCommand::Split {
            track: track_id(s.timeline(), "V1"),
            frame: Frame(250),
            new_id: None,
        };
        let wrong_inverse = EditCommand::Sequence(vec![]);
        s.exec_with_inverse(&bogus, wrong_inverse).unwrap();

        // Undo runs the wrong inverse, and the snapshot puts it right.
        s.undo().unwrap();
        assert_eq!(state(&s), snapshotted, "the snapshot must win");
    }

    #[test]
    fn saving_pins_a_snapshot_at_the_current_state() {
        let mut s = scene();
        s.set_snapshot_interval(0);
        s.exec(&split(&s, 100)).unwrap();
        s.mark_saved();
        let (snapshot, log) = s.history().compacted().unwrap();
        assert!(log.is_empty(), "the save point needs no replay");
        assert_eq!(&snapshot, s.timeline());
    }

    #[test]
    fn macros_live_alongside_the_history() {
        let mut s = scene();
        s.macros_mut().start('a').unwrap();
        s.macros_mut().push("s");
        s.macros_mut().stop().unwrap();
        assert_eq!(s.macros().replay('a').map(<[String]>::len), Ok(1));
    }
}
