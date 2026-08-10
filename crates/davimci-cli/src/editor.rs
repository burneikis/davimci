//! The glue: workspace + backend + presenter + transport behind one
//! [`Host`].
//!
//! This is the only place that is allowed to know about all of them at once.
//! It lives in the binary crate on purpose: no frontend may reference MLT
//!, so the thing that owns a `RenderBackend` *and* a frontend
//! cannot be `davimci-gui`.
//!
//! Session ownership: `App` owns the live session, the workspace owns the
//! buffers. Rather than keep two copies in step, the live one is pushed into
//! the workspace before a `:` command and pulled back after - so `:w` always
//! writes what is on screen, and `:bn` hands back a different timeline.

use davimci_analysis::{FfprobeProber, ImportOptions, Placement, Prober};
use davimci_app::{
    AppError, Host, JobState, JobUpdate, Message, PluginEffects, Thumbnail, ThumbnailRequest,
    Waveform,
};
use davimci_backend::{DecodePolicy, PreviewScale, RenderBackend};
use davimci_cmd::{EditCommand, Session};
use davimci_core::{ClipId, Frame, Selection, TrackId};
use davimci_keys::MediaIntent;
use davimci_keys::engine::TransportCmd;
use davimci_lua::{MotionAnswer, MotionEnv, Request, Sample, TrackData};
use davimci_present::{Presentation, Presenter};

use crate::analyse::Analyser;
use crate::autosave::OnRecovery;
use crate::error::CliError;
use crate::excmd::{ExCommand, ExOutcome};
use crate::export::{ExportEvent, Exporter};
use crate::plugins::Plugins;
use crate::setting::{Numbers, PreviewHeight, PreviewProtocol};
use crate::transport::{Transport, TransportState};
use crate::workspace::Workspace;

/// How tall a timeline thumbnail is decoded, in pixels. A lane is 40 px in
/// the default metrics; a picture taller than its lane is pixels thrown away
/// at draw time.
const THUMBNAIL_HEIGHT: u32 = 40;

