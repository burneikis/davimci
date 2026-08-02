//! The frontend-agnostic event loop (plan.md Phase 9a).
//!
//! Everything a frontend would otherwise reimplement lives here: feeding keys
//! to `davimci-keys`, turning an [`Outcome`] into a status-line message,
//! keeping the playhead on screen, and assembling the [`ViewState`]. A
//! frontend that skipped this would be a second editor.

use davimci_cmd::Session;
use davimci_keys::engine::{Outcome, TransportCmd};
use davimci_keys::{Engine, Key, Keymap, Mode};
use davimci_motion::{JumpConfig, Zoom};

use crate::error::AppError;
use crate::frontend::{Event, Frontend, Response, Surface};
use crate::job::JobList;
use crate::message::{Message, MessageQueue};
use crate::view::{ViewInputs, ViewState};
use crate::viewport::Viewport;

/// The host's hook for things the editor core deliberately does not own:
/// `:` commands (`davimci-cli` owns the vocabulary), transport (the backend
/// clock owns playback), and Lua callbacks (`davimci-lua` owns the runtime).
///
/// Defaulted so tests and the headless frontend can ignore all three.
pub trait Host {
    /// Run a submitted `:` line. Returning `Ok(None)` means "handled, nothing
    /// to say".
    fn command(&mut self, line: &str, session: &mut Session) -> Result<Option<String>, AppError> {
        let _ = session;
        Err(AppError::UnhandledCommand(line.to_string()))
    }

    /// Dispatch a transport action to the backend clock. Never an edit, so it
    /// never reaches the undo log (spec §3.2.1).
    fn transport(&mut self, cmd: TransportCmd) {
        let _ = cmd;
    }

    /// Invoke Lua callback `id` and execute whatever it asks for.
    fn plugin(&mut self, id: u32, session: &mut Session) -> Result<Option<String>, AppError> {
        let _ = (id, session);
        Ok(None)
    }

    /// One pass of whatever the host drives off the clock: pacing a preview
    /// frame, stepping a shuttle, polling a job. Called on every
    /// [`Event::Tick`].
    ///
    /// It takes the session because playback *moves the playhead*, and the
    /// playhead is navigation rather than an edit - so this is the same
    /// non-command escape hatch `set_playhead` already is, not a second
    /// write path.
    fn tick(&mut self, session: &mut Session) {
        let _ = session;
    }

    /// An edit was committed: the render graph is now stale. Called after
    /// anything that reaches the undo log, including undo and redo.
    fn timeline_changed(&mut self, session: &Session) {
        let _ = session;
    }

    /// The playhead or track focus moved, so the backend should seek and the
    /// preview should show that frame.
    fn playhead_moved(&mut self, session: &Session) {
        let _ = session;
    }

    /// True once the host wants the loop to stop (`:q` succeeded).
    fn wants_quit(&self) -> bool {
        false
    }
}

/// A host that does nothing, for tests and the headless frontend.
#[derive(Debug, Default)]
pub struct NullHost;

impl Host for NullHost {}

/// One open timeline, its key engine, and its view state.
#[derive(Debug)]
pub struct App {
    session: Session,
    engine: Engine,
    viewport: Viewport,
    jump_cfg: JumpConfig,
    messages: MessageQueue,
    jobs: JobList,
    command_line: Option<String>,
    quit: bool,
}

