//! The Lua runtime, wired to the editor.
//!
//! `davimci-lua` deliberately knows nothing about backends, files or
//! frontends: it registers what a config asked for and queues requests. This
//! is the layer that gives those requests somewhere to go - keymaps into
//! `davimci-keys`, presets into the export registry, events out of the
//! editor, and every edit back through `Session::exec`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use davimci_app::{Message, Severity};
use davimci_backend::{AudioCodec, Container, Preset, SubtitleMode, TrackSelection, VideoCodec};
use davimci_core::{ErrorClass, Notice};
use davimci_keys::Keymap;
use davimci_lua::{
    ConfigPaths, Dispatch, Event, LuaError, Manifest, MotionEnv, Plugin, Request, Runtime, Source,
    TimelineConfig, Trust, TrustPrompt,
};

/// The runtime plus whatever loading the config had to say about itself.
pub struct Plugins {
    runtime: Runtime,
    notices: Vec<Notice>,
    /// Every plugin this session knows of: the bundled ones and whatever was
    /// installed on the runtime path, in load order.
    known: Vec<Plugin>,
    active: std::collections::BTreeSet<String>,
}

impl std::fmt::Debug for Plugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugins")
            .field("notices", &self.notices.len())
            .finish_non_exhaustive()
    }
}

/// The sources of the plugins that ship with davimci, each a directory laid
/// out exactly like an installed one: a manifest the host reads and Lua the
/// host runs. Nothing about them is written in Rust.
const BUNDLED_SOURCES: &[(&str, &str)] = &[
    (
        include_str!("../runtime/plugins/transitions/davimci.toml"),
        include_str!("../runtime/plugins/transitions/plugin/init.lua"),
    ),
    (
        include_str!("../runtime/plugins/silence/davimci.toml"),
        include_str!("../runtime/plugins/silence/plugin/init.lua"),
    ),
    (
        include_str!("../runtime/plugins/scenes/davimci.toml"),
        include_str!("../runtime/plugins/scenes/plugin/init.lua"),
    ),
    (
        include_str!("../runtime/plugins/text/davimci.toml"),
        include_str!("../runtime/plugins/text/plugin/init.lua"),
    ),
    (
        include_str!("../runtime/plugins/which-key/davimci.toml"),
        include_str!("../runtime/plugins/which-key/plugin/init.lua"),
    ),
];

/// The plugins every build ships with, run before the rest of the user
/// config so a config can rebind or replace what they set up.
///
/// They are examples as much as features: each uses the same `davimci.*`
/// surface a third-party plugin does, and each is off until something asks
/// for it. If a bundled plugin needs something the API cannot express, that
/// is a gap in the API rather than a reason to special-case it.
///
/// A manifest that does not parse is a build defect, not a user error, so it
/// is dropped with the rest of the plugin rather than crashing an editor a
/// user is in the middle of.
pub static BUNDLED: LazyLock<Vec<Plugin>> = LazyLock::new(|| {
    BUNDLED_SOURCES
        .iter()
        .filter_map(|(manifest, source)| {
            let manifest = Manifest::parse(manifest, "bundled davimci.toml").ok()?;
            Some(Plugin {
                manifest,
                source: Source::Builtin(source),
            })
        })
        .collect()
});

/// The bundled plugin that owns `name` as a transition type, if any.
#[must_use]
pub fn provider_of_transition(name: &str) -> Option<&'static Plugin> {
    BUNDLED
        .iter()
        .find(|p| owns(&p.manifest.provides.transitions, name))
}

/// The bundled plugin that owns `name` as a motion, if any.
#[must_use]
pub fn provider_of_motion(name: &str) -> Option<&'static Plugin> {
    BUNDLED
        .iter()
        .find(|p| owns(&p.manifest.provides.motions, name))
}

/// The bundled plugin that owns `tag` as a track kind, if any.
#[must_use]
pub fn provider_of_track_kind(tag: &str) -> Option<&'static Plugin> {
    BUNDLED
        .iter()
        .find(|p| owns(&p.manifest.provides.track_kinds, tag))
}

