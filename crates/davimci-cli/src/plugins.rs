//! The Lua runtime, wired to the editor.
//!
//! `davimci-lua` deliberately knows nothing about backends, files or
//! frontends: it registers what a config asked for and queues requests. This
//! is the layer that gives those requests somewhere to go - keymaps into
//! `davimci-keys`, presets into the export registry, events out of the
//! editor, and every edit back through `Session::exec`.

use std::path::Path;

use davimci_app::{Message, Severity};
use davimci_backend::{AudioCodec, Container, Preset, SubtitleMode, TrackSelection, VideoCodec};
use davimci_core::{ErrorClass, Notice};
use davimci_keys::Keymap;
use davimci_lua::{
    ConfigPaths, Dispatch, Event, LuaError, MotionEnv, Request, Runtime, TimelineConfig, Trust,
    TrustPrompt,
};

/// The runtime plus whatever loading the config had to say about itself.
pub struct Plugins {
    runtime: Runtime,
    notices: Vec<Notice>,
}

impl std::fmt::Debug for Plugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugins")
            .field("notices", &self.notices.len())
            .finish_non_exhaustive()
    }
}

/// The plugins every build ships with, run before any user config so a
/// config can rebind or replace what they set up.
///
/// They use the same `davimci.*` surface a third-party plugin does: if a
/// bundled plugin needs something the API cannot express, that is a gap in
/// the API rather than a reason to special-case it.
const BUNDLED: &[(&str, &str)] = &[(
    "which-key.lua",
    include_str!("../runtime/plugins/which-key.lua"),
)];

impl Plugins {
    /// A runtime with no user config loaded. Every build has one, so the Lua
    /// path is never a special case the tests can skip.
    #[must_use]
    pub fn empty() -> Self {
        match Runtime::new() {
            Ok(runtime) => Self {
                runtime,
                notices: Vec::new(),
            },
            // A runtime that cannot even be created costs the user their
            // plugins, not their editor (Phase 0: degrade locally).
            Err(e) => Self {
                runtime: no_runtime(),
                notices: vec![Notice::from_error(&e)],
            },
        }
    }

    /// Load `~/.config/davimci/`, then the project-local `.davimci.lua` if
    /// the user trusts it. Every failure is a notice; none is fatal.
    #[must_use]
    pub fn load(paths: Option<&ConfigPaths>, project_dir: &Path, trust: &dyn TrustPrompt) -> Self {
        let mut plugins = Self::empty();
        plugins.load_bundled();
        if let Some(paths) = paths {
            plugins.notices.extend(plugins.runtime.load_config(paths));
        }
        let (_, notice) = plugins.runtime.load_project_local(project_dir, trust);
        plugins.notices.extend(notice);
        plugins
    }

    /// Run the bundled plugins. A bundled plugin that fails is a notice like
    /// any other: the editor keeps working without it.
    pub fn load_bundled(&mut self) {
        for (name, source) in BUNDLED {
            if let Err(e) = self
                .runtime
                .exec(source, name, davimci_lua::Sandbox::Trusted)
            {
                self.notices.push(Notice::from_error(&e));
            }
        }
    }

