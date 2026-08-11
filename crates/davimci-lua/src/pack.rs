//! Packages: the runtime path, plugin manifests, and what a name belongs to.
//!
//! A plugin is a directory laid out like the config directory, so a config
//! tree can be moved into a package unchanged. The host ships the loading
//! mechanism only - discovery, ordering and the API compatibility gate -
//! while fetching a plugin from anywhere is a separate program's job.
//!
//! A manifest is declarative and is never executed, so the host can answer
//! "who owns `wipe_left`?" without running a stranger's Lua.

use std::path::{Path, PathBuf};

use crate::error::LuaError;

/// The version of the `davimci.*` Lua surface.
///
/// It moves independently of the binary's version: a plugin declares the
/// range of *API* it was written against, and nothing else about the host is
/// its business.
pub const API_VERSION: Version = Version { major: 1, minor: 0 };

/// A `major.minor` version. Patch numbers are accepted and ignored: the API
/// surface either has a call or does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl Version {
    pub fn parse(text: &str) -> Result<Self, LuaError> {
        let bad = || LuaError::Config(format!("'{text}' is not a version like 1.0"));
        let mut parts = text.trim().split('.');
        let major = parts.next().ok_or_else(bad)?.trim();
        let major: u32 = major.parse().map_err(|_| bad())?;
        let minor = match parts.next() {
            Some(m) => m.trim().parse().map_err(|_| bad())?,
            None => 0,
        };
        Ok(Self { major, minor })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The API versions a plugin says it works with, as `">=1.0, <2.0"`.
///
/// An empty range accepts everything, which is what a manifest that says
/// nothing means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ApiRange {
    pub min: Option<Version>,
    pub below: Option<Version>,
}

impl ApiRange {
    pub fn parse(text: &str) -> Result<Self, LuaError> {
        let mut range = Self::default();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            if let Some(v) = part.strip_prefix(">=") {
                range.min = Some(Version::parse(v)?);
            } else if let Some(v) = part.strip_prefix('<') {
                range.below = Some(Version::parse(v)?);
            } else if let Some(v) = part.strip_prefix('^') {
                // `^1.2` is ">=1.2, <2.0", the same shape everyone expects.
                let v = Version::parse(v)?;
                range.min = Some(v);
                range.below = Some(Version {
                    major: v.major + 1,
                    minor: 0,
                });
            } else {
                return Err(LuaError::Config(format!(
                    "'{part}' is not an api requirement; write '>=1.0', '<2.0' or '^1.0'"
                )));
            }
        }
        Ok(range)
    }

    #[must_use]
    pub fn accepts(&self, v: Version) -> bool {
        self.min.is_none_or(|m| v >= m) && self.below.is_none_or(|b| v < b)
    }
}

impl std::fmt::Display for ApiRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.min, self.below) {
            (Some(a), Some(b)) => write!(f, ">={a}, <{b}"),
            (Some(a), None) => write!(f, ">={a}"),
            (None, Some(b)) => write!(f, "<{b}"),
            (None, None) => write!(f, "any"),
        }
    }
}

/// The names a plugin owns, declared so the host can tell a user which
/// plugin to enable when a project or a keystroke names something nothing
/// registered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provides {
    /// Transition types, as they appear in a saved project.
    pub transitions: Vec<String>,
    /// Motions, as a config or a macro names them.
    pub motions: Vec<String>,
    /// Track kinds this plugin is the editing workflow for, as
    /// `davimci_core::TrackKind::prefix` spells them.
    pub track_kinds: Vec<String>,
    /// `:` commands this plugin is the opinion behind, without the colon.
    /// The host can then refuse one by naming its owner rather than
    /// pretending the command does not exist.
    pub commands: Vec<String>,
}

/// A plugin's `davimci.toml`: everything the host may know about a plugin
/// without running it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub api: ApiRange,
    /// Whether it runs when config says nothing about it.
    ///
    /// Off is the honest default: a plugin that is on in practice is core
    /// wearing a plugin's name.
    pub default_on: bool,
    /// External programs or libraries the plugin needs, reported by
    /// `:checkhealth` rather than checked here.
    pub requires: Vec<String>,
    pub provides: Provides,
}

