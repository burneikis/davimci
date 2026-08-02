//! Finding and loading config (spec §9.1, §9.7).
//!
//! Load order mirrors Neovim's: `init.lua` first (it may `require` the
//! others itself), then `keymaps.lua`, then every file in `motions/`,
//! `presets/`, and `plugin/` in sorted order so a load is reproducible.
//!
//! Failures are isolated per file: one broken plugin costs you that plugin,
//! not the editor.

use std::path::{Path, PathBuf};

use davimci_core::Notice;

use crate::error::LuaError;
use crate::runtime::{Runtime, Sandbox};

/// Where user config lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub root: PathBuf,
}

impl ConfigPaths {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `$XDG_CONFIG_HOME/davimci`, falling back to `~/.config/davimci`.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(Self::new(base.join("davimci")))
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
        for dir in ["motions", "presets", "plugin"] {
            out.extend(lua_files_in(&self.root.join(dir)));
        }
        out
    }
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

/// The answer to "may this project-local config run?" (spec §9.7).
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

    /// Load `<dir>/.davimci.lua` if the user trusts it (spec §9.7).
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
    fn a_missing_config_tree_is_not_an_error() {
        let paths = ConfigPaths::new("/nonexistent/davimci-config");
        assert!(paths.files().is_empty());
        let rt = Runtime::new().unwrap();
        assert!(rt.load_config(&paths).is_empty());
    }
}