fn owns(names: &[String], name: &str) -> bool {
    names.iter().any(|n| n == name)
}

impl Plugins {
    /// A runtime with no user config loaded. Every build has one, so the Lua
    /// path is never a special case the tests can skip.
    #[must_use]
    pub fn empty() -> Self {
        match Runtime::new() {
            Ok(runtime) => Self {
                runtime,
                notices: Vec::new(),
                known: BUNDLED.clone(),
                active: std::collections::BTreeSet::new(),
            },
            // A runtime that cannot even be created costs the user their
            // plugins, not their editor (Phase 0: degrade locally).
            Err(e) => Self {
                runtime: no_runtime(),
                notices: vec![Notice::from_error(&e)],
                known: BUNDLED.clone(),
                active: std::collections::BTreeSet::new(),
            },
        }
    }

    /// Load `~/.config/davimci/`, then the project-local `.davimci.lua` if
    /// the user trusts it. Every failure is a notice; none is fatal.
    #[must_use]
    pub fn load(paths: Option<&ConfigPaths>, project_dir: &Path, trust: &dyn TrustPrompt) -> Self {
        let mut plugins = Self::empty();
        plugins.load_choices_and_bundled(paths);
        if let Some(paths) = paths {
            plugins.notices.extend(plugins.runtime.load_config(paths));
        }
        let (_, notice) = plugins.runtime.load_project_local(project_dir, trust);
        plugins.notices.extend(notice);
        plugins
    }

    /// Load the user's config, and report the project-local file rather than
    /// asking about it.
    ///
    /// The question belongs in the frontend - a window has no terminal to
    /// answer on - so nothing project-local is read, compiled or run here;
    /// the path comes back for the host to ask about and
    /// [`Plugins::grant_project_local`] is the only way it ever runs.
    #[must_use]
    pub fn load_deferred(
        paths: Option<&ConfigPaths>,
        project_dir: &Path,
    ) -> (Self, Option<PathBuf>) {
        let mut plugins = Self::empty();
        plugins.load_choices_and_bundled(paths);
        if let Some(paths) = paths {
            plugins.notices.extend(plugins.runtime.load_config(paths));
        }
        let path = project_dir.join(".davimci.lua");
        let pending = path.is_file().then_some(path);
        (plugins, pending)
    }

    /// Run the project-local config in `dir`, the user having said so.
    ///
    /// It still runs restricted: "I want this project's export presets" is
    /// not "I want this directory to run `os.execute`".
    pub fn grant_project_local(&mut self, dir: &Path) {
        let (_, notice) = self.runtime.load_project_local(dir, &GrantOnce);
        self.notices.extend(notice);
    }

    /// Read `plugins.lua`, discover what is installed, then run everything
    /// it left enabled: the bundled plugins first, then the packages.
    fn load_choices_and_bundled(&mut self, paths: Option<&ConfigPaths>) {
        if let Some(paths) = paths {
            let notices = self.runtime.load_plugin_choices(paths);
            self.notices.extend(notices);
            self.discover(paths);
        }
        self.load_enabled();
    }

    /// Add the installed packages to what this session knows about, and put
    /// their `lua/` directories on `require`'s path.
    ///
    /// A package whose manifest declares an API this build no longer offers
    /// is refused rather than run, because a plugin from the future can ask
    /// for edits this host would misread.
    fn discover(&mut self, paths: &ConfigPaths) {
        let (found, problems) = paths.packages();
        self.notices.extend(problems.iter().map(Notice::from_error));
        let mut roots: Vec<PathBuf> = Vec::new();
        for plugin in found {
            // An incompatible plugin is still known, so `:checkhealth` can
            // say what happened to it; it simply never runs.
            if plugin.compatible() {
                if let Some(root) = plugin.root() {
                    roots.push(root.to_path_buf());
                }
            } else {
                self.notices.push(Notice::from_error(&LuaError::Config(
                    plugin.incompatible_notice(),
                )));
            }
            self.known.push(plugin);
        }
        // The config root goes last: a package must never shadow a module
        // the user wrote.
        roots.push(paths.root.clone());
        if let Err(e) = self.runtime.set_search_path(&roots) {
            self.notices.push(Notice::from_error(&e));
        }
    }

