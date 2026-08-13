//! Open timelines and the state shared between them.
//!
//! A [`Buffer`] is one open timeline: a `Session`, the path it came from, and
//! its autosave writer. A [`Workspace`] is the set of them plus the state
//! Registers and marks are global - shared across timelines, so a yank
//! in one timeline pastes into another.
//!
//! Marks live on a `Timeline` because that is where the model puts them, so
//! "global" is implemented by syncing: leaving a buffer harvests its marks
//! into [`Globals`], entering one copies them back. A mark's focused track
//! only means something in the timeline it was set in, so it is carried
//! across without a track and lands on the frame alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use davimci_cmd::{EditCommand, ProjectFile, Session};
use davimci_core::{Mark, Register, Timeline, TimelineProps};

use crate::autosave::{self, Autosave, OnRecovery, Recovery};
use crate::error::CliError;

/// State shared by every open timeline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Globals {
    pub registers: BTreeMap<char, Register>,
    pub marks: BTreeMap<char, Mark>,
}

/// One open timeline.
#[derive(Debug)]
pub struct Buffer {
    id: usize,
    session: Session,
    path: Option<PathBuf>,
    /// The history node the file on disk holds. Dirty is a comparison against
    /// it, not a flag, so undoing back to the saved state is clean again -
    /// as in vim.
    saved_at: davimci_cmd::NodeId,
    autosave: Autosave,
    /// True when a recovered log is ahead of the project file.
    recovered: bool,
}

impl Buffer {
    #[must_use]
    pub fn id(&self) -> usize {
        self.id
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    #[must_use]
    pub fn timeline(&self) -> &Timeline {
        self.session.timeline()
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.recovered || self.session.history().current() != self.saved_at
    }

    /// The name `:ls` shows.
    #[must_use]
    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .map_or_else(|| "[No Name]".to_string(), |p| p.display().to_string())
    }
}

/// Every open timeline, plus the state shared across them.
#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    buffers: Vec<Buffer>,
    current: usize,
    globals: Globals,
    next_id: usize,
    autosave_enabled: bool,
    quit: bool,
}

