//! The frontend-agnostic event loop (plan.md Phase 9a).
//!
//! Everything a frontend would otherwise reimplement lives here: feeding keys
//! to `davimci-keys`, turning an [`Outcome`] into a status-line message,
//! keeping the playhead on screen, and assembling the [`ViewState`]. A
//! frontend that skipped this would be a second editor.

use davimci_cmd::Session;
use davimci_core::{ClipId, Frame, Selection, TrackId};
use davimci_keys::engine::{Outcome, TransportCmd};
use davimci_keys::{Engine, Key, Keymap, MediaIntent, Mode, ZoomIntent};
use davimci_motion::{JumpConfig, Zoom};

use crate::cmdline::{CommandKey, CommandLine, CommandLineEvent};
use crate::error::AppError;
use crate::frontend::{Event, Frontend, Response, Surface};
use crate::job::{JobList, JobUpdate};
use crate::message::{Message, MessageQueue};
use crate::thumbnail::{Thumbnail, ThumbnailRequest, Thumbnails};
use crate::view::{CommandLineView, ViewInputs, ViewState};
use crate::viewport::Viewport;
use crate::waveform::{Waveform, Waveforms};

/// The host's hook for things the editor core deliberately does not own:
/// `:` commands (`davimci-cli` owns the vocabulary), transport (the backend
/// clock owns playback), and Lua callbacks (`davimci-lua` owns the runtime).
///
/// Defaulted so tests and the headless frontend can ignore all three.
pub trait Host {
    /// Run a submitted `:` line. Returning `Ok(None)` means "handled, nothing
    /// to say".
    ///
    /// `selection` is what the user had selected when the line was submitted
    /// (spec §6.1), or `None` in `NORMAL` - the visual selection lives in
    /// the key engine, and this is the seam that carries it to a host that
    /// has the vocabulary but not the modes. Commands that act on "the clip"
    /// fall back to the playhead when it is `None`.
    fn command(
        &mut self,
        line: &str,
        session: &mut Session,
        selection: Option<&Selection>,
    ) -> Result<Option<String>, AppError> {
        let _ = (session, selection);
        Err(AppError::UnhandledCommand(line.to_string()))
    }

    /// Dispatch a transport action to the backend clock. Never an edit, so it
    /// never reaches the undo log (spec §3.2.1).
    fn transport(&mut self, cmd: TransportCmd) {
        let _ = cmd;
    }

