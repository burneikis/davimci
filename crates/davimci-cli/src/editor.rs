//! The glue: workspace + backend + presenter + transport behind one
//! [`Host`] (plan.md Phase 9a/9b wiring).
//!
//! This is the only place that is allowed to know about all of them at once.
//! It lives in the binary crate on purpose: no frontend may reference MLT
//! (spec §10.1), so the thing that owns a `RenderBackend` *and* a frontend
//! cannot be `davimci-gui`.
//!
//! Session ownership: `App` owns the live session, the workspace owns the
//! buffers. Rather than keep two copies in step, the live one is pushed into
//! the workspace before a `:` command and pulled back after - so `:w` always
//! writes what is on screen, and `:bn` hands back a different timeline.

use davimci_app::{AppError, Host, Message};
use davimci_backend::{PreviewScale, RenderBackend};
use davimci_cmd::Session;
use davimci_core::Frame;
use davimci_keys::engine::TransportCmd;
use davimci_present::{Presentation, Presenter};

use crate::autosave::OnRecovery;
use crate::excmd::ExOutcome;
use crate::transport::{Transport, TransportState};
use crate::workspace::Workspace;

/// Everything the editor needs that is not the frontend.
pub struct Editor {
    workspace: Workspace,
    backend: Box<dyn RenderBackend>,
    presenter: Presenter,
    transport: Transport,
    scale: PreviewScale,
    /// Set when the app adopted a different timeline and the frontend has to
    /// be told; drained by [`Editor::take_session_swap`].
    swap: Option<Session>,
    last: Option<Presentation>,
    /// Deferred status text produced by transport and preview, which have no
    /// other way to reach the status line.
    notices: Vec<Message>,
    quit: bool,
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor")
            .field("transport", &self.transport.state())
            .field("scale", &self.scale)
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

impl Editor {
    #[must_use]
    pub fn new(
        workspace: Workspace,
        backend: Box<dyn RenderBackend>,
        presenter: Presenter,
    ) -> Self {
        Self {
            workspace,
            backend,
            presenter,
            transport: Transport::new(),
            scale: PreviewScale::Full,
            swap: None,
            last: None,
            notices: Vec::new(),
            quit: false,
        }
    }

    #[must_use]
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspace
    }

    #[must_use]
    pub fn presenter(&self) -> &Presenter {
        &self.presenter
    }

    pub fn presenter_mut(&mut self) -> &mut Presenter {
        &mut self.presenter
    }

    #[must_use]
    pub fn transport_state(&self) -> TransportState {
        self.transport.state()
    }

    /// The most recently composited frame, for a frontend to draw.
    #[must_use]
    pub fn presentation(&self) -> Option<&Presentation> {
        self.last.as_ref()
    }

    pub fn set_scale(&mut self, scale: PreviewScale) {
        self.scale = scale;
    }

    /// Messages produced by transport/preview since the last call.
    pub fn take_notices(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.notices)
    }

    /// A timeline the app should adopt (`:e`, `:bn`, `:b <n>` switched
    /// buffers), if any.
    pub fn take_session_swap(&mut self) -> Option<Session> {
        self.swap.take()
    }

    /// Project the timeline and show the frame under the playhead. Called
    /// once at startup so the first paint is not black.
    pub fn prime(&mut self, session: &Session) {
        self.timeline_changed(session);
        self.playhead_moved(session);
    }

    /// Recompose the current frame at the presenter's *current* surface.
    ///
    /// A composed frame is sized to the surface it was composed for, so
    /// resizing the video pane invalidates it. Without this the picture keeps
    /// its old size and sits in the corner of a resized pane.
    pub fn refresh_preview(&mut self, session: &Session) {
        self.show_playhead(session);
    }

    /// Push the graph to the backend, reporting failure as a status message
    /// rather than killing the session - an unprojectable timeline is still
    /// an editable one (Phase 0: degrade locally).
    fn project(&mut self, session: &Session) {
        if let Err(e) = self.backend.set_timeline(session.timeline()) {
            self.notices.push(Message::error(e.to_string()));
        }
    }

    /// Pull and compose the frame at the playhead. Only meaningful when the
    /// transport is idle: during playback the pacer owns the picture.
    fn show_playhead(&mut self, session: &Session) {
        if self.transport.state() != TransportState::Stopped {
            return;
        }
        let at = session.timeline().playhead().frame;
        if let Err(e) = self.backend.seek(at) {
            self.notices.push(Message::error(e.to_string()));
            return;
        }
        match self.backend.frame_at(at, self.scale) {
            Ok(frame) => match self.presenter.present_frame(frame) {
                Ok(p) => self.last = Some(p),
                Err(e) => self.notices.push(Message::error(e.to_string())),
            },
            Err(e) => self.notices.push(Message::error(e.to_string())),
        }
    }
}