impl Workspace {
    /// A workspace with one empty timeline. `root` is the directory that owns
    /// `.davimci/` - normally the working directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let mut ws = Self {
            root: root.into(),
            buffers: Vec::new(),
            current: 0,
            globals: Globals::default(),
            next_id: 1,
            autosave_enabled: true,
            quit: false,
        };
        ws.push_buffer(Session::new(Timeline::new(TimelineProps::default())), None);
        ws
    }

    /// Turn autosave off for the whole workspace (tests, `--no-autosave`).
    #[must_use]
    pub fn without_autosave(mut self) -> Self {
        self.autosave_enabled = false;
        for b in &mut self.buffers {
            b.autosave = Autosave::disabled();
        }
        self
    }

    /// The directory that owns `.davimci/` - where the analysis cache and
    /// the autosave log live.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    #[must_use]
    pub fn autosave_dir(&self) -> PathBuf {
        self.root.join(".davimci").join("autosave")
    }

    #[must_use]
    pub fn buffers(&self) -> &[Buffer] {
        &self.buffers
    }

    #[must_use]
    pub fn current(&self) -> &Buffer {
        // `buffers` is never empty while the workspace lives: closing the
        // last one sets `quit` and leaves the buffer in place.
        &self.buffers[self.current.min(self.buffers.len() - 1)]
    }

    #[must_use]
    pub fn globals(&self) -> &Globals {
        &self.globals
    }

    pub fn globals_mut(&mut self) -> &mut Globals {
        &mut self.globals
    }

    /// True once `:q` has closed the last timeline.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Run an edit on the current timeline and autosave it.
    ///
    /// This is the only write path a frontend needs for a plain command; for
    /// anything that drives the session itself (the key engine), use
    /// [`Workspace::with_session`], which syncs afterwards either way.
    pub fn exec(&mut self, cmd: &EditCommand) -> Result<String, CliError> {
        let label = self.current_mut().session.exec(cmd)?;
        self.sync_autosave()?;
        Ok(label)
    }

    /// Give a caller the live session, then autosave whatever it did.
    ///
    /// The result is handed straight back, so a rejected edit is still the
    /// caller's error to report - autosave runs regardless, because a
    /// rejected command changed nothing and the sync is then a no-op.
    pub fn with_session<T>(&mut self, f: impl FnOnce(&mut Session) -> T) -> T {
        let out = f(&mut self.current_mut().session);
        // A failed autosave must not lose the caller's result; it degrades
        // to a disabled writer (recoverable-error policy).
        if self.sync_autosave().is_err() {
            self.current_mut().autosave = Autosave::disabled();
        }
        out
    }

    /// Hand the app's live session back to the current buffer.
    ///
    /// The editor's `App` owns the session that is actually being edited, so
    /// the workspace's copy is stale between `:` commands. Pushing before a
    /// command and pulling after ([`Workspace::current_session`]) keeps one
    /// authoritative session rather than two that can drift - which matters
    /// because `:w` writes whatever the workspace holds.
    pub fn set_current_session(&mut self, session: Session) {
        self.current_mut().session = session;
    }

    /// The current buffer's session, for the app to adopt after a command
    /// that may have switched buffers (`:e`, `:bn`, `:b <n>`).
    #[must_use]
    pub fn current_session(&self) -> Session {
        self.current().session.clone()
    }

    /// Bring the current buffer's autosave file up to date.
    pub fn sync_autosave(&mut self) -> Result<(), CliError> {
        let b = self.current_mut();
        let session = &b.session;
        b.autosave.sync(session)
    }

    fn current_mut(&mut self) -> &mut Buffer {
        let i = self.current.min(self.buffers.len() - 1);
        &mut self.buffers[i]
    }

    // -- buffer lifecycle ------------------------------------------------

    fn push_buffer(&mut self, session: Session, path: Option<PathBuf>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let autosave = if self.autosave_enabled {
            Autosave::new(self.autosave_path(id, path.as_deref()), path.clone())
        } else {
            Autosave::disabled()
        };
        let saved_at = session.history().current();
        self.buffers.push(Buffer {
            id,
            session,
            path,
            saved_at,
            autosave,
            recovered: false,
        });
        self.current = self.buffers.len() - 1;
        self.apply_globals();
        id
    }

    fn autosave_path(&self, id: usize, path: Option<&Path>) -> PathBuf {
        let name = match path {
            Some(p) => format!(
                "{}-{:016x}.log",
                p.file_stem()
                    .map_or("project".into(), |s| s.to_string_lossy().to_string()),
                path_key(p)
            ),
            None => format!("untitled-{id}.log"),
        };
        self.autosave_dir().join(name)
    }

    /// Where the autosave for `project` would live.
    #[must_use]
    pub fn autosave_path_for(&self, project: &Path) -> PathBuf {
        self.autosave_path(0, Some(project))
    }

    /// Is there an autosave ahead of this project file (crash
    /// recovery)?
    #[must_use]
    pub fn pending_recovery(&self, project: &Path) -> Option<Recovery> {
        autosave::inspect(&self.autosave_path_for(project))
    }

    /// `:e <path>` for a project file. `on_recovery` answers the crash-
    /// recovery prompt; the caller is the one that can ask a human.
    pub fn open_project(
        &mut self,
        path: impl AsRef<Path>,
        on_recovery: OnRecovery,
    ) -> Result<usize, CliError> {
        let path = path.as_ref().to_path_buf();
        let recovery = self.pending_recovery(&path);
        let (session, recovered) = if let (Some(r), OnRecovery::Recover) = (recovery, on_recovery) {
            (autosave::replay(&r.log)?, true)
        } else {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| CliError::io("read", path.display(), &e))?;
            (ProjectFile::from_json(&text)?.into_session()?, false)
        };
        self.harvest_globals();
        let id = self.push_buffer(session, Some(path));
        if recovered {
            // The recovered state is ahead of the file on disk, so the buffer
            // is dirty until it is saved - the whole point of the prompt.
            self.current_mut().recovered = true;
        } else {
            let _ = self.current_mut().autosave.discard();
        }
        Ok(id)
    }

    /// Take the current state as the buffer's baseline, so it is not dirty
    /// until the user edits it.
    ///
    /// Importing one media file builds a timeline the user never authored;
    /// refusing `:q` on it would demand `:q!` to close something with nothing
    /// to lose.
    pub fn pin_clean(&mut self) {
        let node = self.current().session.history().current();
        let b = self.current_mut();
        b.saved_at = node;
        b.recovered = false;
    }

    /// `:new`: an empty timeline with the given properties.
    pub fn new_timeline(&mut self, props: TimelineProps) -> usize {
        self.harvest_globals();
        self.push_buffer(Session::new(Timeline::new(props)), None)
    }

    /// Adopt an already-built session as a new buffer (used by `:e <media>`,
    /// which imports through `davimci-analysis`).
    pub fn adopt(&mut self, session: Session, path: Option<PathBuf>) -> usize {
        self.harvest_globals();
        self.push_buffer(session, path)
    }

    /// `:w [path]`.
    pub fn write(&mut self, path: Option<PathBuf>) -> Result<PathBuf, CliError> {
        if let Some(p) = path {
            let key = self.autosave_path(self.current().id, Some(&p));
            let b = self.current_mut();
            b.path = Some(p.clone());
            b.autosave.retarget(key, Some(p));
        }
        let Some(target) = self.current().path.clone() else {
            return Err(CliError::NoFilename);
        };
        // Saving pins a snapshot, so the file needs no replay to open.
        self.current_mut().session.mark_saved();
        let text = ProjectFile::from_session(&self.current().session).to_json()?;
        if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| CliError::io("create", dir.display(), &e))?;
        }
        std::fs::write(&target, text).map_err(|e| CliError::io("write", target.display(), &e))?;
        let node = self.current().session.history().current();
        let b = self.current_mut();
        b.saved_at = node;
        b.recovered = false;
        // The file on disk is now authoritative; a stale autosave beside it
        // would produce a spurious recovery prompt.
        let _ = b.autosave.discard();
        Ok(target)
    }

    /// `:q` / `:q!`. Refuses on unsaved changes unless `force`.
    pub fn close(&mut self, force: bool) -> Result<(), CliError> {
        if !force && self.current().is_dirty() {
            return Err(CliError::UnsavedChanges);
        }
        self.harvest_globals();
        let i = self.current.min(self.buffers.len() - 1);
        let mut b = self.buffers.remove(i);
        // A clean close leaves no autosave behind: a surviving file means the
        // session did not survive.
        let _ = b.autosave.discard();
        if self.buffers.is_empty() {
            self.quit = true;
            self.buffers.push(b);
            self.current = 0;
        } else {
            self.current = i.min(self.buffers.len() - 1);
            self.apply_globals();
        }
        Ok(())
    }

    /// `:bn` / `:bp` / `:b <n>` - switching syncs the global state.
    pub fn goto_buffer(&mut self, index: usize) -> Result<(), CliError> {
        if index >= self.buffers.len() {
            return Err(CliError::NoSuchBuffer((index + 1).to_string()));
        }
        self.harvest_globals();
        self.current = index;
        self.apply_globals();
        Ok(())
    }

    /// `:b <n>` by the id shown in `:ls`.
    pub fn goto_buffer_id(&mut self, id: usize) -> Result<(), CliError> {
        let index = self
            .buffers
            .iter()
            .position(|b| b.id == id)
            .ok_or_else(|| CliError::NoSuchBuffer(id.to_string()))?;
        self.goto_buffer(index)
    }

    pub fn next_buffer(&mut self) -> Result<(), CliError> {
        let n = self.buffers.len();
        self.goto_buffer((self.current + 1) % n)
    }

    pub fn prev_buffer(&mut self) -> Result<(), CliError> {
        let n = self.buffers.len();
        self.goto_buffer((self.current + n - 1) % n)
    }

    /// `:ls`.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        self.buffers
            .iter()
            .enumerate()
            .map(|(i, b)| {
                format!(
                    "{:>3} {}{} {}",
                    b.id,
                    if i == self.current { "%" } else { " " },
                    if b.is_dirty() { "+" } else { " " },
                    b.name()
                )
            })
            .collect()
    }

    // -- global registers and marks --------------------------------------

    fn harvest_globals(&mut self) {
        if self.buffers.is_empty() {
            return;
        }
        let marks = self.current().timeline().marks.clone();
        let registers = self.current().timeline().registers.clone();
        for (k, m) in marks {
            self.globals.marks.insert(k, m);
        }
        for (k, r) in registers {
            self.globals.registers.insert(k, r);
        }
    }

    fn apply_globals(&mut self) {
        let globals = self.globals.clone();
        let tracks: Vec<_> = self
            .current()
            .timeline()
            .tracks()
            .iter()
            .map(|t| t.id)
            .collect();
        let b = self.current_mut();
        for (k, r) in globals.registers {
            b.session.set_register(k, r);
        }
        for (k, m) in globals.marks {
            // A mark's track id belongs to the timeline it was set in; keep
            // the frame and drop a track this timeline does not have.
            let track = m.track.filter(|t| tracks.contains(t));
            b.session.set_mark(k, m.frame, track);
        }
    }
}

/// A stable, filesystem-safe key for a project path, so two projects with the
/// same file name do not share an autosave.
fn path_key(path: &Path) -> u64 {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // FNV-1a: stable across runs, which a `DefaultHasher` is not.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in abs.display().to_string().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