    /// Stop playback and commit the playhead where it reached (spec §3.2.1).
    ///
    /// Called before an action whose [`davimci_keys::TransportPolicy`] is
    /// `Interrupt` reports its effects, so the host has already let go of the
    /// clock by the time `playhead_moved` asks it to repaint. Idempotent: an
    /// interrupt with nothing playing does nothing.
    fn interrupt_transport(&mut self, session: &Session) {
        let _ = session;
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

    /// Import the media the user picked, at the position `intent` implies.
    ///
    /// The host has the prober and the filesystem; the app has neither. It
    /// still goes through `Session::exec` inside the host, so an import is
    /// one undoable command like every other edit.
    fn import_media(
        &mut self,
        path: &std::path::Path,
        intent: MediaIntent,
        session: &mut Session,
    ) -> Result<Option<String>, AppError> {
        let _ = (path, intent, session);
        Err(AppError::UnhandledCommand(
            "this build cannot import media".to_string(),
        ))
    }

    /// Job progress since the last call, for anything the host runs in the
    /// background (export, analysis). Polled every [`Event::Tick`].
    fn jobs(&mut self) -> Vec<JobUpdate> {
        Vec::new()
    }

    /// Audio analysed since the last call, per track (spec §6.1).
    ///
    /// Analysis runs in the background and finishes whenever it finishes, so
    /// it arrives the same way job progress does rather than as a return
    /// value of the edit that triggered it.
    fn waveforms(&mut self) -> Vec<(TrackId, Waveform)> {
        Vec::new()
    }

    /// Tracks whose audio changed, so any published envelope is now stale
    /// (spec §6.1: gain and fades invalidate the analysis).
    fn stale_waveforms(&mut self) -> Vec<TrackId> {
        Vec::new()
    }

    /// Clips on screen with no current thumbnail, nearest the playhead
    /// first.
    ///
    /// The app decides *what* is worth a picture - it owns the viewport -
    /// and the host decides *when* it can afford to decode one. A host that
    /// ignores this simply shows plain clips.
    fn request_thumbnails(&mut self, wanted: &[ThumbnailRequest]) {
        let _ = wanted;
    }

    /// Thumbnails decoded since the last call. Arrives like job progress,
    /// because decoding finishes whenever it finishes.
    fn thumbnails(&mut self) -> Vec<(ClipId, Thumbnail)> {
        Vec::new()
    }

    /// True once the host wants the loop to stop (`:q` succeeded).
    fn wants_quit(&self) -> bool {
        false
    }
}

/// One key token as the `:` line reads it. `Ctrl` chords and anything the
/// line has no meaning for are dropped rather than typed as text.
fn command_key_of(key: Key) -> Option<CommandKey> {
    use davimci_keys::Named;
    Some(match key {
        Key::Char(c) => CommandKey::Char(c),
        Key::Named(Named::Space) => CommandKey::Char(' '),
        Key::Named(Named::Enter) => CommandKey::Submit,
        Key::Named(Named::Esc) => CommandKey::Cancel,
        Key::Named(Named::Backspace) => CommandKey::Backspace,
        Key::Named(Named::Tab) => CommandKey::Tab,
        Key::Named(Named::Left) => CommandKey::Left,
        Key::Named(Named::Right) => CommandKey::Right,
        Key::Named(Named::Up) => CommandKey::Up,
        Key::Named(Named::Down) => CommandKey::Down,
        Key::Ctrl(_) => return None,
    })
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
    waveforms: Waveforms,
    thumbnails: Thumbnails,
    /// The `:` line's buffer, history and completion vocabulary. Owned here
    /// rather than in a frontend so every host shows the same line, with the
    /// same completions, as it is typed.
    command: CommandLine,
    command_open: bool,
    /// What an `i`/`a`/`r` picker, if one is open, will do with its answer.
    pending_pick: Option<MediaIntent>,
    /// The subtitle clip a frontend is editing the text of, if any.
    editing_text: Option<ClipId>,
    /// The selection at the moment `:` was pressed, for the `:` line that
    /// follows (spec §6.1).
    pending_selection: Option<Selection>,
    /// Set while a batch of events is being drained: the expensive host
    /// notifications are recorded here and issued once at the end.
    batching: bool,
    deferred_moved: bool,
    deferred_changed: bool,
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
            waveforms: Waveforms::default(),
            thumbnails: Thumbnails::default(),
            command: CommandLine::new(crate::cmdline::default_candidates()),
            command_open: false,
            pending_pick: None,
            editing_text: None,
            pending_selection: None,
            batching: false,
            deferred_moved: false,
            deferred_changed: false,
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
        self.pending_selection = None;
        self.viewport = Viewport::new(self.viewport.columns(), self.viewport.rows());
        self.engine.set_zoom(self.viewport.zoom());
        self.follow();
    }