impl Host for Editor {
    fn command(&mut self, line: &str, session: &mut Session) -> Result<Option<String>, AppError> {
        // The app holds the live session; give it to the workspace so the
        // command acts on what the user can see.
        self.workspace.set_current_session(session.clone());
        let outcome = self.workspace.run(line, OnRecovery::Discard);
        // Take back whatever buffer is now current - possibly a different
        // timeline entirely.
        let after = self.workspace.current_session();
        if after.timeline() != session.timeline() {
            self.swap = Some(after.clone());
        }
        *session = after;
        if self.workspace.should_quit() {
            self.quit = true;
        }
        match outcome {
            Ok(ExOutcome::Message(m)) => Ok(Some(m)),
            Ok(ExOutcome::Lines(lines)) => Ok(Some(lines.join("  |  "))),
            Ok(ExOutcome::Quit) => {
                self.quit = true;
                Ok(Some("closed the last timeline".into()))
            }
            Err(e) => Err(AppError::UnhandledCommand(e.to_string())),
        }
    }

    fn transport(&mut self, cmd: TransportCmd) {
        // Transport needs the session only to read the playhead and the
        // duration, and the app owns it - so the workspace's copy, which is
        // synced on every command, is close enough for the read-only use and
        // the authoritative playhead arrives back through `tick`.
        let session = self.workspace.current_session();
        let result = match cmd {
            TransportCmd::PlayPause => {
                self.transport
                    .play_pause(self.backend.as_mut(), &session, self.scale)
            }
            TransportCmd::PreviewAndReturn => {
                self.transport
                    .preview_and_return(self.backend.as_mut(), &session, self.scale)
            }
            TransportCmd::ShuttleForward => {
                self.transport
                    .shuttle(true, self.backend.as_mut(), &session)
            }
            TransportCmd::ShuttleBackward => {
                self.transport
                    .shuttle(false, self.backend.as_mut(), &session)
            }
            TransportCmd::ShuttleStop => {
                self.transport.shuttle_stop(self.backend.as_mut(), &session)
            }
            // Looping needs the visual selection, which lives in the key
            // engine rather than the session; wiring it needs a selection on
            // the `Host` seam (tracked, not silently ignored).
            TransportCmd::LoopSelection => Err("looping a selection is not wired up yet".into()),
        };
        match result {
            Ok(msg) => self.notices.push(Message::info(msg)),
            Err(msg) => self.notices.push(Message::error(msg)),
        }
    }

    fn tick(&mut self, session: &mut Session) {
        // Keep the workspace's playhead in step so transport reads are not a
        // frame behind, then run one presentation tick.
        self.workspace.set_current_session(session.clone());
        let result = self.transport.tick(
            self.backend.as_mut(),
            &mut self.presenter,
            session,
            self.scale,
        );
        if let Some(frame) = result.playhead {
            let track = session.timeline().playhead().track;
            // Playback moves the playhead; that is navigation, never an edit.
            if let Err(e) = session.set_playhead(frame, track) {
                self.notices.push(Message::error(e.to_string()));
            }
        }
        // One tick, one frame: the transport already composed it.
        if let Some(p) = result.presentation {
            self.last = Some(p);
        }
        if result.stopped {
            self.notices.push(Message::info("stopped".to_string()));
            // Show the frame we landed on, now that the pacer has let go.
            let snapshot = session.clone();
            self.show_playhead(&snapshot);
        }
    }

    fn timeline_changed(&mut self, session: &Session) {
        self.project(session);
    }

    fn playhead_moved(&mut self, session: &Session) {
        self.show_playhead(session);
    }

    fn wants_quit(&self) -> bool {
        self.quit
    }
}

/// The frame a fresh editor should show: frame zero of the current timeline.
#[must_use]
pub fn first_frame() -> Frame {
    Frame::ZERO
}