impl Manifest {
    /// Parse the manifest subset: `key = "string"`, `key = true`,
    /// `key = ["a", "b"]`, and a `[provides]` section. Anything else is a
    /// syntax error naming the file, because a manifest the host guesses at
    /// is a manifest that lies about what a plugin owns.
    pub fn parse(text: &str, origin: &str) -> Result<Self, LuaError> {
        let fail =
            |line: usize, why: &str| LuaError::Config(format!("{origin}: line {line}: {why}"));
        let mut name = String::new();
        let mut version = String::from("0.0.0");
        let mut api = ApiRange::default();
        let mut default_on = false;
        let mut requires = Vec::new();
        let mut provides = Provides::default();
        let mut section = String::new();

        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(head) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = head.trim().to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(fail(n + 1, "expected 'key = value'"));
            };
            let (key, value) = (key.trim(), value.trim());
            match (section.as_str(), key) {
                ("", "name") => {
                    name = string_value(value)
                        .ok_or_else(|| fail(n + 1, "name must be a quoted string"))?;
                }
                ("", "version") => {
                    version = string_value(value)
                        .ok_or_else(|| fail(n + 1, "version must be a quoted string"))?;
                }
                ("", "api") => {
                    let v = string_value(value)
                        .ok_or_else(|| fail(n + 1, "api must be a quoted string"))?;
                    api = ApiRange::parse(&v)?;
                }
                ("", "default_on") => default_on = value == "true",
                ("", "requires") => {
                    requires = list_value(value)
                        .ok_or_else(|| fail(n + 1, "requires must be a list of strings"))?;
                }
                ("provides", "transitions") => {
                    provides.transitions = list_value(value)
                        .ok_or_else(|| fail(n + 1, "transitions must be a list of strings"))?;
                }
                ("provides", "motions") => {
                    provides.motions = list_value(value)
                        .ok_or_else(|| fail(n + 1, "motions must be a list of strings"))?;
                }
                ("provides", "commands") => {
                    provides.commands = list_value(value)
                        .ok_or_else(|| fail(n + 1, "commands must be a list of strings"))?;
                }
                ("provides", "track_kinds") => {
                    provides.track_kinds = list_value(value)
                        .ok_or_else(|| fail(n + 1, "track_kinds must be a list of strings"))?;
                }
                (s, k) => {
                    return Err(fail(
                        n + 1,
                        &format!(
                            "'{k}' is not a manifest key{}",
                            if s.is_empty() {
                                String::new()
                            } else {
                                format!(" in [{s}]")
                            }
                        ),
                    ));
                }
            }
        }
        if name.is_empty() {
            return Err(LuaError::Config(format!(
                "{origin}: a manifest must give a name"
            )));
        }
        Ok(Self {
            name,
            version,
            api,
            default_on,
            requires,
            provides,
        })
    }
}

fn string_value(v: &str) -> Option<String> {
    let inner = v.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

fn list_value(v: &str) -> Option<Vec<String>> {
    let inner = v.strip_prefix('[')?.strip_suffix(']')?;
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(string_value)
        .collect()
}

/// Where a plugin came from, which is the whole of the difference between
/// one that ships with davimci and one a user installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Compiled into the binary, so a fresh install has examples to read.
    Builtin(&'static str),
    /// `pack/*/start/<name>`: loaded at startup unless config says no.
    Start(PathBuf),
    /// `pack/*/opt/<name>`: loaded only when something asks for it by name.
    Opt(PathBuf),
}

/// A plugin the host knows about, whether or not it has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub manifest: Manifest,
    pub source: Source,
}

impl Plugin {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    #[must_use]
    pub fn is_builtin(&self) -> bool {
        matches!(self.source, Source::Builtin(_))
    }

    /// Whether it runs when config says nothing. An `opt` package never
    /// does: putting it in `opt` is the statement that it should not.
    #[must_use]
    pub fn default_on(&self) -> bool {
        !matches!(self.source, Source::Opt(_)) && self.manifest.default_on
    }