    /// Startup notices, as status-line messages. Drained once by the host.
    pub fn take_notices(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.notices)
            .iter()
            .map(notice_message)
            .collect()
    }

    /// The default table with the user's bindings layered over it, so a
    /// config wins over a default without either knowing about the other.
    #[must_use]
    pub fn keymap(&self) -> Keymap {
        let mut keymap = Keymap::new().with_overrides(self.runtime.keymap_overrides());
        // A registered object has to be typeable, or `dic` for a config's
        // `c` would be an unbound sequence.
        for name in self.runtime.object_names() {
            keymap.register_object(&name);
        }
        keymap
    }

    #[must_use]
    pub fn timeline_config(&self) -> TimelineConfig {
        self.runtime.timeline_config()
    }

    #[must_use]
    pub fn motion_names(&self) -> Vec<String> {
        self.runtime.motion_names()
    }

    #[must_use]
    pub fn object_names(&self) -> Vec<String> {
        self.runtime.object_names()
    }

    /// Every transition type the config registered, in backend terms
    ///.
    #[must_use]
    pub fn transitions(&self) -> Vec<davimci_backend::TransitionDef> {
        self.runtime
            .transitions()
            .into_iter()
            .map(|t| davimci_backend::TransitionDef {
                name: t.name,
                service: t.service,
                props: t.props,
            })
            .collect()
    }

    /// Resolve a registered text object against one clip.
    pub fn run_object(
        &self,
        name: &str,
        form: davimci_lua::ObjectForm,
        clip: davimci_lua::ClipInfo,
    ) -> Result<Option<(u64, u64)>, davimci_lua::LuaError> {
        self.runtime.run_object(name, form, clip)
    }

    /// Every Lua-defined preset, translated for the export registry. A
    /// preset that cannot be translated comes back as a notice instead, so
    /// the user hears about it at load time rather than after a long render
    ///.
    #[must_use]
    pub fn presets(&self) -> (Vec<Preset>, Vec<Message>) {
        let mut presets = Vec::new();
        let mut problems = Vec::new();
        for name in self.runtime.preset_names() {
            match self.runtime.preset(&name).and_then(|p| convert_preset(&p)) {
                Ok(p) => presets.push(p),
                Err(e) => problems.push(notice_message(&Notice::from_error(&e))),
            }
        }
        (presets, problems)
    }

    /// Run a keymap callback and collect what it asked for. A callback that
    /// throws is already disabled by the runtime; its notice comes back here.
    pub fn invoke(&mut self, id: u32) -> (Vec<Request>, Vec<Message>) {
        let requests = self.runtime.invoke(id).unwrap_or_default();
        (requests, self.drain_notices())
    }

    /// Hand one keystroke to a focused panel's `on_key` handler. A handler
    /// that throws is already disabled by the runtime; its notice comes back
    /// on the next drain, like every other plugin failure.
    pub fn invoke_key(&mut self, id: u32, key: &str) -> Result<Vec<Request>, LuaError> {
        self.runtime.invoke_key(id, key)
    }

    /// Requests queued outside a callback, drained every tick.
    pub fn take_requests(&mut self) -> (Vec<Request>, Vec<Message>) {
        (self.runtime.take_requests(), self.drain_notices())
    }

    /// Fire an editor event at its handlers.
    pub fn dispatch(&mut self, event: &Event) -> Dispatch {
        self.runtime.dispatch(event)
    }

    /// Resolve a registered motion against a snapshot.
    pub fn run_motion(
        &mut self,
        name: &str,
        opts: &davimci_lua::Opts,
        env: &MotionEnv,
    ) -> Result<davimci_lua::MotionAnswer, LuaError> {
        self.runtime.run_motion(name, opts, env)
    }

    fn drain_notices(&mut self) -> Vec<Message> {
        self.runtime
            .take_notices()
            .iter()
            .map(notice_message)
            .collect()
    }
}

/// A runtime that could not be created still has to be *something*: this one
/// registers nothing, so every query answers empty and nothing else needs a
/// null check. Only reachable if `mlua` itself fails to start.
fn no_runtime() -> Runtime {
    #[allow(clippy::expect_used)]
    Runtime::new().expect("a second attempt at a bare Lua state")
}

fn notice_message(notice: &Notice) -> Message {
    let severity = match notice.class {
        ErrorClass::User => Severity::Warning,
        _ => Severity::Error,
    };
    Message {
        severity,
        text: notice.text.clone(),
    }
}

/// Translate a validated Lua preset into the one the exporter runs.
///
/// `davimci-lua` validates a preset where it is defined and has its own
/// vocabulary for containers and codecs; the backend has the registry. This
/// is the one place the two are matched up, so a name accepted by one and
/// not the other is a load-time error rather than a render-time surprise.
fn convert_preset(p: &davimci_lua::ExportPreset) -> Result<Preset, LuaError> {
    let fail = |e: davimci_backend::PresetError| {
        LuaError::Config(format!("export preset '{}': {e}", p.name))
    };
    let container = Container::parse(&p.container).map_err(fail)?;
    let video = VideoCodec::parse(&p.video_codec).map_err(fail)?;
    let audio = AudioCodec::parse(&p.audio_codec).map_err(fail)?;
    let mut preset = Preset::new(p.name.clone(), container, video, audio).map_err(fail)?;
    preset.resolution = p.resolution;
    preset.fps = p.fps;
    preset.audio_tracks = match &p.audio_tracks {
        davimci_lua::TrackSelection::All => TrackSelection::All,
        davimci_lua::TrackSelection::None => TrackSelection::None,
        davimci_lua::TrackSelection::Named(n) => TrackSelection::Named(n.clone()),
    };
    preset.subtitles = match &p.subtitle_tracks {
        // Named subtitle tracks are a selection, not a mode; burning them is
        // what naming one has always meant in a preset.
        davimci_lua::SubtitleSelection::Burned | davimci_lua::SubtitleSelection::Named(_) => {
            SubtitleMode::Burned
        }
        davimci_lua::SubtitleSelection::Sidecar => SubtitleMode::Sidecar,
        davimci_lua::SubtitleSelection::Embedded => SubtitleMode::Embedded,
        davimci_lua::SubtitleSelection::None => SubtitleMode::None,
    };
    Ok(preset)
}

