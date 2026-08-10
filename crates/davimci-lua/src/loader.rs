//! Finding and loading config.
//!
//! Load order mirrors Neovim's: `plugins.lua` first, because it only says
//! which plugins to run and the host needs that answer before it runs any of
//! them; then the packages on the runtime path; then `init.lua` (it may
//! `require` the others itself), `keymaps.lua`, and every file in `motions/`,
//! `presets/`, and `plugin/` in sorted order so a load is reproducible.
//!
//! Packages run before the user's own files so a config always wins over a
//! plugin without either knowing about the other.
//!
//! Failures are isolated per file: one broken plugin costs you that plugin,
//! not the editor.

use std::path::{Path, PathBuf};

use davimci_core::Notice;

use crate::error::LuaError;
use crate::pack::{Plugin, Source};
use crate::runtime::{Runtime, Sandbox};

/// Where user config and installed packages live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub root: PathBuf,
    /// The package root, holding `pack/<group>/{start,opt}/<plugin>`.
    ///
    /// `None` in a test or an embedded run, which is what keeps whatever the
    /// developer happens to have installed out of a test's answer.
    pub site: Option<PathBuf>,
}

impl ConfigPaths {
    /// A config root with no packages: nothing is discovered but this tree.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            site: None,
        }
    }

    #[must_use]
    pub fn with_site(mut self, site: impl Into<PathBuf>) -> Self {
        self.site = Some(site.into());
        self
    }

    /// `$XDG_CONFIG_HOME/davimci` for config and
    /// `$XDG_DATA_HOME/davimci/site` for packages, with the usual `~`
    /// fallbacks.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.clone().map(|h| h.join(".config")))?;
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local/share")))?;
        Some(Self {
            root: config.join("davimci"),
            site: Some(data.join("davimci/site")),
        })
    }

    /// Every plugin installed under [`ConfigPaths::site`], and whatever was
    /// wrong with the ones that could not be read.
    #[must_use]
    pub fn packages(&self) -> (Vec<Plugin>, Vec<LuaError>) {
        match &self.site {
            Some(site) => crate::pack::discover(site),
            None => (Vec::new(), Vec::new()),
        }
    }

    /// The file that declares which bundled plugins to run, if it exists.
    ///
    /// Kept out of [`ConfigPaths::files`]: it runs in its own earlier pass,
    /// and running it twice would run its side effects twice.
    #[must_use]
    pub fn plugins_file(&self) -> Option<PathBuf> {
        let p = self.root.join("plugins.lua");
        p.is_file().then_some(p)
    }

    /// Files to run, in order, skipping what does not exist.
    #[must_use]
    pub fn files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for name in ["init.lua", "keymaps.lua"] {
            let p = self.root.join(name);
            if p.is_file() {
                out.push(p);
            }
        }
        out.extend(plugin_files(&self.root));
        out
    }
}

/// The files a plugin directory contributes, in load order. The layout is
/// the config directory's own, so a config tree can be moved into a package
/// unchanged.
#[must_use]
pub fn plugin_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["motions", "presets", "plugin"] {
        out.extend(lua_files_in(&root.join(dir)));
    }
    out
}

/// The `package.path` a `require` over the runtime path needs, so a plugin's
/// `lua/foo/bar.lua` answers `require("foo.bar")` the way Neovim's does.
#[must_use]
pub fn search_path(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|r| {
            let lua = r.join("lua");
            format!("{0}/?.lua;{0}/?/init.lua", lua.display())
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn lua_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "lua"))
        .collect();
    files.sort();
    files
}

/// The answer to "may this project-local config run?".
///
/// Project-local config is code from wherever the footage came from, so it
/// is opt-in: nothing runs until the user says so for that exact path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Granted,
    Denied,
}

/// Asked once per project-local config file. A frontend implements this as a
/// prompt; tests implement it as a constant.
pub trait TrustPrompt {
    fn trust(&self, path: &Path) -> Trust;
}

/// Refuses everything - the safe default when there is no way to ask.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyAll;

impl TrustPrompt for DenyAll {
    fn trust(&self, _path: &Path) -> Trust {
        Trust::Denied
    }
}

impl Runtime {
    /// Run `plugins.lua` alone, so [`Runtime::plugin_choice`] can answer
    /// before the bundled plugins run.
    pub fn load_plugin_choices(&self, paths: &ConfigPaths) -> Vec<Notice> {
        let Some(file) = paths.plugins_file() else {
            return Vec::new();
        };
        if let Err(e) = self.exec_file(&file, Sandbox::Trusted) {
            self.push_notice(&e);
        }
        self.take_notices()
    }