    /// The plugin's own directory, for everything but a builtin.
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        match &self.source {
            Source::Builtin(_) => None,
            Source::Start(p) | Source::Opt(p) => Some(p),
        }
    }

    /// Whether the API this plugin was written against is one this build
    /// still offers.
    #[must_use]
    pub fn compatible(&self) -> bool {
        self.manifest.api.accepts(API_VERSION)
    }

    /// The sentence a user gets when the plugin is refused.
    #[must_use]
    pub fn incompatible_notice(&self) -> String {
        format!(
            "the plugin '{}' needs davimci api {} and this build offers {API_VERSION}; it was not loaded",
            self.name(),
            self.manifest.api
        )
    }
}

/// Discover the plugins installed under `site`.
///
/// Layout, borrowed wholesale from Neovim's `packpath`:
///
/// ```text
/// <site>/pack/<group>/start/<plugin>/   loaded at startup
/// <site>/pack/<group>/opt/<plugin>/     loaded by davimci.pack.add
/// ```
///
/// The group level exists so a fetcher can own one directory
/// (`pack/fetched/`) without touching what a user dropped in by hand.
/// Ordering is by group then plugin name, so a load is reproducible.
#[must_use]
pub fn discover(site: &Path) -> (Vec<Plugin>, Vec<LuaError>) {
    let mut found = Vec::new();
    let mut problems = Vec::new();
    for group in sorted_dirs(&site.join("pack")) {
        for (dir, opt) in [(group.join("start"), false), (group.join("opt"), true)] {
            for root in sorted_dirs(&dir) {
                match read_manifest(&root) {
                    Ok(manifest) => found.push(Plugin {
                        manifest,
                        source: if opt {
                            Source::Opt(root)
                        } else {
                            Source::Start(root)
                        },
                    }),
                    Err(e) => problems.push(e),
                }
            }
        }
    }
    (found, problems)
}

/// A plugin's manifest, or the defaults for a plugin that has none.
///
/// A manifest is optional so that a bare directory of Lua still works, the
/// way it does in Neovim; what it buys is the host being able to answer for
/// the plugin before running it.
fn read_manifest(root: &Path) -> Result<Manifest, LuaError> {
    let file = root.join("davimci.toml");
    let name = root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    if !file.is_file() {
        return Ok(Manifest {
            name,
            version: "0.0.0".into(),
            api: ApiRange::default(),
            default_on: true,
            requires: Vec::new(),
            provides: Provides::default(),
        });
    }
    let text = std::fs::read_to_string(&file).map_err(|e| LuaError::Io {
        path: file.display().to_string(),
        reason: e.to_string(),
    })?;
    let mut manifest = Manifest::parse(&text, &file.display().to_string())?;
    if manifest.name != name {
        return Err(LuaError::Config(format!(
            "{}: the manifest calls this plugin '{}' but it is installed as '{name}'",
            file.display(),
            manifest.name
        )));
    }
    // A package on the path is asked for by being there; `default_on` in a
    // manifest is what a builtin uses to say it is not.
    manifest.default_on = true;
    Ok(manifest)
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_declares_what_a_plugin_owns() {
        let m = Manifest::parse(
            r#"
            name = "silence"      # the directory it lives in
            version = "1.2.0"
            api = ">=1.0, <2.0"

            [provides]
            motions = ["next_silence", "prev_silence"]
            "#,
            "davimci.toml",
        )
        .unwrap();
        assert_eq!(m.name, "silence");
        assert_eq!(m.provides.motions, ["next_silence", "prev_silence"]);
        assert!(m.api.accepts(API_VERSION));
        assert!(!m.api.accepts(Version { major: 2, minor: 0 }));
        assert!(!m.default_on, "a manifest that says nothing means off");
    }

    #[test]
    fn an_unknown_manifest_key_is_an_error_naming_the_line() {
        let e =
            Manifest::parse("name = \"x\"\nprovides = [\"y\"]\n", "p/davimci.toml").unwrap_err();
        assert!(e.to_string().contains("line 2"), "{e}");
    }

    #[test]
    fn a_caret_range_is_the_shape_everyone_expects() {
        let r = ApiRange::parse("^1.2").unwrap();
        assert!(!r.accepts(Version { major: 1, minor: 1 }));
        assert!(r.accepts(Version { major: 1, minor: 9 }));
        assert!(!r.accepts(Version { major: 2, minor: 0 }));
    }
}