    /// The live visual selection, if any (spec §6.1). `None` in `NORMAL`.
    #[must_use]
    pub fn selection(&self) -> Option<Selection> {
        self.engine.selection()
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

    /// Zoom lives here rather than in the key engine because the viewport is
    /// app state: `zi`/`zo`/`z0` (spec §11) come back as [`Outcome::Zoom`]
    /// and land here, as does a wheel or a menu, so the anchoring rule has
    /// exactly one implementation.
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
            command_line: self.command_open.then(|| self.command.view()),
            message: self.messages.current().cloned(),
            job: self.jobs.foreground().cloned(),
            recording: self.session.macros().recording(),
            waveforms: (!self.waveforms.is_empty()).then_some(&self.waveforms),
            thumbnails: (!self.thumbnails.is_empty()).then_some(&self.thumbnails),
        };
        ViewState::build(&self.session, self.viewport, &self.jump_cfg, &inputs)
    }

    /// Seek to a clicked column, and to a clicked lane when there is one.
    ///
    /// A click is navigation, so it takes the same path a motion does: the
    /// playhead moves, the host seeks and presents, and nothing reaches the
    /// undo log. Playback owns the clock, so it is interrupted first -
    /// otherwise the next tick would drag the playhead back.
    fn click(&mut self, column: u32, row: Option<usize>, host: &mut dyn Host) -> Response {
        host.interrupt_transport(&self.session);
        let duration = self.session.timeline().duration();
        if duration == Frame::ZERO {
            return Response::Continue;
        }
        // The viewport quantises to whole columns, so the frame under a click
        // is the first frame of that column, clamped to the timeline.
        let frame = self
            .viewport
            .frame_at_column(column)
            .min(Frame(duration.get().saturating_sub(1)));
        let track = row
            .map(|r| r.saturating_add(self.viewport.top_track()))
            .and_then(|i| self.session.timeline().tracks().get(i))
            .map_or_else(|| self.session.timeline().playhead().track, |t| t.id);
        match self.session.set_playhead(frame, track) {
            Ok(()) => {
                self.note_moved(host);
                self.follow();
            }
            Err(e) => self.messages.push(Message::error(e.to_string())),
        }
        Response::Continue
    }

    /// Feed one key. The single entry point for every frontend's input path.
    pub fn key(&mut self, key: Key, host: &mut dyn Host) -> Response {
        // While the `:` line is open it owns the keyboard, exactly as a
        // modal does. A frontend that routes modal keys itself sends
        // `Event::CommandKey`; a scripted or terminal frontend just sends
        // keys, and they must not reach the grammar.
        if self.command_open
            && let Some(k) = command_key_of(key)
        {
            return self.command_key(k, host);
        }
        // Entering COMMAND clears the visual selection in the key engine
        // (`:` is a mode change like any other), so what the user had
        // selected is remembered here, before the key is fed, and handed to
        // the host when the line is submitted.
        let before = self.engine.selection();
        let fed = self.engine.feed(key, &mut self.session);
        if self.engine.mode() == Mode::Command {
            self.pending_selection = before;
        }
        // Playback owns the playhead while it runs, so an interrupting bind
        // takes it back before anything is reported: otherwise the pacer
        // overwrites the motion on the next tick and the preview never moves
        // (spec §3.2.1).
        if fed.transport.interrupts() {
            host.interrupt_transport(&self.session);
        }
        self.apply_outcome(fed.outcome, host)
    }

    /// Handle a batch of frontend events, telling the host *once* about the
    /// stale graph and the moved playhead at the end.
    ///
    /// This is the entry point a frontend should use for one frame's worth
    /// of input. A held `h`/`l` delivers a burst of key repeats; seeking and
    /// decoding once per repeat is what made holding a key stall and then
    /// freeze, so the burst moves the playhead as many times as it says and
    /// costs one picture (spec §14).
    ///
    /// Returns one [`Response`] per event handled, in order, so a frontend
    /// can still open a picker or a `:` line.
    pub fn drain<I: IntoIterator<Item = Event>>(
        &mut self,
        events: I,
        host: &mut dyn Host,
    ) -> Vec<Response> {
        let was_batching = self.batching;
        self.batching = true;
        let mut out = Vec::new();
        for event in events {
            let response = self.event(event, host);
            let quit = response == Response::Quit;
            out.push(response);
            if quit {
                break;
            }
        }
        if !was_batching {
            self.flush_notifications(host);
        }
        out
    }

    /// Handle one frontend event.
    pub fn event(&mut self, event: Event, host: &mut dyn Host) -> Response {
        match event {
            Event::Key(k) => self.key(k, host),
            Event::Resize(s) => {
                self.resize(s);
                Response::Continue
            }
            Event::CommandKey(key) => self.command_key(key, host),
            Event::Command(line) => {
                self.close_command_line();
                // A `:` line may edit or swap the timeline, neither of which
                // is survivable mid-playback, and the ex vocabulary lives in
                // the host - so the clock is dropped unconditionally rather
                // than parsed for (spec §3.2.1).
                host.interrupt_transport(&self.session);
                // Read before the command runs: `:` mode has already left
                // visual mode behind, so the selection is the one the user
                // was looking at when they typed the line.
                // Falling back to the live selection covers a frontend that
                // submits a line without the `:` key ever being fed.
                let selection = self
                    .pending_selection
                    .take()
                    .or_else(|| self.engine.selection());
                match host.command(&line, &mut self.session, selection.as_ref()) {
                    Ok(Some(msg)) => self.messages.push(Message::info(msg)),
                    Ok(None) => {}
                    Err(e) => self.messages.push(Message::error(e.to_string())),
                }
                // A `:` line can edit (`:relink`) or swap the whole timeline
                // (`:e`, `:bn`), so the graph and the playhead are both
                // assumed stale rather than diffed.
                self.note_changed(host);
                self.note_moved(host);
                self.follow();
                if host.wants_quit() {
                    self.quit = true;
                    return Response::Quit;
                }
                Response::Continue
            }
            Event::CommandCancelled => {
                self.close_command_line();
                self.pending_selection = None;
                Response::Continue
            }
            Event::MediaChosen(path) => {
                let Some(intent) = self.pending_pick.take() else {
                    // No picker was open, so nothing asked for this file.
                    // Silently importing it would be a write the user never
                    // requested.
                    self.messages
                        .push(Message::error("no media picker is open".to_string()));
                    return Response::Continue;
                };
                // An import into an empty timeline is the one place the view
                // may move on its own: the default zoom would show a clip as
                // a couple of columns, which reads as "nothing happened".
                let was_empty = self.session.timeline().duration() == Frame::ZERO;
                match host.import_media(&path, intent, &mut self.session) {
                    Ok(msg) => {
                        if was_empty {
                            self.viewport.fit(self.session.timeline().duration());
                            self.engine.set_zoom(self.viewport.zoom());
                        }
                        // Importing is an edit: the graph is stale and the
                        // frame under the playhead may have changed.
                        self.note_changed(host);
                        self.note_moved(host);
                        if let Some(m) = msg {
                            self.messages.push(Message::info(m));
                        }
                    }
                    Err(e) => self.messages.push(Message::error(e.to_string())),
                }
                self.follow();
                Response::Continue
            }
            Event::PickerCancelled => {
                self.pending_pick = None;
                Response::Continue
            }
            Event::Click { column, row } => self.click(column, row, host),
            Event::TextEdited { clip, text } => {
                let Some(open) = self.editing_text.take() else {
                    // Nothing asked for this text, so committing it would be
                    // a write the user never requested.
                    self.messages
                        .push(Message::error("no subtitle is being edited".to_string()));
                    return Response::Continue;
                };
                if open != clip {
                    self.messages.push(Message::error(
                        "that text belongs to a different subtitle".to_string(),
                    ));
                    return Response::Continue;
                }
                let track = self
                    .session
                    .timeline()
                    .find_clip(clip)
                    .map(|(track, _)| track);
                let Some(track) = track else {
                    self.messages
                        .push(Message::error("that subtitle is gone".to_string()));
                    return Response::Continue;
                };
                // Editing text is an ordinary edit: one command, one undo
                // step (spec §15.4).
                match self.session.exec(&davimci_cmd::EditCommand::SetClipText {
                    track,
                    clip,
                    text,
                }) {
                    Ok(label) => {
                        self.messages.push(Message::info(label));
                        self.note_changed(host);
                        self.note_moved(host);
                    }
                    Err(e) => self.messages.push(Message::error(e.to_string())),
                }
                Response::Continue
            }
            Event::TextEditCancelled => {
                self.editing_text = None;
                Response::Continue
            }
            Event::Tick => {
                host.tick(&mut self.session);
                // Jobs report on the clock, not on the edit: an export runs
                // in the background and the status line has to keep up.
                for update in host.jobs() {
                    self.jobs.apply(update);
                }
                for track in host.stale_waveforms() {
                    self.waveforms.invalidate(track);
                }
                for (track, waveform) in host.waveforms() {
                    self.waveforms.insert(track, waveform);
                }
                for (clip, thumbnail) in host.thumbnails() {
                    self.thumbnails.insert(clip, thumbnail);
                }
                self.ask_for_thumbnails(host);
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
            // the grammar knows only that COMMAND was entered. The app owns
            // the line itself; the frontend is told to route keys to it.
            Outcome::Mode(change) => {
                if change.to == Mode::Command {
                    self.open_command_line();
                    self.follow();
                    return Response::OpenCommandLine;
                }
                if change.from == Mode::Command {
                    self.close_command_line();
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
            Outcome::Zoom(intent) => match intent {
                ZoomIntent::In => self.zoom_in(),
                ZoomIntent::Out => self.zoom_out(),
                ZoomIntent::Reset => self.set_zoom(Zoom::default()),
            },
            Outcome::PickMedia(intent) => {
                self.pending_pick = Some(intent);
                self.follow();
                return Response::OpenPicker(intent);
            }
            Outcome::EditText { clip, text } => {
                self.editing_text = Some(clip);
                self.follow();
                return Response::EditText { clip, text };
            }
            Outcome::EnterCommandMode => {
                self.open_command_line();
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
            self.note_changed(host);
        }
        if moved {
            self.note_moved(host);
        }
        self.follow();
        Response::Continue
    }

    /// The vocabulary Tab completes against. The app does not own the ex
    /// vocabulary (`davimci-cli` does), so the host supplies it once at
    /// startup and it is shown to every frontend identically.
    pub fn set_command_candidates(&mut self, candidates: Vec<String>) {
        self.command.set_candidates(candidates);
    }

    /// The `:` line as it currently stands, if one is open.
    #[must_use]
    pub fn command_line(&self) -> Option<CommandLineView> {
        self.command_open.then(|| self.command.view())
    }

    fn open_command_line(&mut self) {
        self.command_open = true;
        self.command.open();
    }

    fn close_command_line(&mut self) {
        self.command_open = false;
        self.command.close();
    }

    /// One keystroke into the open `:` line. A frontend names the key; the
    /// app decides what it does and, on Enter, runs the line.
    fn command_key(&mut self, key: CommandKey, host: &mut dyn Host) -> Response {
        if !self.command_open {
            // A stray command key with no line open would otherwise edit an
            // invisible buffer and submit it later.
            return Response::Continue;
        }
        match self.command.key(key) {
            CommandLineEvent::Editing => Response::Continue,
            CommandLineEvent::Submit(line) => self.event(Event::Command(line), host),
            CommandLineEvent::Cancel => self.event(Event::CommandCancelled, host),
        }
    }

    /// Ask the host for pictures of the visible clips that have none.
    ///
    /// Only video lanes, only what is on screen, and only what is missing or
    /// stale: a thumbnail costs a decode, so asking for one the user cannot
    /// see is a frame the preview does not get. Requests are ordered by
    /// distance from the playhead, so a host that decodes one per tick fills
    /// the screen outwards from where the user is looking.
    fn ask_for_thumbnails(&mut self, host: &mut dyn Host) {
        let tl = self.session.timeline();
        let (from, to) = self.viewport.visible_range();
        let playhead = tl.playhead().frame;
        let mut visible: Vec<ClipId> = Vec::new();
        let mut wanted: Vec<(u64, ThumbnailRequest)> = Vec::new();
        for track in tl
            .tracks()
            .iter()
            .skip(self.viewport.top_track())
            .take(self.viewport.rows())
            .filter(|t| t.kind == davimci_core::TrackKind::Video)
        {
            for clip in track
                .clips()
                .iter()
                .filter(|c| c.end() > from && c.start < to)
            {
                visible.push(clip.id);
                if self.thumbnails.get(clip.id, clip.source_in).is_some() {
                    continue;
                }
                // Inside the clip, and inside the viewport: a clip whose
                // head is scrolled off is pictured where it is visible.
                let at = clip.start.max(from).min(Frame(clip.end().get() - 1));
                let distance = at.get().abs_diff(playhead.get());
                wanted.push((
                    distance,
                    ThumbnailRequest {
                        clip: clip.id,
                        at,
                        source_in: clip.source_in,
                    },
                ));
            }
        }
        // Pixels for clips that are gone are pixels nobody will ever draw.
        self.thumbnails.retain(&visible);
        if wanted.is_empty() {
            return;
        }
        wanted.sort_by_key(|(d, _)| *d);
        let requests: Vec<ThumbnailRequest> = wanted.into_iter().map(|(_, r)| r).collect();
        host.request_thumbnails(&requests);
    }

    /// "The graph is stale" - issued now, or once at the end of the batch.
    fn note_changed(&mut self, host: &mut dyn Host) {
        if self.batching {
            self.deferred_changed = true;
        } else {
            host.timeline_changed(&self.session);
        }
    }

    /// "The playhead moved" - issued now, or once at the end of the batch.
    fn note_moved(&mut self, host: &mut dyn Host) {
        if self.batching {
            self.deferred_moved = true;
        } else {
            host.playhead_moved(&self.session);
        }
    }

    /// End a batch: tell the host once about everything it missed. Order is
    /// the same as the unbatched path - the graph is rebuilt before a frame
    /// is pulled from it.
    fn flush_notifications(&mut self, host: &mut dyn Host) {
        self.batching = false;
        if std::mem::take(&mut self.deferred_changed) {
            host.timeline_changed(&self.session);
        }
        if std::mem::take(&mut self.deferred_moved) {
            host.playhead_moved(&self.session);
        }
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
            // One batch, one seek. A held `h`/`l` delivers a burst of
            // repeats in a single poll; reprojecting and decoding once per
            // repeat is what made holding a key stall and then freeze, so
            // the batch moves the playhead as many times as it was told and
            // the host is asked for a picture once, at the end (spec §14).
            let events = frontend.poll();
            if self.drain(events, host).contains(&Response::Quit) {
                return Ok(());
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