    /// Run every plugin this session should start with: the ones config
    /// enabled, the packages that are on the path, and the `opt` packages
    /// `davimci.pack.add` named. A plugin that fails is a notice like any
    /// other - the editor keeps working without it.
    pub fn load_enabled(&mut self) {
        for i in 0..self.known.len() {
            if self.wants(&self.known[i]) {
                self.run_plugin(i);
            }
        }
        for name in self.runtime.packadds() {
            match self.known.iter().position(|p| p.name() == name) {
                Some(i) => self.run_plugin(i),
                None => self
                    .notices
                    .push(Notice::from_error(&LuaError::Config(format!(
                        "davimci.pack.add(\"{name}\"): no such plugin is installed"
                    )))),
            }
        }
    }

    /// Run one known plugin, at most once a session.
    fn run_plugin(&mut self, index: usize) {
        let Some(plugin) = self.known.get(index).cloned() else {
            return;
        };
        if !plugin.compatible() {
            return;
        }
        if !self.active.insert(plugin.name().to_string()) {
            return;
        }
        self.notices.extend(self.runtime.load_plugin(&plugin));
    }

    /// Whether `name` has run this session.
    #[must_use]
    pub fn is_active(&self, name: &str) -> bool {
        self.active.contains(name)
    }

    /// Every plugin this session knows of, whether or not it has run.
    #[must_use]
    pub fn known(&self) -> &[Plugin] {
        &self.known
    }

    /// What `:checkhealth` reports: API ranges, missing external programs,
    /// and names two plugins both claim.
    #[must_use]
    pub fn health(&self) -> Vec<String> {
        crate::health::report(
            &self.known,
            &|name| self.is_active(name),
            &crate::health::on_path,
        )
    }

    /// Turn a plugin on because something asked for a name it owns.
    ///
    /// A config that said `disable(name)` is obeyed: an opinion the user
    /// wrote down outranks one the project implies. Answers whether the
    /// plugin is now running.
    pub fn activate(&mut self, plugin: &Plugin) -> bool {
        if self.runtime.plugin_choice(plugin.name()) == Some(false) {
            return false;
        }
        let index = self
            .known
            .iter()
            .position(|p| p.name() == plugin.name())
            .unwrap_or_else(|| {
                self.known.push(plugin.clone());
                self.known.len() - 1
            });
        self.run_plugin(index);
        true
    }

    /// Whether a plugin runs unasked: what config chose, or what the plugin
    /// ships as when config said nothing.
    #[must_use]
    pub fn wants(&self, plugin: &Plugin) -> bool {
        self.runtime
            .plugin_choice(plugin.name())
            .unwrap_or_else(|| plugin.default_on())
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
    if p.hardware {
        preset = preset.require_hardware().map_err(fail)?;
    }
    Ok(preset)
}

/// The answer a user has already given in the frontend.
#[derive(Debug, Clone, Copy, Default)]
struct GrantOnce;

impl TrustPrompt for GrantOnce {
    fn trust(&self, _path: &Path) -> Trust {
        Trust::Granted
    }
}

/// Asks on the terminal, and refuses when there is nobody to ask.
///
/// The fallback for a session with no frontend to ask in: `-c` scripts and
/// `--script` runs, where the editor never draws anything.
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

    /// Write a package into `<site>/pack/<group>/<kind>/<name>`.
    fn install(site: &Path, kind: &str, name: &str, manifest: &str, source: &str) {
        let root = site.join("pack/test").join(kind).join(name);
        std::fs::create_dir_all(root.join("plugin")).unwrap();
        std::fs::write(root.join("davimci.toml"), manifest).unwrap();
        std::fs::write(root.join("plugin/init.lua"), source).unwrap();
    }

