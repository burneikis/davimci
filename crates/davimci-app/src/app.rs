//! The frontend-agnostic event loop.
//!
//! Everything a frontend would otherwise reimplement lives here: feeding keys
//! to `davimci-keys`, turning an [`Outcome`] into a status-line message,
//! keeping the playhead on screen, and assembling the [`ViewState`]. A
//! frontend that skipped this would be a second editor.

use davimci_cmd::Session;
use davimci_core::{ClipId, Frame, Selection, TrackId};
use davimci_keys::engine::{Outcome, TransportCmd};
use davimci_keys::{CenterIntent, Engine, Key, Keymap, MediaIntent, Mode, ZoomIntent};
use davimci_motion::{JumpConfig, TimeRange, Zoom};

use crate::cmdline::{CommandKey, CommandLine, CommandLineEvent};
use crate::confirm::{Confirm, ConfirmId, answer_of};
use crate::error::AppError;
use crate::frontend::{Event, Frontend, Response, Surface};
use crate::job::{JobList, JobUpdate};
use crate::message::{Message, MessageQueue};
use crate::modal::ModalKey;
use crate::panel::{PanelId, PanelOp, PanelStore};
use crate::plugin::PluginEffects;
use crate::style::TimelineStyle;
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
    ///, or `None` in `NORMAL` - the visual selection lives in
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
    /// never reaches the undo log.
    ///
    /// `selection` carries what is selected, because `<Space>l` loops it and
    /// the visual selection lives in the key engine, not in the host.
    fn transport(&mut self, cmd: TransportCmd, selection: Option<&Selection>) {
        let _ = (cmd, selection);
    }

    /// Questions the host wants asked in the frontend, taken once each.
    ///
    /// A host that has to ask before it may run something - project-local
    /// config above all - raises the question here instead of on the
    /// terminal, so the answer comes from whichever frontend the user is
    /// actually looking at.
    fn take_confirms(&mut self) -> Vec<Confirm> {
        Vec::new()
    }

    /// The answer to a question the host raised. Called once per question,
    /// with the id it was raised under.
    fn confirmed(&mut self, id: ConfirmId, granted: bool, session: &mut Session) {
        let _ = (id, granted, session);
    }

    /// A keymap the host rebuilt, taken once.
    ///
    /// Config can be loaded after startup - a project-local file the user
    /// has just trusted - and the bindings it declares are only real once
    /// they are in the table the grammar consults.
    fn take_keymap(&mut self) -> Option<Keymap> {
        None
    }

    /// A `:set` the host parsed but the key engine owns, taken once.
    ///
    /// `visualstart` shapes selections, which live in the engine, while `:set`
    /// is the host's vocabulary; this is the seam between the two, so no
    /// frontend has to know the setting exists.
    fn take_visual_start(&mut self) -> Option<davimci_keys::VisualStart> {
        None
    }

    /// `:set centerfollow`, taken once.
    ///
    /// Scrolling belongs to the viewport, which no host owns, so the host
    /// parks what it parsed and the app is what applies it - the same state
    /// `zZ` toggles.
    fn take_center_follow(&mut self) -> Option<bool> {
        None
    }

    /// How the timeline is to draw cuts and gaps, taken once.
    fn take_timeline_style(&mut self) -> Option<TimelineStyle> {
        None
    }

    /// Whether `i` on a text track edits a subtitle, taken once.
    ///
    /// The plugin that owns text tracks is what switches this on, and the
    /// host is the only layer that knows which plugins are running; the
    /// grammar just needs the answer.
    fn take_text_editing(&mut self) -> Option<bool> {
        None
    }

    /// The visual selection changed or was cleared. Only the loop
    /// cares: a loop that follows a selection ends when the selection does.
    fn selection_changed(&mut self, selection: Option<&Selection>) {
        let _ = selection;
    }

    /// Resolve a config-registered text object against the clip
    /// under the playhead. Only the host has the Lua runtime, so the grammar
    /// hands the name here and applies the verb to whatever range comes
    /// back. `Ok(None)` means the object matched nothing.
    fn resolve_object(
        &mut self,
        name: char,
        around: bool,
        session: &Session,
    ) -> Result<Option<TimeRange>, AppError> {
        let _ = (name, around, session);
        Err(AppError::UnhandledCommand(format!(
            "no text object '{name}' is registered"
        )))
    }

    /// Stop playback and commit the playhead where it reached.
    ///
    /// Called before an action whose [`davimci_keys::TransportPolicy`] is
    /// `Interrupt` reports its effects, so the host has already let go of the
    /// clock by the time `playhead_moved` asks it to repaint. Idempotent: an
    /// interrupt with nothing playing does nothing.
    fn interrupt_transport(&mut self, session: &Session) {
        let _ = session;
    }

    /// The half-typed key sequence changed, so anything that draws it - a
    /// which-key panel above all - can be rebuilt.
    ///
    /// The app owns the grammar's state, so this is the only place a host
    /// can learn of it, and a plugin is a *view* of it rather than a second
    /// copy of the keymap.
    fn key_pending(&mut self, pending: &davimci_keys::Pending, session: &mut Session) {
        let _ = (pending, session);
    }

    /// One keystroke into a focused plugin panel.
    ///
    /// The panel already owns the keyboard by the time this is called; the
    /// host only has to hand the key to the plugin and report what came
    /// back.
    fn panel_key(&mut self, panel: PanelId, key: ModalKey, session: &mut Session) -> PluginEffects {
        let _ = (panel, key, session);
        PluginEffects::default()
    }

    /// Invoke Lua callback `id` and report what it asked for.
    ///
    /// The host runs the requests only it can answer - an export, an import,
    /// a registered motion - and hands back the ones that are edits. The app
    /// runs those through the key engine, so a plugin edit takes the same
    /// write path a keystroke does and there is no second one.
    fn plugin(&mut self, id: u32, session: &mut Session) -> Result<PluginEffects, AppError> {
        let _ = (id, session);
        Ok(PluginEffects::default())
    }

    /// Requests Lua queued outside a keymap callback - from an event handler,
    /// or from a callback that queued more after it returned. Drained on
    /// every [`Event::Tick`] so a plugin edit is never left sitting.
    fn plugin_tick(&mut self, session: &mut Session) -> PluginEffects {
        let _ = session;
        PluginEffects::default()
    }

    /// The vocabulary Tab completes against, asked for each time the `:`
    /// line opens. Only the host knows what the settings currently hold, and
    /// a completion that shows a stale value is worse than none; `None`
    /// keeps whatever vocabulary was supplied at startup.
    fn command_vocabulary(&mut self, session: &Session) -> Option<crate::CommandVocabulary> {
        let _ = session;
        None
    }

    /// The mode changed, for the `ModeChanged` event. The app owns
    /// the mode FSM, so this is the only place a host can learn of it.
    fn mode_changed(&mut self, from: Mode, to: Mode) {
        let _ = (from, to);
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

    /// Audio analysed since the last call, per track.
    ///
    /// Analysis runs in the background and finishes whenever it finishes, so
    /// it arrives the same way job progress does rather than as a return
    /// value of the edit that triggered it.
    fn waveforms(&mut self) -> Vec<(TrackId, Waveform)> {
        Vec::new()
    }

    /// Tracks whose audio changed, so any published envelope is now stale
    ///.
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

/// One key as a modal reads it. `Ctrl` chords and anything a panel has no
/// alphabet for are dropped rather than delivered as text.
fn modal_key_of(key: Key) -> Option<ModalKey> {
    use davimci_keys::Named;
    Some(match key {
        Key::Char(c) => ModalKey::Char(c),
        Key::Named(Named::Space) => ModalKey::Char(' '),
        Key::Named(Named::Enter) => ModalKey::Enter,
        Key::Named(Named::Esc) => ModalKey::Escape,
        Key::Named(Named::Backspace) => ModalKey::Backspace,
        Key::Named(Named::Tab) => ModalKey::Tab,
        Key::Named(Named::Left) => ModalKey::Left,
        Key::Named(Named::Right) => ModalKey::Right,
        Key::Named(Named::Up) => ModalKey::Up,
        Key::Named(Named::Down) => ModalKey::Down,
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
    /// How wide the frontend draws one thumbnail, in columns; zero means it
    /// draws none.
    thumbnail_columns: u32,
    /// How many character cells wide and tall the frontend's panel area is -
    /// the units plugin panels are placed in.
    cell_columns: u32,
    cell_rows: u32,
    /// The `:` line's buffer, history and completion vocabulary. Owned here
    /// rather than in a frontend so every host shows the same line, with the
    /// same completions, as it is typed.
    command: CommandLine,
    command_open: bool,
    /// What an `i`/`a`/`r` picker, if one is open, will do with its answer.
    pending_pick: Option<MediaIntent>,
    /// `zZ`: keep the playhead in the middle column instead of scrolling only
    /// when it reaches an edge.
    center_follow: bool,
    /// How cuts and gaps are drawn. View state: it changes nothing about the
    /// timeline, so it never reaches the undo log.
    timeline_style: TimelineStyle,
    /// The subtitle clip a frontend is editing the text of, if any.
    editing_text: Option<ClipId>,
    /// The selection at the moment `:` was pressed, for the `:` line that
    /// follows.
    pending_selection: Option<Selection>,
    /// Panels plugins have open. View state, not project state: nothing here
    /// reaches the timeline or the undo log.
    panels: PanelStore,
    /// Questions waiting for an answer, oldest first. The front one owns the
    /// keyboard: a question about running someone else's code must not be
    /// answerable by a keystroke aimed at the timeline behind it.
    confirms: std::collections::VecDeque<Confirm>,
    /// The pending sequence last reported to the host, so `key_pending` fires
    /// on a change rather than on every keystroke.
    last_pending: Option<davimci_keys::Pending>,
    /// Set while a batch of events is being drained: the expensive host
    /// notifications are recorded here and issued once at the end.
    batching: bool,
    deferred_moved: bool,
    deferred_changed: bool,
    /// Whether the view has already been fitted to content. The first clip in
    /// an empty timeline moves the view once; every later edit leaves it
    /// where the user put it.
    fitted: bool,
    quit: bool,
}

impl App {
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self::with_keymap(session, Keymap::new())
    }

    #[must_use]
    pub fn with_keymap(session: Session, keymap: Keymap) -> Self {
        let fitted = session.timeline().duration() != Frame::ZERO;
        Self {
            fitted,
            session,
            engine: Engine::with_keymap(keymap),
            viewport: Viewport::default(),
            jump_cfg: JumpConfig::default(),
            messages: MessageQueue::default(),
            jobs: JobList::default(),
            waveforms: Waveforms::default(),
            thumbnails: Thumbnails::default(),
            thumbnail_columns: 0,
            cell_columns: 0,
            cell_rows: 0,
            command: CommandLine::new(crate::cmdline::default_vocabulary()),
            command_open: false,
            pending_pick: None,
            center_follow: false,
            timeline_style: TimelineStyle::default(),
            editing_text: None,
            pending_selection: None,
            panels: PanelStore::default(),
            confirms: std::collections::VecDeque::new(),
            last_pending: None,
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
        self.fitted = self.session.timeline().duration() != Frame::ZERO;
        self.engine.set_zoom(self.viewport.zoom());
        self.follow();
    }

    /// The live visual selection, if any. `None` in `NORMAL`.
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
        self.thumbnail_columns = surface.thumbnail_columns;
        self.cell_columns = surface.cell_columns;
        self.cell_rows = surface.cell_rows;
        self.viewport.resize(surface.columns, surface.rows);
        self.follow();
    }

    /// Zoom lives here rather than in the key engine because the viewport is
    /// app state: `zi`/`zo`/`z0` come back as [`Outcome::Zoom`]
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

    /// `:set visualstart`: what `v` covers at each end.
    pub fn set_visual_start(&mut self, start: davimci_keys::VisualStart) {
        self.engine.set_visual_start(start);
    }

    /// Let `i` on a text track open the subtitle under the playhead.
    pub fn set_text_editing(&mut self, on: bool) {
        self.engine.set_text_editing(on);
    }

    #[must_use]
    pub fn text_editing(&self) -> bool {
        self.engine.text_editing()
    }

    #[must_use]
    pub fn visual_start(&self) -> davimci_keys::VisualStart {
        self.engine.visual_start()
    }

    /// Scroll so the playhead sits in the middle column. The playhead itself
    /// does not move, so this is view state only and never an edit.
    pub fn center_playhead(&mut self) {
        let ph = self.session.timeline().playhead().frame;
        self.viewport
            .center_on(ph, self.session.timeline().duration());
    }

    /// Whether the view re-centres on the playhead after every move, rather
    /// than scrolling only at the edges.
    #[must_use]
    pub fn center_follow(&self) -> bool {
        self.center_follow
    }

    /// Turn permanent centring on or off. Turning it on centres immediately,
    /// so the setting and the view never disagree.
    pub fn set_center_follow(&mut self, on: bool) {
        self.center_follow = on;
        if on {
            self.center_playhead();
        }
    }

    /// How the timeline draws cuts and gaps.
    #[must_use]
    pub fn timeline_style(&self) -> TimelineStyle {
        self.timeline_style
    }

    pub fn set_timeline_style(&mut self, style: TimelineStyle) {
        self.timeline_style = style;
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
            thumbnail_columns: self.thumbnail_columns,
            cell_columns: self.cell_columns,
            cell_rows: self.cell_rows,
            panels: (!self.panels.is_empty()).then_some(&self.panels),
            confirm: self.confirms.front(),
            style: self.timeline_style,
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
            Err(e) => self.fail(e.to_string()),
        }
        Response::Continue
    }

    /// Raise a question. Shown until it is answered; the timeline behind it
    /// keeps its keyboard only once nothing is pending.
    pub fn ask(&mut self, confirm: Confirm) {
        self.confirms.push_back(confirm);
    }

    /// The question currently on screen, if any.
    #[must_use]
    pub fn pending_confirm(&self) -> Option<&Confirm> {
        self.confirms.front()
    }

    /// Answer the question on screen. An answer for a question that is no
    /// longer the pending one is dropped: it was aimed at something else.
    pub fn answer_confirm(&mut self, id: ConfirmId, granted: bool, host: &mut dyn Host) {
        if self.confirms.front().map(|c| c.id) != Some(id) {
            return;
        }
        self.confirms.pop_front();
        host.confirmed(id, granted, &mut self.session);
        self.adopt_host_settings(host);
    }

    /// Install what the host rebuilt after loading more config: bindings,
    /// and the capabilities a plugin turning on has just granted.
    fn adopt_host_settings(&mut self, host: &mut dyn Host) {
        if let Some(keymap) = host.take_keymap() {
            self.engine.set_keymap(keymap);
        }
        if let Some(on) = host.take_text_editing() {
            self.engine.set_text_editing(on);
        }
    }

    /// Feed one key. The single entry point for every frontend's input path.
    pub fn key(&mut self, key: Key, host: &mut dyn Host) -> Response {
        self.adopt_host_settings(host);
        // A pending question owns the keyboard before anything else: it is
        // asked because nothing may proceed until it is answered.
        if let Some(id) = self.confirms.front().map(|c| c.id) {
            if let Some(granted) = modal_key_of(key).and_then(answer_of) {
                self.answer_confirm(id, granted, host);
            }
            return Response::Continue;
        }
        // While the `:` line is open it owns the keyboard, exactly as a
        // modal does. A frontend that routes modal keys itself sends
        // `Event::CommandKey`; a scripted or terminal frontend just sends
        // keys, and they must not reach the grammar.
        if self.command_open
            && let Some(k) = command_key_of(key)
        {
            return self.command_key(k, host);
        }
        // A focused panel owns the keyboard next, for a frontend that sends
        // raw keys rather than routing modals itself. An unfocused panel -
        // which-key and every other reporting panel - is skipped here, which
        // is what keeps it from ever eating a keystroke.
        if let Some(panel) = self.panels.focused().map(|p| p.id)
            && let Some(k) = modal_key_of(key)
        {
            return self.panel_key(panel, k, host);
        }
        // Entering COMMAND clears the visual selection in the key engine
        // (`:` is a mode change like any other), so what the user had
        // selected is remembered here, before the key is fed, and handed to
        // the host when the line is submitted.
        let before = self.engine.selection();
        let fed = self.engine.feed(key, &mut self.session);
        let after = self.engine.selection();
        if after != before {
            host.selection_changed(after.as_ref());
        }
        if self.engine.mode() == Mode::Command {
            self.pending_selection = before;
        }
        // Playback owns the playhead while it runs, so an interrupting bind
        // takes it back before anything is reported: otherwise the pacer
        // overwrites the motion on the next tick and the preview never moves
        //.
        if fed.transport.interrupts() {
            host.interrupt_transport(&self.session);
        }
        let response = self.apply_outcome(fed.outcome, host);
        self.note_pending(host);
        response
    }

    /// Handle a batch of frontend events, telling the host *once* about the
    /// stale graph and the moved playhead at the end.
    ///
    /// This is the entry point a frontend should use for one frame's worth
    /// of input. A held `h`/`l` delivers a burst of key repeats; seeking and
    /// decoding once per repeat is what made holding a key stall and then
    /// freeze, so the burst moves the playhead as many times as it says and
    /// costs one picture.
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
            Event::Command(line) => self.run_command_line(&line, host),
            Event::CommandCancelled => {
                self.close_command_line();
                self.pending_selection = None;
                Response::Continue
            }
            Event::ConfirmAnswered { id, granted } => {
                self.answer_confirm(id, granted, host);
                Response::Continue
            }
            Event::MediaChosen(path) => self.import_chosen_media(&path, host),
            Event::PickerCancelled => {
                self.pending_pick = None;
                Response::Continue
            }
            Event::Click { column, row } => self.click(column, row, host),
            Event::PanelKey { panel, key } => self.panel_key(panel, key, host),
            Event::TextEdited { clip, text } => self.commit_clip_text(clip, text, host),
            Event::TextEditCancelled => {
                self.editing_text = None;
                Response::Continue
            }
            Event::Tick => self.tick(host),
            Event::Quit => {
                self.quit = true;
                Response::Quit
            }
        }
    }

    /// Run a submitted `:` line against the host's ex vocabulary.
    fn run_command_line(&mut self, line: &str, host: &mut dyn Host) -> Response {
        self.close_command_line();
        // A `:` line may edit or swap the timeline, neither of which
        // is survivable mid-playback, and the ex vocabulary lives in
        // the host - so the clock is dropped unconditionally rather
        // than parsed for.
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
        match host.command(line, &mut self.session, selection.as_ref()) {
            Ok(Some(msg)) => self.say(msg),
            Ok(None) => {}
            Err(e) => self.fail(e.to_string()),
        }
        self.drain_host_settings(host);
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

    /// Import the file a media picker returned, for the intent that opened it.
    fn import_chosen_media(&mut self, path: &std::path::Path, host: &mut dyn Host) -> Response {
        let Some(intent) = self.pending_pick.take() else {
            // No picker was open, so nothing asked for this file. Silently
            // importing it would be a write the user never requested.
            self.fail("no media picker is open".to_string());
            return Response::Continue;
        };
        match host.import_media(path, intent, &mut self.session) {
            Ok(msg) => {
                // Importing is an edit: the graph is stale and the frame
                // under the playhead may have changed.
                self.note_changed(host);
                self.note_moved(host);
                if let Some(m) = msg {
                    self.say(m);
                }
            }
            Err(e) => self.fail(e.to_string()),
        }
        self.follow();
        Response::Continue
    }

    /// Commit subtitle text the frontend collected for the clip it was opened
    /// on.
    fn commit_clip_text(
        &mut self,
        clip: davimci_core::ClipId,
        text: String,
        host: &mut dyn Host,
    ) -> Response {
        let Some(open) = self.editing_text.take() else {
            // Nothing asked for this text, so committing it would be a write
            // the user never requested.
            self.fail("no subtitle is being edited".to_string());
            return Response::Continue;
        };
        if open != clip {
            self.fail("that text belongs to a different subtitle".to_string());
            return Response::Continue;
        }
        let track = self
            .session
            .timeline()
            .find_clip(clip)
            .map(|(track, _)| track);
        let Some(track) = track else {
            self.fail("that subtitle is gone".to_string());
            return Response::Continue;
        };
        // Editing text is an ordinary edit: one command, one undo step.
        match self
            .session
            .exec(&davimci_cmd::EditCommand::SetClipText { track, clip, text })
        {
            Ok(label) => {
                self.say(label);
                self.note_changed(host);
                self.note_moved(host);
            }
            Err(e) => self.fail(e.to_string()),
        }
        Response::Continue
    }

    /// One turn of the clock: plugin work, then whatever the host finished
    /// off the edit path.
    fn tick(&mut self, host: &mut dyn Host) -> Response {
        host.tick(&mut self.session);
        // Anything Lua queued since the last tick - an event handler that
        // asked for an edit, most often. Run before the view is assembled so
        // the edit and its status line land together.
        let effects = host.plugin_tick(&mut self.session);
        self.apply_plugin(effects, host);
        self.collect_background_work(host);
        self.ask_for_thumbnails(host);
        self.follow();
        Response::Continue
    }

    /// Results that arrive on the clock rather than on an edit: an export runs
    /// in the background and the status line has to keep up.
    fn collect_background_work(&mut self, host: &mut dyn Host) {
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
        for confirm in host.take_confirms() {
            self.confirms.push_back(confirm);
        }
    }

    fn apply_outcome(&mut self, outcome: Outcome, host: &mut dyn Host) -> Response {
        // An edit invalidates the render graph; an edit or a motion moves the
        // playhead. Both are reported once, here, so a host cannot miss one
        // by handling only some outcomes.
        let edited = matches!(outcome, Outcome::Applied(_));
        let moved = edited || matches!(outcome, Outcome::Moved);
        match outcome {
            Outcome::Pending | Outcome::Cancelled | Outcome::Moved => {}
            // `:` is a mode change in `davimci-keys`, not a distinct outcome:
            // the grammar knows only that COMMAND was entered. The app owns
            // the line itself; the frontend is told to route keys to it.
            Outcome::Mode(change) => {
                if change.to == Mode::Command {
                    self.open_command_line(host);
                    self.follow();
                    return Response::OpenCommandLine;
                }
                if change.from == Mode::Command {
                    self.close_command_line();
                }
                host.mode_changed(change.from, change.to);
            }
            Outcome::Invalid => {
                self.warn("That key sequence is not bound to anything.".to_string());
            }
            Outcome::Applied(label) => self.say(label),
            Outcome::PredicatePending => self
                .warn("Analysis is still running; that motion cannot be resolved yet.".to_string()),
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
            Outcome::Transport(cmd) => {
                let selection = self.engine.selection();
                host.transport(cmd, selection.as_ref());
            }
            Outcome::Zoom(intent) => match intent {
                ZoomIntent::In => self.zoom_in(),
                ZoomIntent::Out => self.zoom_out(),
                ZoomIntent::Reset => self.set_zoom(Zoom::default()),
            },
            Outcome::Center(intent) => match intent {
                CenterIntent::Once => self.center_playhead(),
                CenterIntent::Toggle => {
                    let on = !self.center_follow;
                    self.set_center_follow(on);
                    self.say(if on {
                        "playhead centred".to_string()
                    } else {
                        "playhead centring off".to_string()
                    });
                }
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
                self.open_command_line(host);
                self.follow();
                return Response::OpenCommandLine;
            }
            Outcome::Plugin(id) => match host.plugin(id, &mut self.session) {
                Ok(effects) => self.apply_plugin(effects, host),
                Err(e) => self.fail(e.to_string()),
            },
            // A registered object is resolved by the host and the verb then
            // runs through the engine, so a plugin object edits by the same
            // write path a built-in one does.
            Outcome::ResolveObject { name, around, verb } => {
                match host.resolve_object(name, around, &self.session) {
                    Ok(Some(range)) => {
                        let outcome = self
                            .engine
                            .execute_action(verb.with_range(range), &mut self.session);
                        return self.apply_outcome(outcome, host);
                    }
                    Ok(None) => {
                        self.warn(format!("The text object '{name}' matched nothing here."));
                    }
                    Err(e) => self.fail(e.to_string()),
                }
            }
            Outcome::NotImplemented(what) => self
                .messages
                .push(Message::warning(format!("Not implemented yet: {what}."))),
            Outcome::Error(msg) => self.fail(msg),
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

    /// Run what a plugin asked for. Each action goes through the key engine,
    /// so it is validated, undoable and `.`-repeatable exactly as the same
    /// action typed by hand would be.
    fn apply_plugin(&mut self, effects: PluginEffects, host: &mut dyn Host) {
        for message in effects.messages {
            self.messages.push(message);
        }
        for action in effects.actions {
            let outcome = self.engine.execute_action(action, &mut self.session);
            self.apply_outcome(outcome, host);
        }
        for op in effects.panels {
            if let Err(e) = self.panels.apply(op) {
                self.fail(e);
            }
        }
        // A plugin `editor.set` is routed as the `:` line it stands for, so it
        // parks the same state a typed `:set` does and has to be collected on
        // this path too - config would otherwise be silently ignored.
        self.drain_host_settings(host);
    }

    /// Collect the `:set` state the host parsed but the app owns.
    ///
    /// Both the `:` line and a plugin request reach the same registry, so both
    /// have to drain it or a setting applies on one path only.
    fn drain_host_settings(&mut self, host: &mut dyn Host) {
        if let Some(start) = host.take_visual_start() {
            self.engine.set_visual_start(start);
        }
        if let Some(on) = host.take_center_follow() {
            self.set_center_follow(on);
        }
        if let Some(style) = host.take_timeline_style() {
            self.set_timeline_style(style);
        }
    }

    /// The panels currently open, for a host that wants to inspect them.
    #[must_use]
    pub fn panels(&self) -> &PanelStore {
        &self.panels
    }

    /// Apply one panel operation directly, for a host with no plugin runtime
    /// - the headless harness and the tests.
    pub fn apply_panel(&mut self, op: PanelOp) {
        if let Err(e) = self.panels.apply(op) {
            self.fail(e);
        }
    }

    /// Hand one key to the focused panel.
    ///
    /// `Esc` always closes the panel *and* is still delivered, so a plugin
    /// that stops answering can never hold the keyboard.
    fn panel_key(&mut self, panel: PanelId, key: ModalKey, host: &mut dyn Host) -> Response {
        if self.panels.get(panel).is_none() {
            return Response::Continue;
        }
        let effects = host.panel_key(panel, key, &mut self.session);
        if key == ModalKey::Escape {
            let _ = self.panels.apply(PanelOp::Close(panel));
        }
        self.apply_plugin(effects, host);
        Response::Continue
    }

    /// Tell the host what the grammar is now waiting for, when that changed.
    fn note_pending(&mut self, host: &mut dyn Host) {
        let pending = self.engine.pending();
        if self.last_pending.as_ref() == Some(&pending) {
            return;
        }
        host.key_pending(&pending, &mut self.session);
        self.last_pending = Some(pending);
    }

    /// The vocabulary Tab completes against. The app does not own the ex
    /// vocabulary (`davimci-cli` does), so the host supplies it once at
    /// startup and it is shown to every frontend identically.
    pub fn set_command_vocabulary(&mut self, vocabulary: crate::CommandVocabulary) {
        self.command.set_vocabulary(vocabulary);
    }

    /// The `:` line as it currently stands, if one is open.
    #[must_use]
    pub fn command_line(&self) -> Option<CommandLineView> {
        self.command_open.then(|| self.command.view())
    }

    fn open_command_line(&mut self, host: &mut dyn Host) {
        if let Some(vocabulary) = host.command_vocabulary(&self.session) {
            self.command.set_vocabulary(vocabulary);
        }
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
        if self.thumbnail_columns == 0 {
            return;
        }
        let tl = self.session.timeline();
        let (from, to) = self.viewport.visible_range();
        let playhead = tl.playhead().frame;
        let mut keep: Vec<(ClipId, Frame)> = Vec::new();
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
                // The same sample points the view draws, so nothing is
                // decoded that would not be shown and nothing shown is
                // missing because it was never asked for.
                for (column, source) in
                    crate::view::strip_samples(&self.viewport, clip, self.thumbnail_columns)
                {
                    keep.push((clip.id, source));
                    if self.thumbnails.get(clip.id, source).is_some() {
                        continue;
                    }
                    let at = self.viewport.frame_at_column(column);
                    let at = at.max(clip.start).min(Frame(clip.end().get() - 1));
                    wanted.push((
                        at.get().abs_diff(playhead.get()),
                        ThumbnailRequest {
                            clip: clip.id,
                            at,
                            source,
                        },
                    ));
                }
            }
        }
        // Pixels nobody would draw are pixels nobody should be holding.
        self.thumbnails.retain(&keep);
        if wanted.is_empty() {
            return;
        }
        // Outwards from the playhead: a host that decodes one per tick fills
        // in where the user is looking first.
        wanted.sort_by_key(|(d, _)| *d);
        let requests: Vec<ThumbnailRequest> = wanted.into_iter().map(|(_, r)| r).collect();
        host.request_thumbnails(&requests);
    }

    /// Status-line shorthands. Every message the app raises goes through one
    /// of these, so a frontend cannot be handed a bare string.
    fn say(&mut self, text: String) {
        self.messages.push(Message::info(text));
    }

    fn warn(&mut self, text: String) {
        self.messages.push(Message::warning(text));
    }

    fn fail(&mut self, text: String) {
        self.messages.push(Message::error(text));
    }

    /// "The graph is stale" - issued now, or once at the end of the batch.
    fn note_changed(&mut self, host: &mut dyn Host) {
        self.fit_to_first_content();
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

    /// The first content in an empty timeline is the one place the view moves
    /// on its own: the default zoom draws a clip as a couple of columns, which
    /// reads as "nothing happened".
    ///
    /// The fitted level is also never left below the level where subdivisions
    /// begin, or a long import would offer no jump point between its two
    /// ends. Overshooting the width by one level is preferred to a track the
    /// user cannot navigate inside.
    fn fit_to_first_content(&mut self) {
        if self.fitted || self.session.timeline().duration() == Frame::ZERO {
            return;
        }
        self.fitted = true;
        let floor = Zoom::new(self.jump_cfg.subdivide_from);
        self.viewport
            .fit_at_least(self.session.timeline().duration(), floor);
        self.engine.set_zoom(self.viewport.zoom());
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
        if self.center_follow {
            self.viewport.center_on(ph.frame, duration);
        } else {
            self.viewport.follow_playhead(ph.frame, duration);
        }
        self.viewport.follow_track(index, count);
    }

    /// Drive a frontend until it quits. Rendering errors are reported and the
    /// loop continues (recoverable errors degrade locally).
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
            // the host is asked for a picture once, at the end.
            let events = frontend.poll();
            if self.drain(events, host).contains(&Response::Quit) {
                return Ok(());
            }
            if self.quit || host.wants_quit() {
                return Ok(());
            }
            let view = self.view();
            if let Err(e) = frontend.render(&view) {
                self.fail(e.to_string());
            }
        }
    }
}