/// Asks on the terminal, and refuses when there is nobody to ask.
#[derive(Debug, Clone, Copy, Default)]
pub struct AskOnTerminal;

impl TrustPrompt for AskOnTerminal {
    fn trust(&self, path: &Path) -> Trust {
        use std::io::{IsTerminal, Write};
        if !std::io::stdin().is_terminal() {
            return Trust::Denied;
        }
        print!(
            "{} wants to run project-local config. Trust it? [y/N] ",
            path.display()
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return Trust::Denied;
        }
        match answer.trim() {
            "y" | "Y" | "yes" => Trust::Granted,
            _ => Trust::Denied,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_lua::DenyAll;

    fn scratch(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("davimci-plugins-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_lua_preset_reaches_the_export_registry() {
        let root = scratch("preset");
        std::fs::write(
            root.join("init.lua"),
            r#"require("davimci.export").preset("yt", {
                 container = "mp4", video_codec = "h264", audio_codec = "aac",
                 resolution = "1920x1080",
               })"#,
        )
        .unwrap();
        let plugins = Plugins::load(Some(&ConfigPaths::new(&root)), &root, &DenyAll);
        let (presets, problems) = plugins.presets();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "yt");
        assert_eq!(presets[0].container, Container::Mp4);
        assert_eq!(
            presets[0].resolution,
            Some(davimci_core::Resolution {
                width: 1920,
                height: 1080
            })
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_broken_config_file_costs_that_file_and_nothing_else() {
        let root = scratch("broken");
        std::fs::write(root.join("init.lua"), "this is not lua(").unwrap();
        std::fs::write(
            root.join("keymaps.lua"),
            r#"require("davimci.keymap").map("normal", "gz", "editor.split_at_playhead")"#,
        )
        .unwrap();
        let mut plugins = Plugins::load(Some(&ConfigPaths::new(&root)), &root, &DenyAll);
        let notices = plugins.take_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        // The keymap after the broken file still loaded.
        let keymap = plugins.keymap();
        let keys = davimci_keys::Key::parse_str("gz");
        assert!(
            !matches!(keymap.lookup(&keys), davimci_keys::keymap::Lookup::NoMatch),
            "the binding from the surviving file is missing"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_untrusted_project_local_config_is_reported_and_not_run() {
        let root = scratch("untrusted");
        std::fs::write(
            root.join(".davimci.lua"),
            r#"require("davimci.keymap").map("normal", "gz", "editor.undo")"#,
        )
        .unwrap();
        let mut plugins = Plugins::load(None, &root, &DenyAll);
        let notices = plugins.take_notices();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].text.contains("trusted"), "{notices:?}");
        assert!(matches!(
            plugins.keymap().lookup(&davimci_keys::Key::parse_str("gz")),
            davimci_keys::keymap::Lookup::NoMatch
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_preset_the_backend_cannot_build_becomes_a_notice() {
        // `davimci-lua` accepts `pcm` in a mov; a pairing the backend refuses
        // must still be reported at load time, never at render time.
        let root = scratch("badpreset");
        std::fs::write(
            root.join("init.lua"),
            r#"require("davimci.export").preset("odd", {
                 container = "mkv", video_codec = "h264", audio_codec = "flac",
               })"#,
        )
        .unwrap();
        let plugins = Plugins::load(Some(&ConfigPaths::new(&root)), &root, &DenyAll);
        let (presets, problems) = plugins.presets();
        assert_eq!(presets.len(), 1, "{problems:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