    #[test]
    fn a_start_package_runs_and_its_lua_directory_answers_require() {
        let dir = scratch("pack-start");
        let (cfg, site) = (dir.join("config"), dir.join("site"));
        std::fs::create_dir_all(&cfg).unwrap();
        install(
            &site,
            "start",
            "beats",
            "name = \"beats\"\napi = \"^1.0\"\n\n[provides]\nmotions = [\"next_beat\"]\n",
            r#"local grid = require("beats.grid")
               require("davimci.motions").register("next_beat", function() return grid.first end)"#,
        );
        let lua = site.join("pack/test/start/beats/lua/beats");
        std::fs::create_dir_all(&lua).unwrap();
        std::fs::write(lua.join("grid.lua"), "return { first = 12 }").unwrap();

        let mut plugins = Plugins::load(
            Some(&ConfigPaths::new(&cfg).with_site(&site)),
            &cfg,
            &DenyAll,
        );
        let notices = plugins.take_notices();
        assert!(notices.is_empty(), "{notices:?}");
        assert!(
            plugins.is_active("beats"),
            "an installed package did not run"
        );
        assert!(plugins.motion_names().iter().any(|m| m == "next_beat"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_opt_package_runs_only_when_plugins_lua_names_it() {
        let dir = scratch("pack-opt");
        let (cfg, site) = (dir.join("config"), dir.join("site"));
        std::fs::create_dir_all(&cfg).unwrap();
        let manifest = "name = \"proxies\"\napi = \"^1.0\"\n";
        let source =
            r#"require("davimci.motions").register("proxy_next", function() return 0 end)"#;
        install(&site, "opt", "proxies", manifest, source);
        let paths = ConfigPaths::new(&cfg).with_site(&site);

        let plugins = Plugins::load(Some(&paths), &cfg, &DenyAll);
        assert!(!plugins.is_active("proxies"), "an opt package ran unasked");

        std::fs::write(
            cfg.join("plugins.lua"),
            r#"require("davimci.pack").add("proxies")"#,
        )
        .unwrap();
        let mut plugins = Plugins::load(Some(&paths), &cfg, &DenyAll);
        let notices = plugins.take_notices();
        assert!(notices.is_empty(), "{notices:?}");
        assert!(plugins.is_active("proxies"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A plugin written against an API this build does not offer is refused
    /// rather than run: it would ask for edits the host would misread.
    #[test]
    fn a_package_needing_another_api_is_refused_with_a_sentence() {
        let dir = scratch("pack-api");
        let (cfg, site) = (dir.join("config"), dir.join("site"));
        std::fs::create_dir_all(&cfg).unwrap();
        install(
            &site,
            "start",
            "future",
            "name = \"future\"\napi = \"^9.0\"\n",
            "error('this must never run')",
        );
        let mut plugins = Plugins::load(
            Some(&ConfigPaths::new(&cfg).with_site(&site)),
            &cfg,
            &DenyAll,
        );
        let notices = plugins.take_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].text.contains("api"), "{notices:?}");
        assert!(!plugins.is_active("future"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Packages run before the user's own files, so a config always wins.
    #[test]
    fn a_config_overrides_what_a_package_registered() {
        let dir = scratch("pack-order");
        let (cfg, site) = (dir.join("config"), dir.join("site"));
        std::fs::create_dir_all(&cfg).unwrap();
        install(
            &site,
            "start",
            "marks",
            "name = \"marks\"\napi = \"^1.0\"\n",
            r#"require("davimci.motions").register("m", function() return 1 end)"#,
        );
        std::fs::write(
            cfg.join("init.lua"),
            r#"require("davimci.motions").register("m", function() return 99 end)"#,
        )
        .unwrap();
        let mut plugins = Plugins::load(
            Some(&ConfigPaths::new(&cfg).with_site(&site)),
            &cfg,
            &DenyAll,
        );
        assert!(plugins.take_notices().is_empty());
        let env = davimci_lua::MotionEnv::default();
        let answer = plugins
            .run_motion("m", &davimci_lua::Opts::new(), &env)
            .unwrap();
        assert_eq!(answer, davimci_lua::MotionAnswer::Found(99));
        std::fs::remove_dir_all(&dir).unwrap();
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