    /// Run one plugin directory's files, isolating failures per file.
    pub fn load_plugin(&self, plugin: &Plugin) -> Vec<Notice> {
        match &plugin.source {
            Source::Builtin(src) => {
                if let Err(e) = self.exec(src, plugin.name(), Sandbox::Trusted) {
                    self.push_notice(&e);
                }
            }
            Source::Start(root) | Source::Opt(root) => {
                for file in plugin_files(root) {
                    if let Err(e) = self.exec_file(&file, Sandbox::Trusted) {
                        self.push_notice(&e);
                    }
                }
            }
        }
        self.take_notices()
    }

    /// Point `require` at the `lua/` directory of every runtime path entry,
    /// nearest first. The config root goes last so a package cannot shadow
    /// a module the user wrote.
    pub fn set_search_path(&self, roots: &[PathBuf]) -> Result<(), LuaError> {
        self.set_package_path(&search_path(roots))
    }

    /// `opt` packages `plugins.lua` asked for, in the order it asked.
    #[must_use]
    pub fn packadds(&self) -> Vec<String> {
        self.state_packadds()
    }

    /// Load the user's config tree. Every file failure becomes a notice and
    /// loading continues, so one bad plugin cannot cost you your keymaps.
    pub fn load_config(&self, paths: &ConfigPaths) -> Vec<Notice> {
        let mut notices = Vec::new();
        for file in paths.files() {
            if let Err(e) = self.exec_file(&file, Sandbox::Trusted) {
                self.push_notice(&e);
            }
            notices.extend(self.take_notices());
        }
        notices
    }

    /// Load `<dir>/.davimci.lua` if the user trusts it.
    ///
    /// An untrusted file is not read, not compiled, and not run; a trusted
    /// one still runs under [`Sandbox::Restricted`], because "I want this
    /// project's export presets" is not "I want this directory to run
    /// `os.execute`".
    pub fn load_project_local(
        &self,
        dir: &Path,
        prompt: &dyn TrustPrompt,
    ) -> (bool, Option<Notice>) {
        let path = dir.join(".davimci.lua");
        if !path.is_file() {
            return (false, None);
        }
        if prompt.trust(&path) == Trust::Denied {
            let e = LuaError::Config(format!(
                "{} was not loaded: project-local config runs only when trusted",
                path.display()
            ));
            return (false, Some(Notice::from_error(&e)));
        }
        match self.exec_file(&path, Sandbox::Restricted) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(Notice::from_error(&e))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "davimci-lua-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_order_is_init_keymaps_then_sorted_directories() {
        let root = scratch("order");
        std::fs::write(root.join("init.lua"), "").unwrap();
        std::fs::write(root.join("keymaps.lua"), "").unwrap();
        std::fs::create_dir_all(root.join("motions")).unwrap();
        std::fs::write(root.join("motions/b.lua"), "").unwrap();
        std::fs::write(root.join("motions/a.lua"), "").unwrap();
        std::fs::write(root.join("motions/notes.txt"), "").unwrap();

        let files: Vec<String> = ConfigPaths::new(&root)
            .files()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, ["init.lua", "keymaps.lua", "a.lua", "b.lua"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn plugins_lua_runs_in_its_own_pass_and_never_twice() {
        let root = scratch("choices");
        std::fs::write(
            root.join("plugins.lua"),
            r#"runs = (runs or 0) + 1
               require("davimci.plugins").enable("which-key")"#,
        )
        .unwrap();
        std::fs::write(root.join("init.lua"), "").unwrap();
        let paths = ConfigPaths::new(&root);
        assert!(
            !paths.files().iter().any(|p| p.ends_with("plugins.lua")),
            "plugins.lua would run a second time"
        );

        let rt = Runtime::new().unwrap();
        assert!(rt.load_plugin_choices(&paths).is_empty());
        assert_eq!(rt.plugin_choice("which-key"), Some(true));
        assert!(rt.load_config(&paths).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_config_tree_is_not_an_error() {
        let paths = ConfigPaths::new("/nonexistent/davimci-config");
        assert!(paths.files().is_empty());
        let rt = Runtime::new().unwrap();
        assert!(rt.load_config(&paths).is_empty());
    }
}