/// The id the project-local config question is raised under.
const TRUST_CONFIRM: u64 = 1;

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
    /// The preset registry and whatever export is running (Phase 8b).
    exporter: Exporter,
    /// Background analysis of the audio tracks (Phase 9e).
    analyser: Analyser,
    /// `:set proxy`: the policy, the encodes it started, and which proxy
    /// stands in for which original.
    proxies: crate::proxy::Proxies,
    /// Proxies that finished while the transport was running, waiting for it
    /// to stop before the graph is rebuilt onto them.
    proxies_landed: usize,
    /// Job updates waiting for the app to collect on the next tick.
    job_updates: Vec<JobUpdate>,
    /// Envelopes finished since the app last collected them.
    pending_waveforms: Vec<(TrackId, Waveform)>,
    /// Thumbnails decoded since the app last collected them.
    pending_thumbnails: Vec<(davimci_core::ClipId, Thumbnail)>,
    /// The next clip to decode a thumbnail for, if the app asked for any.
    /// One per tick: a thumbnail is a decode, and the preview needs the
    /// decoder more than the timeline does.
    thumbnail_queue: Option<ThumbnailRequest>,
    /// Job ids the app has already been told about.
    started_jobs: Vec<u64>,
    /// How picked media is inspected. Injected so the picker path is
    /// testable without ffprobe on the machine.
    prober: Box<dyn Prober>,
    /// The Lua runtime and everything user config registered into it.
    plugins: Plugins,
    /// Requests an event handler queued, waiting for the next tick. They are
    /// not run where the event fires: an edit must not happen inside the
    /// notification that an edit happened.
    queued_requests: Vec<Request>,
    /// Messages the plugin layer produced outside a callback.
    plugin_messages: Vec<Message>,
    /// The `on_key` handler of each focusable panel a plugin opened, so a
    /// keystroke into a focused panel reaches the plugin that owns it.
    panel_handlers: std::collections::BTreeMap<u32, u32>,
    /// The preset and output of the running export, for `AfterExport`.
    last_export: Option<(String, String)>,
    /// Which clips existed at the last notification, so an edit can be
    /// reported as `ClipInserted`/`ClipDeleted` without every
    /// command having to describe itself twice.
    known_clips: std::collections::BTreeSet<(TrackId, ClipId)>,
    /// `:set preview off`: no frame is pulled or composed, which
    /// is what makes a no-display session possible.
    preview: bool,
    /// `:set decode cpu|auto`: what the session has *asked* for. What it
    /// gets is the backend's business, since a probe may refuse.
    decode: DecodePolicy,
    /// `:set encode cpu|auto`: whether the next export may be accelerated.
    /// A preset that requires hardware is binding regardless, and refused by
    /// the backend when it cannot be met.
    encode: davimci_backend::EncodePolicy,
    /// Whether the host draws planar frames itself. Set by a host with a
    /// GPU and a shader; every other frontend, and every test, stays on the
    /// RGBA composition that the parity assertions are written against.
    planar_preview: bool,
    /// Preview resolution that follows the frame budget during playback.
    /// Never a setting and never an edit: everything it took is given back
    /// when playback stops.
    adaptive: davimci_present::AdaptiveScale,
    /// `:set previewheight` and `:set previewprotocol`. Held here
    /// with the other view settings and read by the terminal session; inert
    /// for the window, which has a texture instead of a band.
    /// `None` until `:set previewheight` says otherwise, because the band a
    /// terminal opens with and the pane a window opens with are not the same
    /// default. Each frontend resolves `None` for itself.
    preview_height: Option<PreviewHeight>,
    preview_protocol: PreviewProtocol,
    numbers: Numbers,
    /// A project-local `.davimci.lua` the user has not been asked about
    /// yet. Nothing of it has been read; the question goes to the frontend
    /// and only a yes runs it.
    pending_trust: Option<std::path::PathBuf>,
    /// True once the question has been handed to the app, so it is asked
    /// once rather than on every tick.
    trust_asked: bool,
    /// A keymap rebuilt after config loaded late, taken by the app.
    pending_keymap: Option<davimci_keys::Keymap>,
    /// Set by `:set visualstart`, taken by the app on the next poll: the
    /// setting is parsed here and enforced by the key engine.
    visual_start: davimci_keys::VisualStart,
    pending_visual_start: Option<davimci_keys::VisualStart>,
    /// Whether `i` on a text track edits the cue under the playhead, taken by
    /// the app. Only the plugin that owns text tracks grants this, so the
    /// grammar never has to know a text workflow exists.
    pending_text_editing: Option<bool>,
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
            analyser: Analyser::new(workspace.root()),
            proxies: crate::proxy::Proxies::new(workspace.root()),
            proxies_landed: 0,
            workspace,
            backend,
            presenter,
            transport: Transport::new(),
            scale: PreviewScale::Full,
            swap: None,
            last: None,
            notices: Vec::new(),
            exporter: Exporter::new(),
            job_updates: Vec::new(),
            pending_waveforms: Vec::new(),
            pending_thumbnails: Vec::new(),
            thumbnail_queue: None,
            started_jobs: Vec::new(),
            prober: Box::new(FfprobeProber),
            plugins: Plugins::empty(),
            queued_requests: Vec::new(),
            plugin_messages: Vec::new(),
            panel_handlers: std::collections::BTreeMap::new(),
            last_export: None,
            known_clips: std::collections::BTreeSet::new(),
            preview: true,
            decode: DecodePolicy::default(),
            encode: davimci_backend::EncodePolicy::default(),
            planar_preview: false,
            adaptive: davimci_present::AdaptiveScale::new(),
            preview_height: None,
            preview_protocol: PreviewProtocol::Auto,
            numbers: Numbers::Off,
            pending_trust: None,
            trust_asked: false,
            pending_keymap: None,
            visual_start: davimci_keys::VisualStart::default(),
            pending_visual_start: None,
            pending_text_editing: None,
            quit: false,
        }
    }

    /// Adopt a loaded Lua runtime, and with it the user's export presets.
    ///
    /// Presets are installed rather than consulted lazily so `:presets` and
    /// Tab completion list what the config defined, and a preset the backend
    /// cannot build is reported now rather than at render time.
    #[must_use]
    pub fn with_plugins(mut self, plugins: Plugins) -> Self {
        self.plugins = plugins;
        self.install_registrations();
        // The project on argv is already open by now, and it may have been
        // written with a plugin this session has not run.
        let session = self.workspace.current_session();
        self.activate_plugins_for(&session);
        self
    }

    /// Put the project-local config to the user, in the frontend.
    ///
    /// The file is untouched until the answer comes back through
    /// [`Host::confirmed`]: this only records what to ask about.
    pub fn ask_about_project_config(&mut self, path: &std::path::Path) {
        self.pending_trust = Some(path.to_path_buf());
        self.trust_asked = false;
    }

    /// Install what a freshly loaded config registered.
    ///
    /// Presets and transition types are pushed rather than looked up lazily,
    /// so `:presets` and Tab completion list what the config defined and a
    /// preset the backend cannot build is reported now, not at render time.
    fn install_registrations(&mut self) {
        let (presets, problems) = self.plugins.presets();
        for preset in presets {
            self.exporter.presets_mut().define(preset);
        }
        for def in self.plugins.transitions() {
            let name = def.name.clone();
            if let Err(e) = self.backend.register_transition(def) {
                self.notices.push(Message::warning(format!(
                    "the transition type '{name}' is not available: {e}"
                )));
            }
        }
        self.notices.extend(problems);
        self.notices.extend(self.plugins.take_notices());
        self.pending_text_editing = Some(self.plugins.is_active("text"));
    }

    /// Turn on the bundled plugins this project's own contents need.
    ///
    /// A saved project names transition types, and a type nothing registered
    /// renders as a dissolve - silently losing what the editor that wrote
    /// the file could do; a project with a text track opened without the
    /// plugin that owns text tracks would have cues nothing could edit. So
    /// the project is what asks: opening one that uses a wipe switches the
    /// plugin that owns wipes on, and says so. A config that disabled the
    /// plugin outright is still obeyed.
    fn activate_plugins_for(&mut self, session: &Session) {
        // One message per plugin, naming the first thing that needed it: a
        // project with fifty wipes should not be fifty notices.
        let mut wanted: std::collections::BTreeMap<&'static str, Need> =
            std::collections::BTreeMap::new();
        for track in session.timeline().tracks() {
            if let Some(owner) = crate::plugins::provider_of_track_kind(track.kind.prefix())
                && !self.plugins.is_active(owner.name())
            {
                wanted
                    .entry(owner.name())
                    .or_insert_with(|| Need::TrackKind(track.name.clone()));
            }
            for clip in track.clips() {
                let Some(kind) = clip.transition_in.as_ref().map(|t| t.kind.as_str()) else {
                    continue;
                };
                let Some(owner) = crate::plugins::provider_of_transition(kind) else {
                    continue;
                };
                if self.plugins.is_active(owner.name()) {
                    continue;
                }
                wanted
                    .entry(owner.name())
                    .or_insert_with(|| Need::Transition(kind.to_string()));
            }
        }
        if wanted.is_empty() {
            return;
        }
        for (owner, need) in wanted {
            let Some(plugin) = crate::plugins::BUNDLED.iter().find(|p| p.name() == owner) else {
                continue;
            };
            if self.plugins.activate(plugin) {
                self.notices.push(Message::info(format!(
                    "enabled the bundled '{owner}' plugin: {}",
                    need.because()
                )));
            } else {
                self.notices.push(Message::warning(format!(
                    "{}, {}",
                    need.because(),
                    need.cost(owner)
                )));
            }
        }
        self.install_registrations();
        self.pending_keymap = Some(self.plugins.keymap());
    }

    #[must_use]
    pub fn plugins(&self) -> &Plugins {
        &self.plugins
    }

    pub fn plugins_mut(&mut self) -> &mut Plugins {
        &mut self.plugins
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

    /// Replace the media prober. Tests use this to import without ffprobe.
    #[must_use]
    pub fn with_prober(mut self, prober: Box<dyn Prober>) -> Self {
        self.prober = prober;
        self
    }

    #[must_use]
    pub fn exporter(&self) -> &Exporter {
        &self.exporter
    }

    pub fn exporter_mut(&mut self) -> &mut Exporter {
        &mut self.exporter
    }

    /// Run a command about the plugins themselves. The workspace cannot
    /// answer these: it has no runtime, and what is loaded is the editor's.
    fn plugin_command(&self, cmd: &ExCommand) -> Option<Result<String, CliError>> {
        match cmd {
            ExCommand::CheckHealth => Some(Ok(self.plugins.health().join("  |  "))),
            _ => None,
        }
    }

    /// Run an export command. Split out from [`Editor::command`] because
    /// these are the only `:` commands the workspace cannot answer: they
    /// need the render backend, which only the editor holds.
    fn export_command(
        &mut self,
        cmd: &ExCommand,
        session: &Session,
    ) -> Option<Result<String, CliError>> {
        match cmd {
            ExCommand::Export { path, preset } => {
                let name = preset.clone().unwrap_or_else(|| self.preset_for(path));
                if let Some(reason) = self.before_export(&name, &path.display().to_string()) {
                    return Some(Err(CliError::ExportRefused { reason }));
                }
                let shipped = match self.shipping_timeline(session) {
                    Ok(tl) => tl,
                    Err(e) => return Some(Err(e)),
                };
                Some(
                    self.exporter
                        .start(self.backend.as_mut(), path, preset.as_deref(), &shipped),
                )
            }
            ExCommand::Render { preset } => {
                let container = match self.exporter.presets().get(preset) {
                    Ok(p) => p.container,
                    Err(e) => return Some(Err(e.into())),
                };
                // `:render` names the file after the project, so the common
                // case needs no path at all.
                let out = crate::export::default_output(
                    self.workspace.current().path().map(std::path::Path::new),
                    container,
                );
                if let Some(reason) = self.before_export(preset, &out.display().to_string()) {
                    return Some(Err(CliError::ExportRefused { reason }));
                }
                let shipped = match self.shipping_timeline(session) {
                    Ok(tl) => tl,
                    Err(e) => return Some(Err(e)),
                };
                Some(
                    self.exporter
                        .start(self.backend.as_mut(), &out, Some(preset), &shipped),
                )
            }
            // `:set preview off` is a view setting, so it never enters the
            // undo log; it belongs here because the preview is the editor's.
            ExCommand::Set(crate::setting::Setting::Preview(on)) => {
                Some(Ok(self.set_preview(*on, session)))
            }
            // Inert here by design: both frontends read this every loop and
            // turn it into rows or pixels themselves.
            // A session policy, not an edit: it decides what the *next*
            // import decodes from, so it never reaches the undo log.
            ExCommand::Set(crate::setting::Setting::Proxy(on)) => {
                Some(Ok(self.proxies.set_enabled(*on)))
            }
            // Acceleration is never a command: it changes how pixels are
            // produced, not what the timeline holds. The backend answers
            // with what it is actually doing, which may be software.
            ExCommand::Set(crate::setting::Setting::Decode(policy)) => {
                self.decode = *policy;
                let status = self.backend.set_decode_policy(*policy);
                Some(Ok(status.detail))
            }
            ExCommand::Set(crate::setting::Setting::Encode(policy)) => {
                self.encode = *policy;
                self.backend.set_encode_policy(*policy);
                Some(Ok(format!(
                    "Exports will use {}.",
                    match policy {
                        davimci_backend::EncodePolicy::Cpu => "a software encoder",
                        davimci_backend::EncodePolicy::Auto =>
                            "a hardware encoder where one meets the preset",
                    }
                )))
            }
            ExCommand::Set(crate::setting::Setting::PreviewHeight(height)) => {
                self.preview_height = Some(*height);
                Some(Ok(height.describe()))
            }
            ExCommand::Set(crate::setting::Setting::PreviewProtocol(protocol)) => {
                self.preview_protocol = *protocol;
                Some(Ok(format!("preview protocol {}", protocol.name())))
            }
            ExCommand::Set(crate::setting::Setting::Numbers(numbers)) => {
                self.numbers = *numbers;
                Some(Ok(numbers.describe().to_string()))
            }
            ExCommand::Set(crate::setting::Setting::VisualStart(start)) => {
                self.visual_start = *start;
                self.pending_visual_start = Some(*start);
                Some(Ok(start.describe().to_string()))
            }
            ExCommand::Presets => Some(Ok(self.exporter.list_presets().join("  |  "))),
            ExCommand::CancelRender => Some(self.exporter.cancel(self.backend.as_mut())),
            _ => None,
        }
    }

    /// The timeline an export ships, checked by the guard that must never
    /// see a proxy.
    ///
    /// It is the session's own timeline: a proxy is substituted on the way
    /// to the preview graph and never enters the model, so there is nothing
    /// to relink back. The guard stays because rendering 540p by accident is
    /// the one proxy failure a user would not notice until it is published.
    fn shipping_timeline(&self, session: &Session) -> Result<davimci_core::Timeline, CliError> {
        let tl = session.timeline().clone();
        self.proxies.check_export(&tl)?;
        Ok(tl)
    }

    /// Take the proxies that finished encoding and reproject onto them.
    ///
    /// No command, no undo entry: the timeline does not change. What changes
    /// is which file the preview graph decodes, which is why the projection
    /// is rebuilt rather than the model edited.
    ///
    /// Never while the transport is running. Rebuilding the graph out from
    /// under a consumer that is pulling from it is the same hazard an edit
    /// during playback has, and an encode finishing is not a reason to
    /// interrupt playback - so the swap waits for the transport to stop.
    fn adopt_finished_proxies(&mut self, session: &Session) {
        let (updates, swaps) = self.proxies.poll();
        self.job_updates.extend(updates);
        self.proxies_landed += swaps
            .iter()
            .filter(|(source, _)| {
                session
                    .timeline()
                    .tracks()
                    .iter()
                    .flat_map(davimci_core::Track::clips)
                    .any(|c| c.media.as_ref().is_some_and(|m| &m.path == source))
            })
            .count();
        if self.proxies_landed == 0 || self.transport.state() != TransportState::Stopped {
            return;
        }
        let landed = std::mem::take(&mut self.proxies_landed);
        self.project(session);
        self.show_playhead(session);
        self.notices.push(Message::info(format!(
            "{landed} source(s) are now decoding from a proxy"
        )));
    }

    /// `:set preview on|off`.
    ///
    /// Turning the preview off stops the transport and drops the last
    /// composed frame, so a no-display session neither decodes nor paints;
    /// turning it back on shows the frame under the playhead again.
    fn set_preview(&mut self, on: bool, session: &Session) -> String {
        self.preview = on;
        if on {
            self.show_playhead(session);
            "preview on".into()
        } else {
            self.interrupt_transport(session);
            self.last = None;
            "preview off".into()
        }
    }

    /// Whether the preview is showing frames at all (`:set preview`).
    #[must_use]
    pub fn preview_enabled(&self) -> bool {
        self.preview
    }

    /// Declare that this host uploads planar YUV and converts it on the GPU.
    ///
    /// Ignored by a backend that cannot decode planar, so a host may ask
    /// unconditionally and get the RGBA path where there is no other.
    pub fn set_planar_preview(&mut self, on: bool) {
        self.planar_preview = on;
    }

    /// The render backend, for a test that has to ask it something the
    /// editor does not expose - a refused export, say.
    pub fn backend_mut(&mut self) -> &mut dyn RenderBackend {
        self.backend.as_mut()
    }

    /// What the backend is actually decoding with, as a complete sentence.
    ///
    /// Asked of the backend rather than remembered here: `:set decode auto`
    /// on a machine with no usable device is a session that keeps decoding
    /// in software, and a report that echoed the request would hide that.
    #[must_use]
    pub fn acceleration(&self) -> davimci_backend::AccelerationStatus {
        self.backend.acceleration()
    }

    /// What `:set previewheight` asks for, or `None` if it was never set.
    #[must_use]
    pub fn preview_height(&self) -> Option<PreviewHeight> {
        self.preview_height
    }

    /// What `:set previewprotocol` asks the terminal to draw with.
    #[must_use]
    pub fn preview_protocol(&self) -> PreviewProtocol {
        self.preview_protocol
    }

    /// What every `:set` property holds right now, for completion: the view
    /// settings the editor owns, and the clip and transition the next
    /// `:set clip.*` / `:set transition.*` would act on.
    #[must_use]
    pub fn current_settings(&self, session: &Session) -> crate::setting::CurrentSettings {
        let tl = session.timeline();
        let head = tl.playhead();
        let clip = tl.track(head.track).and_then(|t| t.clip_at(head.frame));
        crate::setting::CurrentSettings {
            preview: Some(self.preview),
            decode: Some(self.decode),
            encode: Some(self.encode),
            proxy: Some(self.proxies.enabled()),
            preview_height: self.preview_height,
            preview_protocol: Some(self.preview_protocol),
            numbers: Some(self.numbers),
            visual_start: Some(self.visual_start),
            fps: Some(tl.props.fps),
            resolution: Some(tl.props.resolution),
            clip: clip.map(|c| c.props),
            transition: tl
                .transition_at(head.track, head.frame)
                .map(|(_, t)| t.clone())
                .or_else(|| clip.and_then(|c| c.transition_in.clone())),
        }
    }

    /// What `:set numbers` (or `--numbers`) asks the ruler to label with.
    #[must_use]
    pub fn numbers(&self) -> Numbers {
        self.numbers
    }

    /// The startup value of `:set numbers`, from `--numbers`.
    pub fn set_numbers(&mut self, numbers: Numbers) {
        self.numbers = numbers;
    }

    /// `:normalize` and `:duck`.
    ///
    /// They live here rather than in the workspace for the same reason the
    /// export commands do: they need something the workspace has no business
    /// owning - in this case the analysis, which is what "how loud is this"
    /// and "where is the other track audible" both come down to.
    fn audio_command(
        &mut self,
        cmd: &ExCommand,
        session: &mut Session,
        selection: Option<&Selection>,
    ) -> Option<Result<String, CliError>> {
        match cmd {
            ExCommand::Normalize { target_db } => {
                Some(self.normalize(*target_db, session, selection))
            }
            ExCommand::Duck { track, db } => Some(self.duck(track, *db, session, selection)),
            // Every envelope is dropped and re-measured, reported like any
            // other background job.
            ExCommand::Analyze => {
                let n = self.analyser.reanalyse();
                Some(Ok(if n == 0 {
                    "there is no audio in this timeline to analyse".to_string()
                } else {
                    format!("re-analysing {n} track(s)")
                }))
            }
            _ => None,
        }
    }

    /// `:normalize` - each clip in the selection is measured and gained on
    /// its own, since "the same loudness" is a per-clip statement; the whole
    /// set is still one command, so one `u` undoes it.
    fn normalize(
        &mut self,
        target_db: f32,
        session: &mut Session,
        selection: Option<&Selection>,
    ) -> Result<String, CliError> {
        let fps = session.timeline().props.fps;
        let clips = crate::audio::targets(session.timeline(), selection, "normalize")?;
        let mut cmds = Vec::with_capacity(clips.len());
        let mut last_gain = 0.0;
        for (track, clip) in &clips {
            let analysis = self
                .analyser
                .analysis(*track)
                .ok_or(CliError::AnalysisNotReady(":normalize"))?;
            let gain = crate::audio::normalize_gain(clip, analysis, fps, target_db)
                .ok_or(CliError::AnalysisNotReady(":normalize"))?;
            last_gain = gain;
            cmds.push(crate::audio::gain(*track, clip, gain));
        }
        session.exec(&davimci_cmd::EditCommand::Sequence(cmds))?;
        let what = crate::audio::describe(&clips);
        Ok(if clips.len() == 1 {
            format!("{what} normalised to {target_db} dB ({last_gain:+.1} dB)")
        } else {
            format!("{what} normalised to {target_db} dB")
        })
    }

    fn duck(
        &mut self,
        name: &str,
        db: f32,
        session: &mut Session,
        selection: Option<&Selection>,
    ) -> Result<String, CliError> {
        let reference = session
            .timeline()
            .track_by_name(name)
            .map(|t| t.id)
            .ok_or_else(|| CliError::NoSuchTrack(name.to_string()))?;
        let targets = crate::audio::target_tracks(session.timeline(), selection);
        if targets.contains(&reference) {
            return Err(CliError::NoSuchTrack(format!(
                "{name}; a track cannot duck against itself"
            )));
        }
        let analysis = self
            .analyser
            .analysis(reference)
            .ok_or(CliError::AnalysisNotReady(":duck"))?;
        let spans = crate::audio::loud_spans(session.timeline(), reference, analysis);
        let needed: usize = targets
            .iter()
            .map(|t| crate::audio::duck_ids_needed(session.timeline(), *t, &spans))
            .sum();
        let ids = session.reserve_ids(needed);
        let mut ids = ids.into_iter();
        // Built before anything is applied, so a duck that cannot land
        // leaves the timeline untouched (Phase 0 user-error policy).
        let mut plans = Vec::with_capacity(targets.len());
        for track in &targets {
            plans.push(crate::audio::duck_plan(
                session.timeline(),
                *track,
                &spans,
                db,
                &mut ids,
            )?);
        }
        session.exec(&davimci_cmd::EditCommand::Sequence(plans))?;
        Ok(format!(
            "ducked by {db} dB under {} region(s) of {name}",
            spans.len()
        ))
    }

    /// Import a picked file at the position the intent implies.
    ///
    /// All three intents are one command, so one `u` undoes the whole import
    /// including the delete that `r` needs.
    fn import_picked(
        &mut self,
        path: &std::path::Path,
        intent: MediaIntent,
        session: &mut Session,
    ) -> Result<String, CliError> {
        let info = self.prober.probe(path)?;
        let head = session.timeline().playhead();
        let clip = session
            .timeline()
            .track(head.track)
            .and_then(|t| t.clip_at(head.frame))
            .cloned();

        // Where the new media lands, and what has to get out of its way.
        let (at, replaced) = match intent {
            MediaIntent::Insert => (head.frame, None),
            // Append means *after* the clip under the playhead, not at the
            // playhead - otherwise `a` and `i` would be the same key.
            MediaIntent::Append => (
                clip.as_ref().map_or(head.frame, davimci_core::Clip::end),
                None,
            ),
            MediaIntent::Replace => {
                let c = clip.ok_or(CliError::NothingToReplace)?;
                (c.start, Some((head.track, c.start, c.end())))
            }
        };

        let (subtitles, unread) = davimci_analysis::extract_all(&info);
        let opts = ImportOptions {
            at,
            // The playhead is on a track; that is where the user is looking.
            target: Some(head.track),
            // Imported media pushes what follows aside rather than erasing
            // it; nothing is destroyed by picking a file.
            placement: Placement::Insert,
            subtitles,
            ..ImportOptions::default()
        };
        for problem in &unread {
            self.notices.push(Message::warning(format!(
                "a subtitle stream was imported without its cues: {problem}"
            )));
        }
        let ids = session.reserve_ids(davimci_analysis::ids_needed(&info, &opts));
        let plan = davimci_analysis::plan(session.timeline(), &info, &opts, &ids)?;

        let command = match replaced {
            // Delete first, then insert into the gap that leaves, so the
            // replacement need not match the old clip's length.
            Some((track, start, end)) => EditCommand::Sequence(vec![
                EditCommand::RippleDelete { track, start, end },
                plan.command,
            ]),
            None => plan.command,
        };
        session.exec(&command)?;

        // A proxy is decided per source, and only after the import
        // succeeded: encoding one for a file that failed to import would be
        // minutes of CPU for nothing.
        let proxying = self
            .proxies
            .queue_for_import(&info, session.timeline().props);
        if let Some(msg) = proxying {
            self.notices.push(Message::info(msg));
        }

        let verb = match intent {
            MediaIntent::Insert => "inserted",
            MediaIntent::Append => "appended",
            MediaIntent::Replace => "replaced with",
        };
        Ok(format!(
            "{verb} {} at {} ({} track(s))",
            plan.result.path,
            at.get(),
            plan.result.mapping.len()
        ))
    }

    /// Poll the running export and turn its progress into job updates and,
    /// at the end, one status line.
    fn poll_export(&mut self) {
        let Some(event) = self.exporter.poll(self.backend.as_ref()) else {
            return;
        };
        match event {
            ExportEvent::Progress { id, permille } => {
                // A job the app has not seen yet must be started first, or
                // progress for an unknown id would be dropped.
                if !self.started_jobs.contains(&id) {
                    self.started_jobs.push(id);
                    self.job_updates.push(JobUpdate::Started {
                        id,
                        label: "export".into(),
                    });
                }
                self.job_updates.push(JobUpdate::Progress { id, permille });
            }
            ExportEvent::Finished { id, message } => {
                let (preset, output) = self.last_export.clone().unwrap_or_default();
                self.fire(&davimci_lua::Event::AfterExport { preset, output });
                self.finish_job(id, JobState::Done, message, false);
            }
            ExportEvent::Cancelled { id, message } => {
                self.finish_job(id, JobState::Cancelled, message, false);
            }
            ExportEvent::Failed { id, message } => {
                self.finish_job(id, JobState::Failed, message, true);
            }
        }
    }

    fn finish_job(&mut self, id: u64, state: JobState, message: String, is_error: bool) {
        if !self.started_jobs.contains(&id) {
            self.started_jobs.push(id);
            self.job_updates.push(JobUpdate::Started {
                id,
                label: "export".into(),
            });
        }
        self.job_updates.push(JobUpdate::Finished { id, state });
        self.notices.push(if is_error {
            Message::error(message)
        } else {
            Message::info(message)
        });
    }

    /// The range `<Space>l` is looping, if any.
    #[must_use]
    pub fn loop_range(&self) -> Option<(Frame, Frame)> {
        self.transport.loop_range()
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

    /// Stop everything the session started, before the frontend closes.
    ///
    /// Dropping the editor already does this, but drop order runs it *after*
    /// the window is gone: an encode still running then holds a dead window
    /// on screen and the process alive for as long as ffmpeg takes. Asking
    /// first, while the last frame is still up, is what makes quit look
    /// immediate. Cancellation is a request, so the joins still happen in
    /// `Drop`; they just have nothing left to wait for.
    pub fn shutdown(&mut self) {
        self.analyser.cancel_all();
        self.proxies.cancel_all();
        if self.exporter.is_running() {
            let _ = self.exporter.cancel(self.backend.as_mut());
        }
        let _ = self.transport.interrupt(self.backend.as_mut());
        if self.backend.is_previewing() {
            let _ = self.backend.preview_stop();
        }
    }

    /// Project the timeline and show the frame under the playhead. Called
    /// once at startup so the first paint is not black.
    pub fn prime(&mut self, session: &Session) {
        // The clip set is adopted, not diffed: nothing was inserted by
        // opening a project, so nothing should be reported as inserted.
        self.known_clips = session
            .timeline()
            .tracks()
            .iter()
            .flat_map(|t| t.clips().iter().map(move |c| (t.id, c.id)))
            .collect();
        let path = self
            .workspace
            .current()
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.fire(&davimci_lua::Event::ProjectLoaded { path });
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
        // The one place a proxy stands in for its original: everything above
        // this line is the timeline the user edited.
        let projected = self.proxies.with_proxies(session.timeline());
        if let Err(e) = self.backend.set_timeline(&projected) {
            self.notices.push(Message::error(e.to_string()));
        }
    }

    /// Decode at most one queued thumbnail, and only while the transport is
    /// idle.
    ///
    /// The backend has one playhead: pulling a thumbnail seeks it, so doing
    /// this during playback would fight the pacer for the decoder and stutter
    /// the picture. When it does run, the playhead is put back where it was.
    fn decode_one_thumbnail(&mut self, session: &Session) {
        let Some(req) = self.thumbnail_queue.take() else {
            return;
        };
        if self.transport.state() != TransportState::Stopped {
            return;
        }
        // Quarter-res is already small; the thumbnail is scaled down again to
        // a lane's height, so a decode never carries a full frame around.
        let decoded = self
            .backend
            .seek(req.at)
            .and_then(|()| self.backend.thumbnail_at(req.at, PreviewScale::Quarter));
        // A thumbnail that cannot be decoded leaves the clip drawn plain
        // rather than putting an error in the status line: the media may be
        // offline, which the clip's own colour already says.
        if let Ok(frame) = decoded {
            let thumb = crate::thumbnail::downscale(&frame, THUMBNAIL_HEIGHT, req.source);
            self.pending_thumbnails.push((req.clip, thumb));
        }
        // Put the decoder back where the user left it. A seek, not a
        // repaint: the composed preview frame is cached and a thumbnail pull
        // never touched it, so recomposing here would decode the playhead's
        // frame again on every tick a strip is filling in. The frame cache
        // keeps scales apart, so the preview run this walked over survives.
        let _ = self.backend.seek(session.timeline().playhead().frame);
    }

    /// Let the preview drop resolution when it stops keeping up, and take it
    /// back when it does.
    ///
    /// Scrubbing already trades resolution for keeping up; this is playback
    /// doing the same. It is not an edit and not a setting: the scale the
    /// user chose is restored the moment playback stops.
    fn follow_frame_budget(&mut self, session: &Session) {
        let change = if self.transport.is_playing() {
            self.adaptive.observe(self.presenter.stats(), self.scale)
        } else {
            self.adaptive.release(self.scale)
        };
        let Some(change) = change else { return };
        self.scale = change.scale;
        let at = session.timeline().playhead().frame;
        // A restart that fails leaves the pass running at the old scale,
        // which is a slower preview rather than a stopped one.
        match self
            .transport
            .rescale(self.backend.as_mut(), at, change.scale)
        {
            Ok(()) => self.notices.push(Message::info(change.message())),
            Err(e) => self.notices.push(Message::error(e)),
        }
    }

    /// Pull and compose the frame at the playhead. Only meaningful when the
    /// transport is idle: during playback the pacer owns the picture.
    fn show_playhead(&mut self, session: &Session) {
        if !self.preview {
            return;
        }
        if self.transport.state() != TransportState::Stopped {
            return;
        }
        let at = session.timeline().playhead().frame;
        if let Err(e) = self.backend.seek(at) {
            self.notices.push(Message::error(e.to_string()));
            return;
        }
        // A host that converts planar frames on the GPU never needs the
        // picture in RGBA: the decoder's own planes go straight to the card,
        // which skips both MLT's colour conversion and the presenter's blit.
        // A planar pull that fails is a slower frame, not a lost one: the
        // RGBA path below produces the same picture.
        if self.planar_preview
            && self.backend.supports_planar()
            && let Ok(frame) = self.backend.planar_frame_at(at, self.scale)
        {
            match self.presenter.present_planar(std::sync::Arc::new(frame)) {
                Ok(p) => {
                    self.last = Some(p);
                    return;
                }
                Err(e) => self.notices.push(Message::error(e.to_string())),
            }
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

/// The plugin seam: events out, requests in.
impl Editor {
    /// Fire an event at its handlers, keeping whatever edits they asked for
    /// until the next tick. Returns the veto, for a cancellable event.
    fn fire(&mut self, event: &davimci_lua::Event) -> Option<String> {
        let mut dispatch = self.plugins.dispatch(event);
        self.queued_requests.append(&mut dispatch.requests);
        for failure in &dispatch.failures {
            self.notices.push(Message::error(failure.message.clone()));
        }
        // Anything queued outside a handler - by config top level, or by a
        // handler through a module that queues rather than returns - waits
        // with the rest for the next tick. Dropping it here is what used to
        // lose a panel a config opened at load time.
        let (mut requests, messages) = self.plugins.take_requests();
        self.queued_requests.append(&mut requests);
        self.plugin_messages.extend(messages);
        dispatch.cancelled
    }

    /// The `BeforeExport` veto: the only event in v1 that can
    /// stop what it is reporting.
    fn before_export(&mut self, preset: &str, output: &str) -> Option<String> {
        self.last_export = Some((preset.to_string(), output.to_string()));
        self.fire(&davimci_lua::Event::BeforeExport {
            preset: preset.to_string(),
            output: output.to_string(),
        })
    }

    /// The preset `:export <path>` would infer, so the event payload names
    /// the same preset the render will use.
    fn preset_for(&self, path: &std::path::Path) -> String {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        self.exporter.presets().for_extension(&ext).name.clone()
    }

    /// Turn Lua requests into effects the app can run.
    ///
    /// Everything that needs the backend, the prober or the analysis is done
    /// here; everything that is an *edit* is handed back as an action, so it
    /// goes through the key engine and lands in the undo log as one step,
    /// exactly as the same action typed by hand would.
    fn run_requests(&mut self, requests: Vec<Request>, session: &mut Session) -> PluginEffects {
        let mut effects = PluginEffects::default();
        effects.messages.append(&mut self.plugin_messages);
        for request in requests {
            self.run_request(request, session, &mut effects);
        }
        effects
    }

    fn run_request(&mut self, request: Request, session: &mut Session, out: &mut PluginEffects) {
        match request {
            Request::Edit(action) => out.act(action),
            Request::Message(text) => out.say(Message::info(text)),
            // Routed as the `:` line it stands for, so a config-set property
            // is validated, applied and reported by exactly the code a typed
            // `:set` uses - including the undo rules, for the properties that
            // are edits.
            Request::Set { property, value } => {
                match self.command(&format!("set {property} {value}"), session, None) {
                    Ok(Some(msg)) => out.say(Message::info(msg)),
                    Ok(None) => {}
                    Err(e) => out.say(Message::error(e.to_string())),
                }
            }
            Request::Export { preset } => self.export_for_plugin(&preset, session, out),
            Request::Import { path } => out.report(self.import_picked(
                std::path::Path::new(&path),
                MediaIntent::Insert,
                session,
            )),
            Request::Analyze { track } => {
                let n = self.analyser.reanalyse();
                out.say(Message::info(match track {
                    Some(name) => format!("re-analysing {name}"),
                    None => format!("re-analysing {n} track(s)"),
                }));
            }
            Request::Motion { name, opts } => {
                self.move_by_plugin_motion(&name, &opts, session, out);
            }
            // A panel is view state: it goes to the app as an effect and
            // never near `Session::exec`.
            Request::Panel(request) => self.run_panel_request(request, out),
        }
    }

    /// Translate a Lua panel request into the app's own panel vocabulary.
    fn run_panel_request(&mut self, request: davimci_lua::PanelRequest, out: &mut PluginEffects) {
        use davimci_app::{PanelId, PanelOp};
        let id = PanelId(request.handle());
        let op = match request {
            davimci_lua::PanelRequest::Open { handle, spec } => {
                match spec.on_key {
                    Some(handler) => {
                        self.panel_handlers.insert(handle, handler);
                    }
                    None => {
                        self.panel_handlers.remove(&handle);
                    }
                }
                PanelOp::Open {
                    id,
                    spec: Box::new(davimci_app::PanelSpec {
                        owner: format!("lua:{handle}"),
                        title: spec.title,
                        anchor: panel_anchor(spec.anchor),
                        size: davimci_app::PanelSize {
                            columns: spec.columns,
                            rows: spec.rows,
                        },
                        z: spec.z,
                        focus: spec.focus,
                        on_key: spec.on_key,
                    }),
                }
            }
            davimci_lua::PanelRequest::SetContent { content, .. } => PanelOp::SetContent {
                id,
                content: panel_content(content),
            },
            davimci_lua::PanelRequest::Show(_) => PanelOp::Show(id),
            davimci_lua::PanelRequest::Hide(_) => PanelOp::Hide(id),
            davimci_lua::PanelRequest::Close(handle) => {
                self.panel_handlers.remove(&handle);
                PanelOp::Close(id)
            }
        };
        out.panel(op);
    }

    /// A plugin export goes to the preset's own default output, through the
    /// same guards a typed `:export` passes.
    fn export_for_plugin(&mut self, preset: &str, session: &Session, out: &mut PluginEffects) {
        let container = match self.exporter.presets().get(preset) {
            Ok(p) => p.container,
            Err(e) => return out.say(Message::error(e.to_string())),
        };
        let path = crate::export::default_output(
            self.workspace.current().path().map(std::path::Path::new),
            container,
        );
        if let Some(reason) = self.before_export(preset, &path.display().to_string()) {
            return out.say(Message::error(reason));
        }
        let shipped = match self.shipping_timeline(session) {
            Ok(tl) => tl,
            Err(e) => return out.say(Message::error(e.to_string())),
        };
        out.report(
            self.exporter
                .start(self.backend.as_mut(), &path, Some(preset), &shipped),
        );
    }

    /// A motion is a pure query: it answers a frame and the editor moves, so
    /// a plugin never touches the playhead itself.
    fn move_by_plugin_motion(
        &mut self,
        name: &str,
        opts: &davimci_lua::Opts,
        session: &mut Session,
        out: &mut PluginEffects,
    ) {
        let env = self.motion_env(session);
        match self.plugins.run_motion(name, opts, &env) {
            Ok(MotionAnswer::Found(frame)) => {
                let track = session.timeline().playhead().track;
                if let Err(e) = session.set_playhead(Frame(frame), track) {
                    out.say(Message::error(e.to_string()));
                }
            }
            Ok(MotionAnswer::NoMatch) => out.say(Message::warning(format!(
                "the motion '{name}' found nothing from here"
            ))),
            Ok(MotionAnswer::Pending) => out.say(Message::warning(format!(
                "analysis is still running; the motion '{name}' cannot be resolved yet"
            ))),
            // A motion nothing registered is usually one a bundled plugin
            // owns: say which, rather than leaving the user to guess that
            // the name exists at all.
            Err(davimci_lua::LuaError::NoSuchMotion(missing)) => {
                match crate::plugins::provider_of_motion(&missing) {
                    Some(owner) => out.say(Message::warning(format!(
                        "the motion '{missing}' comes from the bundled '{}' plugin; enable it in plugins.lua with require(\"davimci.plugins\").enable(\"{}\")",
                        owner.name(), owner.name()
                    ))),
                    None => out.say(Message::error(
                        davimci_lua::LuaError::NoSuchMotion(missing).to_string(),
                    )),
                }
            }
            Err(e) => out.say(Message::error(e.to_string())),
        }
    }

    /// The snapshot a registered motion runs against.
    ///
    /// A track with no analysis to wait for is reported as analysed: only an
    /// audio track whose measurement has not landed answers "not yet".
    fn motion_env(&self, session: &Session) -> MotionEnv {
        let tl = session.timeline();
        let head = tl.playhead();
        let focused = tl
            .track(head.track)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        let mut env = MotionEnv::new(head.frame.get(), focused);
        let fps = tl.props.fps;
        for track in tl.tracks() {
            let analysis = self.analyser.analysis(track.id);
            let mut data = TrackData {
                kind: match track.kind {
                    davimci_core::TrackKind::Video => "video",
                    davimci_core::TrackKind::Audio => "audio",
                    davimci_core::TrackKind::Text => "text",
                    davimci_core::TrackKind::Overlay => "overlay",
                }
                .to_string(),
                analysed: analysis.is_some() || track.kind != davimci_core::TrackKind::Audio,
                ..TrackData::default()
            };
            if let Some(a) = analysis {
                data.samples = a
                    .hops
                    .iter()
                    .enumerate()
                    .map(|(i, hop)| Sample {
                        frame: frame_of_ms(a.hop_start_ms(i), fps),
                        rms_db: f64::from(hop.rms_db),
                        peak_db: f64::from(hop.peak_db),
                    })
                    .collect();
                data.scene_changes = a
                    .scene_changes
                    .iter()
                    .map(|ms| frame_of_ms(*ms, fps))
                    .collect();
            }
            data.clip_bounds = track
                .clips()
                .iter()
                .flat_map(|c| [c.start.get(), c.end().get()])
                .collect();
            env = env.with_track(track.name.clone(), data);
        }
        env
    }

    /// Report what an edit did, as the v1 events.
    ///
    /// Insertions and deletions are diffed rather than read off the command,
    /// so undo, a macro replay and a plugin edit all report the same thing;
    /// a split is read off the command, because a diff cannot tell one from
    /// an insertion.
    fn report_edit(&mut self, session: &Session) {
        let tl = session.timeline();
        let now: std::collections::BTreeSet<(TrackId, ClipId)> = tl
            .tracks()
            .iter()
            .flat_map(|t| t.clips().iter().map(move |c| (t.id, c.id)))
            .collect();
        let name = |id: TrackId| tl.track(id).map_or_else(String::new, |t| t.name.clone());
        let mut events: Vec<davimci_lua::Event> = Vec::new();
        for (track, clip) in now.difference(&self.known_clips) {
            events.push(davimci_lua::Event::ClipInserted {
                clip: clip.0,
                track: name(*track),
            });
        }
        for (track, clip) in self.known_clips.difference(&now) {
            events.push(davimci_lua::Event::ClipDeleted {
                clip: clip.0,
                track: name(*track),
            });
        }
        for (track, frame) in session.last_edit().map(splits).unwrap_or_default() {
            events.push(davimci_lua::Event::SplitPerformed {
                frame: frame.get(),
                track: name(track),
            });
        }
        self.known_clips = now;
        for event in &events {
            self.fire(event);
        }
    }
}

/// What in a project asked for a bundled plugin, and what it costs when the
/// user has disabled that plugin anyway.
enum Need {
    /// A transition type only that plugin registers.
    Transition(String),
    /// A track of a kind that plugin is the workflow for, by track name.
    TrackKind(String),
}

impl Need {
    fn because(&self) -> String {
        match self {
            Self::Transition(kind) => format!("this project uses the transition '{kind}'"),
            Self::TrackKind(track) => format!("this project has the text track {track}"),
        }
    }

    fn cost(&self, owner: &str) -> String {
        match self {
            Self::Transition(_) => {
                format!("which the disabled '{owner}' plugin owns; it renders as a dissolve")
            }
            Self::TrackKind(_) => format!(
                "which the disabled '{owner}' plugin edits; its cues stay in the project but cannot be edited"
            ),
        }
    }
}

/// Every split inside a command, including the implicit ones a ripple edit
/// expands into.
fn splits(command: &EditCommand) -> Vec<(TrackId, Frame)> {
    match command {
        EditCommand::Split { track, frame, .. } => vec![(*track, *frame)],
        EditCommand::Sequence(inner) => inner.iter().flat_map(splits).collect(),
        _ => Vec::new(),
    }
}

/// A panel key as a Lua handler reads it: the spelling a config would use,
/// so a handler matches on `"j"` or `"<Esc>"`.
fn panel_key_name(key: davimci_app::ModalKey) -> String {
    use davimci_app::ModalKey;
    match key {
        ModalKey::Char(c) => c.to_string(),
        ModalKey::Escape => "<Esc>".to_string(),
        ModalKey::Enter => "<Enter>".to_string(),
        ModalKey::Backspace => "<BS>".to_string(),
        ModalKey::Tab => "<Tab>".to_string(),
        ModalKey::Left => "<Left>".to_string(),
        ModalKey::Right => "<Right>".to_string(),
        ModalKey::Up => "<Up>".to_string(),
        ModalKey::Down => "<Down>".to_string(),
    }
}

fn panel_anchor(anchor: davimci_lua::PanelAnchor) -> davimci_app::PanelAnchor {
    use davimci_lua::PanelAnchor as From;
    match anchor {
        From::Center => davimci_app::PanelAnchor::Center,
        From::TopLeft => davimci_app::PanelAnchor::TopLeft,
        From::TopRight => davimci_app::PanelAnchor::TopRight,
        From::BottomLeft => davimci_app::PanelAnchor::BottomLeft,
        From::BottomRight => davimci_app::PanelAnchor::BottomRight,
        From::Playhead => davimci_app::PanelAnchor::Playhead,
    }
}

fn panel_content(content: davimci_lua::PanelContent) -> davimci_app::PanelContent {
    match content {
        davimci_lua::PanelContent::Lines(lines) => davimci_app::PanelContent::Lines(
            lines
                .into_iter()
                .map(|line| davimci_app::PanelLine {
                    spans: line
                        .spans
                        .into_iter()
                        .map(|s| davimci_app::PanelSpan::new(s.text, panel_role(s.role)))
                        .collect(),
                })
                .collect(),
        ),
        davimci_lua::PanelContent::Picture {
            width,
            height,
            rgba,
        } => davimci_app::PanelContent::Pixels {
            width,
            height,
            rgba: std::sync::Arc::new(rgba),
        },
    }
}

fn panel_role(role: davimci_lua::PanelRole) -> davimci_app::PanelRole {
    use davimci_lua::PanelRole as From;
    match role {
        From::Normal => davimci_app::PanelRole::Normal,
        From::Key => davimci_app::PanelRole::Key,
        From::Accent => davimci_app::PanelRole::Accent,
        From::Warning => davimci_app::PanelRole::Warning,
    }
}

/// A source millisecond as a timeline frame.
fn frame_of_ms(ms: u64, fps: davimci_core::Fps) -> u64 {
    ms.saturating_mul(u64::from(fps.num)) / (1000 * u64::from(fps.den).max(1))
}

impl Host for Editor {
    fn import_media(
        &mut self,
        path: &std::path::Path,
        intent: MediaIntent,
        session: &mut Session,
    ) -> Result<Option<String>, AppError> {
        self.import_picked(path, intent, session)
            .map(Some)
            .map_err(|e| AppError::UnhandledCommand(e.to_string()))
    }

    fn jobs(&mut self) -> Vec<JobUpdate> {
        std::mem::take(&mut self.job_updates)
    }

    fn take_confirms(&mut self) -> Vec<davimci_app::Confirm> {
        if self.trust_asked {
            return Vec::new();
        }
        let Some(path) = self.pending_trust.clone() else {
            return Vec::new();
        };
        self.trust_asked = true;
        vec![davimci_app::Confirm::new(
            TRUST_CONFIRM,
            format!(
                "{} wants to run project-local config. Trust it? [y/N]",
                path.display()
            ),
        )]
    }

    /// The answer to the project-local config question.
    ///
    /// A no leaves the file unread, and says so once: silence would look
    /// like the config had loaded. A yes runs it restricted and rebuilds the
    /// keymap, since a binding that is not in the table is not a binding.
    fn confirmed(&mut self, id: davimci_app::ConfirmId, granted: bool, _session: &mut Session) {
        if id != davimci_app::ConfirmId(TRUST_CONFIRM) {
            return;
        }
        let Some(path) = self.pending_trust.take() else {
            return;
        };
        if !granted {
            self.notices.push(Message::warning(format!(
                "{} was not loaded: project-local config runs only when trusted",
                path.display()
            )));
            return;
        }
        let dir = path.parent().map_or_else(
            || self.workspace.root().to_path_buf(),
            std::path::Path::to_path_buf,
        );
        self.plugins.grant_project_local(&dir);
        self.install_registrations();
        self.pending_keymap = Some(self.plugins.keymap());
    }

    fn take_keymap(&mut self) -> Option<davimci_keys::Keymap> {
        self.pending_keymap.take()
    }

    fn command_vocabulary(&mut self, session: &Session) -> Option<davimci_app::CommandVocabulary> {
        Some(crate::excmd::vocabulary_with(
            &self.current_settings(session),
        ))
    }

    fn plugin(&mut self, id: u32, session: &mut Session) -> Result<PluginEffects, AppError> {
        let (requests, messages) = self.plugins.invoke(id);
        self.plugin_messages.extend(messages);
        Ok(self.run_requests(requests, session))
    }

    fn plugin_tick(&mut self, session: &mut Session) -> PluginEffects {
        let (mut requests, messages) = self.plugins.take_requests();
        self.plugin_messages.extend(messages);
        // Whatever an event handler queued since the last tick runs first:
        // it was asked for earlier.
        let mut queued = std::mem::take(&mut self.queued_requests);
        queued.append(&mut requests);
        if queued.is_empty() && self.plugin_messages.is_empty() {
            return PluginEffects::default();
        }
        self.run_requests(queued, session)
    }

    fn key_pending(&mut self, pending: &davimci_keys::Pending, _session: &mut Session) {
        self.fire(&davimci_lua::Event::KeyPending {
            mode: pending.mode.name().to_string(),
            keys: pending.text.clone(),
            continuations: pending
                .continuations
                .iter()
                .map(|c| davimci_lua::Continuation {
                    key: davimci_keys::docs::render(std::slice::from_ref(&c.key)),
                    description: c
                        .leaf
                        .as_ref()
                        .map(davimci_keys::docs::describe_leaf)
                        .unwrap_or_default(),
                    group: c.leaf.is_none(),
                })
                .collect(),
        });
    }

    fn panel_key(
        &mut self,
        panel: davimci_app::PanelId,
        key: davimci_app::ModalKey,
        session: &mut Session,
    ) -> PluginEffects {
        let Some(handler) = self.panel_handlers.get(&panel.get()).copied() else {
            return PluginEffects::default();
        };
        let requests = self
            .plugins
            .invoke_key(handler, &panel_key_name(key))
            .unwrap_or_default();
        let messages = self.plugins.take_requests().1;
        self.plugin_messages.extend(messages);
        self.run_requests(requests, session)
    }

    fn mode_changed(&mut self, from: davimci_keys::Mode, to: davimci_keys::Mode) {
        self.fire(&davimci_lua::Event::ModeChanged {
            from: from.name().to_string(),
            to: to.name().to_string(),
        });
    }

    fn waveforms(&mut self) -> Vec<(TrackId, Waveform)> {
        std::mem::take(&mut self.pending_waveforms)
    }

    fn stale_waveforms(&mut self) -> Vec<TrackId> {
        self.analyser.take_stale()
    }

    fn take_visual_start(&mut self) -> Option<davimci_keys::VisualStart> {
        self.pending_visual_start.take()
    }

    fn take_text_editing(&mut self) -> Option<bool> {
        self.pending_text_editing.take()
    }

    fn command(
        &mut self,
        line: &str,
        session: &mut Session,
        selection: Option<&Selection>,
    ) -> Result<Option<String>, AppError> {
        // Export and audio commands never reach the workspace: it has
        // neither a backend nor the analysis.
        if let Ok(cmd) = crate::excmd::parse(line)
            && let Some(result) = self
                .plugin_command(&cmd)
                .or_else(|| self.export_command(&cmd, session))
                .or_else(|| self.audio_command(&cmd, session, selection))
        {
            return match result {
                Ok(msg) => Ok(Some(msg)),
                Err(e) => Err(AppError::UnhandledCommand(e.to_string())),
            };
        }
        // The app holds the live session; give it to the workspace so the
        // command acts on what the user can see.
        self.workspace.set_current_session(session.clone());
        let buffer_before = self.workspace.current().id();
        let outcome = self
            .workspace
            .run_selected(line, OnRecovery::Discard, selection);
        // Take back whatever buffer is now current - possibly a different
        // timeline entirely. A swap is a *different buffer*, not a different
        // timeline: replacing the session resets the viewport, so a command
        // that merely edits or moves the playhead (`:1234`) must not look
        // like one, or the user's zoom and scroll are thrown away.
        let after = self.workspace.current_session();
        if self.workspace.current().id() != buffer_before {
            self.swap = Some(after.clone());
        }
        *session = after;
        // A command may have opened a project written with plugins this
        // session has not run.
        self.activate_plugins_for(session);
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

    fn resolve_object(
        &mut self,
        name: char,
        around: bool,
        session: &Session,
    ) -> Result<Option<davimci_motion::TimeRange>, AppError> {
        let tl = session.timeline();
        let head = tl.playhead();
        let clip = tl
            .track(head.track)
            .and_then(|t| t.clip_at(head.frame))
            .ok_or_else(|| {
                AppError::UnhandledCommand(
                    "there is no clip under the playhead for that text object".into(),
                )
            })?;
        let with_transitions = tl
            .track(head.track)
            .and_then(|t| t.transition_range(clip.id))
            .unwrap_or((clip.start, clip.end()));
        let info = davimci_lua::ClipInfo {
            start: clip.start.get(),
            end: clip.end().get(),
            with_transitions_start: with_transitions.0.get(),
            with_transitions_end: with_transitions.1.get(),
        };
        let form = if around {
            davimci_lua::ObjectForm::Around
        } else {
            davimci_lua::ObjectForm::Inner
        };
        self.plugins
            .run_object(&name.to_string(), form, info)
            .map(|range| range.map(|(s, e)| davimci_motion::TimeRange::new(Frame(s), Frame(e))))
            .map_err(|e| AppError::UnhandledCommand(e.to_string()))
    }

    fn selection_changed(&mut self, selection: Option<&Selection>) {
        // A loop follows the selection it was set on; losing the selection
        // ends it rather than leaving playback wrapping over nothing.
        let gone =
            selection.is_none_or(|s| s.clips(self.workspace.current().timeline()).is_empty());
        if gone && self.transport.clear_loop() {
            self.notices
                .push(Message::info("loop ended: the selection is gone"));
        }
    }

    fn transport(&mut self, cmd: TransportCmd, selection: Option<&Selection>) {
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
                    .shuttle(true, self.backend.as_mut(), &session, self.scale)
            }
            TransportCmd::ShuttleBackward => {
                self.transport
                    .shuttle(false, self.backend.as_mut(), &session, self.scale)
            }
            TransportCmd::ShuttleStop => {
                self.transport.shuttle_stop(self.backend.as_mut(), &session)
            }
            // An explicit `interrupt_transport` bind is the same code path
            // the implicit interrupt takes, so the two cannot drift.
            TransportCmd::Interrupt => {
                self.interrupt_transport(&session);
                return;
            }
            TransportCmd::LoopSelection => match loop_range(&session, selection) {
                Some(range) => self.transport.loop_range_start(
                    self.backend.as_mut(),
                    &session,
                    self.scale,
                    range,
                ),
                None => Err("there is nothing here to loop.".into()),
            },
        };
        match result {
            Ok(msg) => self.notices.push(Message::info(msg)),
            Err(msg) => self.notices.push(Message::error(msg)),
        }
    }

    fn interrupt_transport(&mut self, session: &Session) {
        match self.transport.interrupt(self.backend.as_mut()) {
            // Only repaint when something was actually running: the guard in
            // `show_playhead` has just been released, and the app's own
            // `playhead_moved` may not follow (an interrupt bind is not a
            // motion).
            Ok(true) => {
                self.notices.push(Message::info("paused".to_string()));
                self.show_playhead(session);
            }
            Ok(false) => {}
            Err(e) => self.notices.push(Message::error(e)),
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
        self.poll_export();
        // Analysis finishes on its own thread; this is where its results
        // cross back onto the editor's.
        let (updates, waves) = self.analyser.poll();
        self.job_updates.extend(updates);
        self.pending_waveforms.extend(waves);
        self.adopt_finished_proxies(session);
        if let Some(frame) = result.playhead {
            let track = session.timeline().playhead().track;
            // Playback moves the playhead; that is navigation, never an edit.
            if let Err(e) = session.set_playhead(frame, track) {
                self.notices.push(Message::error(e.to_string()));
            }
        }
        // One tick, one frame: the transport already composed it.
        if let Some(p) = result.presentation.filter(|_| self.preview) {
            self.last = Some(p);
        }
        self.follow_frame_budget(session);
        if result.stopped {
            self.notices.push(Message::info("stopped".to_string()));
            // Show the frame we landed on, now that the pacer has let go.
            let snapshot = session.clone();
            self.show_playhead(&snapshot);
        }
        let snapshot = session.clone();
        self.decode_one_thumbnail(&snapshot);
    }

    fn request_thumbnails(&mut self, wanted: &[ThumbnailRequest]) {
        // Only the first is kept: the app orders them by distance from the
        // playhead and asks again every tick, so the queue re-forms around
        // wherever the user has since looked.
        self.thumbnail_queue = wanted.first().copied();
    }

    fn thumbnails(&mut self) -> Vec<(davimci_core::ClipId, Thumbnail)> {
        std::mem::take(&mut self.pending_thumbnails)
    }

    fn timeline_changed(&mut self, session: &Session) {
        self.report_edit(session);
        self.project(session);
        // Analysis follows the timeline: a new audio track is queued, and a
        // gain or fade change invalidates what was measured before it
        //.
        self.analyser.sync(session.timeline());
    }

    fn playhead_moved(&mut self, session: &Session) {
        let head = session.timeline().playhead();
        let track = session
            .timeline()
            .track(head.track)
            .map_or_else(String::new, |t| t.name.clone());
        self.fire(&davimci_lua::Event::PlayheadMoved {
            frame: head.frame.get(),
            track,
        });
        // A loop survives a seek inside its range and ends on one outside it.
        if self.transport.playhead_moved(head.frame) {
            self.notices
                .push(Message::info("loop ended: the playhead left the loop"));
        }
        self.show_playhead(session);
    }

    fn wants_quit(&self) -> bool {
        self.quit
    }
}

/// What `<Space>l` loops: the selection's span, or the clip under the
/// playhead in `NORMAL`.
fn loop_range(session: &Session, selection: Option<&Selection>) -> Option<(Frame, Frame)> {
    let tl = session.timeline();
    if let Some(sel) = selection {
        let clips = sel.clips(tl);
        let start = clips.iter().map(|(_, c)| c.start).min()?;
        let end = clips.iter().map(|(_, c)| c.end()).max()?;
        return (end > start).then_some((start, end));
    }
    let head = tl.playhead();
    let clip = tl.track(head.track)?.clip_at(head.frame)?;
    Some((clip.start, clip.end()))
}

/// The frame a fresh editor should show: frame zero of the current timeline.
#[must_use]
pub fn first_frame() -> Frame {
    Frame::ZERO
}