impl App {
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self::with_keymap(session, Keymap::new())
    }

    #[must_use]
    pub fn with_keymap(session: Session, keymap: Keymap) -> Self {
        Self {
            session,
            engine: Engine::with_keymap(keymap),
            viewport: Viewport::default(),
            jump_cfg: JumpConfig::default(),
            messages: MessageQueue::default(),
            jobs: JobList::default(),
            command_line: None,
            quit: false,
        }
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Mutable access for the host binary, which owns project lifecycle.
    /// Still not a second write path: everything it can do to the timeline
    /// goes through [`Session::exec`].
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Adopt a different timeline - `:e`, `:bn`, `:b <n>`. The viewport and
    /// the key engine are reset to the new timeline rather than carried
    /// over, since a column, a selection and a pending sequence all mean
    /// something only in the timeline they were made in.
    pub fn replace_session(&mut self, session: Session) {
        self.session = session;
        self.engine.reset();
        self.viewport = Viewport::new(self.viewport.columns(), self.viewport.rows());
        self.engine.set_zoom(self.viewport.zoom());
        self.follow();
    }

    #[must_use]
    pub fn messages(&self) -> &MessageQueue {
        &self.messages
    }

    pub fn jobs_mut(&mut self) -> &mut JobList {
        &mut self.jobs
    }

    #[must_use]
    pub fn jobs(&self) -> &JobList {
        &self.jobs
    }

    #[must_use]
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn set_jump_config(&mut self, cfg: JumpConfig) {
        self.jump_cfg = cfg;
    }

    pub fn notify(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn resize(&mut self, surface: Surface) {
        self.viewport.resize(surface.columns, surface.rows);
        self.follow();
    }

    /// Zoom is an app-level concern, not a keybinding: spec §11 defines no
    /// zoom key, so frontends drive this from a wheel or a menu and the
    /// anchoring rule stays in one place.
    pub fn zoom_in(&mut self) {
        let ph = self.session.timeline().playhead().frame;
        self.viewport
            .zoom_in(ph, self.session.timeline().duration());
        self.engine.set_zoom(self.viewport.zoom());
    }

    pub fn zoom_out(&mut self) {
        let ph = self.session.timeline().playhead().frame;
        self.viewport
            .zoom_out(ph, self.session.timeline().duration());
        self.engine.set_zoom(self.viewport.zoom());
    }

    pub fn set_zoom(&mut self, zoom: Zoom) {
        let ph = self.session.timeline().playhead().frame;
        self.viewport
            .set_zoom(zoom, ph, self.session.timeline().duration());
        self.engine.set_zoom(zoom);
    }

    pub fn scroll_columns(&mut self, delta: i64) {
        let ph = self.session.timeline().playhead().frame;
        self.viewport
            .scroll_columns(delta, ph, self.session.timeline().duration());
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.engine.mode()
    }

    #[must_use]
    pub fn wants_quit(&self) -> bool {
        self.quit
    }

    pub fn quit(&mut self) {
        self.quit = true;
    }

    /// Assemble the current view. Cheap enough to call every frame; it copies
    /// only what is on screen.
    #[must_use]
    pub fn view(&self) -> ViewState {
        let inputs = ViewInputs {
            mode: self.engine.mode(),
            selection: self.engine.mode_state().visual(),
            pending: String::new(),
            command_line: self.command_line.clone(),
            message: self.messages.current().cloned(),
            job: self.jobs.foreground().cloned(),
            recording: self.session.macros().recording(),
        };
        ViewState::build(&self.session, self.viewport, &self.jump_cfg, &inputs)
    }

    /// Feed one key. The single entry point for every frontend's input path.
    pub fn key(&mut self, key: Key, host: &mut dyn Host) -> Response {
        let outcome = self.engine.feed(key, &mut self.session);
        self.apply_outcome(outcome, host)
    }

    /// Handle one frontend event.
    pub fn event(&mut self, event: Event, host: &mut dyn Host) -> Response {
        match event {
            Event::Key(k) => self.key(k, host),
            Event::Resize(s) => {
                self.resize(s);
                Response::Continue
            }
            Event::Command(line) => {
                self.command_line = None;
                match host.command(&line, &mut self.session) {
                    Ok(Some(msg)) => self.messages.push(Message::info(msg)),
                    Ok(None) => {}
                    Err(e) => self.messages.push(Message::error(e.to_string())),
                }
                // A `:` line can edit (`:relink`) or swap the whole timeline
                // (`:e`, `:bn`), so the graph and the playhead are both
                // assumed stale rather than diffed.
                host.timeline_changed(&self.session);
                host.playhead_moved(&self.session);
                self.follow();
                if host.wants_quit() {
                    self.quit = true;
                    return Response::Quit;
                }
                Response::Continue
            }
            Event::CommandCancelled => {
                self.command_line = None;
                Response::Continue
            }
            Event::Tick => {
                host.tick(&mut self.session);
                self.follow();
                Response::Continue
            }
            Event::Quit => {
                self.quit = true;
                Response::Quit
            }
        }
    }

    fn apply_outcome(&mut self, outcome: Outcome, host: &mut dyn Host) -> Response {
        // An edit invalidates the render graph; an edit or a motion moves the
        // playhead. Both are reported once, here, so a host cannot miss one
        // by handling only some outcomes.
        let edited = matches!(outcome, Outcome::Applied(_));
        let moved = edited || matches!(outcome, Outcome::Moved);
        match outcome {
            Outcome::Pending | Outcome::Cancelled => {}
            // `:` is a mode change in `davimci-keys`, not a distinct outcome:
            // the grammar knows only that COMMAND was entered. Owning the
            // `:` line is the frontend's job, so the app hands it over here.
            Outcome::Mode(change) => {
                if change.to == Mode::Command {
                    self.command_line = Some(String::new());
                    self.follow();
                    return Response::OpenCommandLine;
                }
                if change.from == Mode::Command {
                    self.command_line = None;
                }
            }
            Outcome::Invalid => self.messages.push(Message::warning(
                "That key sequence is not bound to anything.".to_string(),
            )),
            Outcome::Applied(label) => self.messages.push(Message::info(label)),
            Outcome::Moved => {}
            Outcome::PredicatePending => self.messages.push(Message::warning(
                "Analysis is still running; that motion cannot be resolved yet.".to_string(),
            )),
            Outcome::MacroStarted(r) => self
                .messages
                .push(Message::info(format!("Recording into register {r}."))),
            Outcome::MacroStopped(r) => self
                .messages
                .push(Message::info(format!("Stopped recording register {r}."))),
            Outcome::Replayed(outcomes) => {
                for o in outcomes {
                    self.apply_outcome(o, host);
                }
            }
            Outcome::Transport(cmd) => host.transport(cmd),
            Outcome::EnterCommandMode => {
                self.command_line = Some(String::new());
                self.follow();
                return Response::OpenCommandLine;
            }
            Outcome::Plugin(id) => match host.plugin(id, &mut self.session) {
                Ok(Some(msg)) => self.messages.push(Message::info(msg)),
                Ok(None) => {}
                Err(e) => self.messages.push(Message::error(e.to_string())),
            },
            Outcome::NotImplemented(what) => self
                .messages
                .push(Message::warning(format!("Not implemented yet: {what}."))),
            Outcome::Error(msg) => self.messages.push(Message::error(msg)),
        }
        if edited {
            host.timeline_changed(&self.session);
        }
        if moved {
            host.playhead_moved(&self.session);
        }
        self.follow();
        Response::Continue
    }

    /// Scroll-follow: the playhead and the focused track are visible after
    /// anything that could have moved either.
    fn follow(&mut self) {
        let tl = self.session.timeline();
        let ph = tl.playhead();
        let duration = tl.duration();
        let index = tl
            .tracks()
            .iter()
            .position(|t| t.id == ph.track)
            .unwrap_or(0);
        let count = tl.tracks().len();
        self.viewport.follow_playhead(ph.frame, duration);
        self.viewport.follow_track(index, count);
    }

    /// Drive a frontend until it quits. Rendering errors are reported and the
    /// loop continues (Phase 0: recoverable errors degrade locally).
    pub fn run<F: Frontend>(
        &mut self,
        frontend: &mut F,
        host: &mut dyn Host,
    ) -> Result<(), AppError> {
        frontend.on_start()?;
        self.resize(frontend.surface());
        let result = self.pump(frontend, host);
        frontend.on_stop();
        result
    }

    fn pump<F: Frontend>(&mut self, frontend: &mut F, host: &mut dyn Host) -> Result<(), AppError> {
        loop {
            for event in frontend.poll() {
                if self.event(event, host) == Response::Quit {
                    return Ok(());
                }
            }
            if self.quit || host.wants_quit() {
                return Ok(());
            }
            let view = self.view();
            if let Err(e) = frontend.render(&view) {
                self.messages.push(Message::error(e.to_string()));
            }
        }
    }
}
