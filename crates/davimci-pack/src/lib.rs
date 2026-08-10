//! Fetching plugins into a davimci site directory.
//!
//! A separate program on purpose. The editor exposes no way for Lua to spawn
//! a process, open a socket or write outside a project, and one program's
//! convenience is not worth handing that to every plugin; and a package
//! manager running inside the editor would be mutating the runtime path
//! while the editor is reading it.
//!
//! What this owns is a directory and a lockfile. What the editor owns is
//! everything after that: discovery, ordering, and the API gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// The group this program owns under `site/pack/`.
///
/// Everything it writes lives here, so `pack/manual/` stays whatever the
/// user put there and an `update` can never delete work by hand.
pub const GROUP: &str = "fetched";

/// Where a package is installed, which is the whole of the difference
/// between one that runs at startup and one that waits to be named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Start,
    Opt,
}

impl Kind {
    #[must_use]
    pub fn dir(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Opt => "opt",
        }
    }
}

/// One installed plugin, pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub url: String,
    /// The exact commit, which is the only thing that makes a restore
    /// reproducible. A project outlives a branch.
    pub rev: String,
    pub kind: Kind,
}

/// `davimci-lock.json`, kept beside `plugins.lua` so a config repository
/// carries the versions it was written against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    #[serde(default)]
    pub plugins: BTreeMap<String, Pin>,
}

impl Lock {
    /// Read the lockfile, treating a missing one as an empty one: a first
    /// `add` should not need a file to exist first.
    pub fn read(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("{} is not a davimci lockfile", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow!("{}: {e}", path.display())),
        }
    }

    /// Write the lockfile, pretty-printed and newline-terminated so a diff
    /// of it reads as one line per plugin.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

/// The two directories this program works between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `~/.config/davimci`, which is where the lockfile goes.
    pub config: PathBuf,
    /// `~/.local/share/davimci/site`, which is where packages go.
    pub site: PathBuf,
}

impl Paths {
    /// The same directories the editor reads, so what is fetched is what
    /// runs. Both are overridable for a scripted or scoped install.
    pub fn from_env() -> Result<Self> {
        if let (Some(config), Some(site)) = (
            std::env::var_os("DAVIMCI_CONFIG"),
            std::env::var_os("DAVIMCI_SITE"),
        ) {
            return Ok(Self {
                config: config.into(),
                site: site.into(),
            });
        }
        let paths = davimci_lua::ConfigPaths::from_env()
            .ok_or_else(|| anyhow!("no HOME, so there is no config directory to install into"))?;
        let site = paths
            .site
            .clone()
            .ok_or_else(|| anyhow!("no site directory to install into"))?;
        Ok(Self {
            config: paths.root,
            site,
        })
    }

    #[must_use]
    pub fn lockfile(&self) -> PathBuf {
        self.config.join("davimci-lock.json")
    }

    #[must_use]
    pub fn install_dir(&self, kind: Kind, name: &str) -> PathBuf {
        self.site
            .join("pack")
            .join(GROUP)
            .join(kind.dir())
            .join(name)
    }

    /// Where a plugin is, whichever way it was installed.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<(Kind, PathBuf)> {
        [Kind::Start, Kind::Opt].into_iter().find_map(|kind| {
            let dir = self.install_dir(kind, name);
            dir.is_dir().then_some((kind, dir))
        })
    }
}

/// A plugin as a user names it on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub url: String,
    pub name: String,
}

impl Spec {
    /// Accept `user/repo`, `host/user/repo`, a full URL, and a local path.
    ///
    /// The shorthand assumes GitHub the way every plugin manager does; a URL
    /// is taken verbatim, so nothing here decides where plugins may live. A
    /// local path is how a plugin is written: clone the thing being worked
    /// on and the editor loads it like any other.
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim().trim_end_matches('/');
        if spec.is_empty() {
            bail!("name a plugin to install, as 'user/repo' or a git URL");
        }
        let verbatim = spec.starts_with('/')
            || spec.starts_with('.')
            || spec.contains("://")
            || spec.starts_with("git@");
        let url = if verbatim {
            spec.to_string()
        } else if spec.matches('/').count() == 1 {
            format!("https://github.com/{spec}")
        } else if spec.contains('.') && spec.contains('/') {
            format!("https://{spec}")
        } else {
            bail!("'{spec}' is not a plugin; write 'user/repo' or a git URL");
        };
        let name = url
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".git")
            .to_string();
        if name.is_empty() {
            bail!("'{spec}' has no plugin name at the end of it");
        }
        Ok(Self { url, name })
    }
}

/// Run `git` in `dir` and return its stdout.
///
/// Shelling out rather than linking a git library keeps the fetcher small
/// and keeps the editor's dependency tree untouched by it.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| "git is not installed, and fetching a plugin needs it")?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", args.join(" "), why.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn head_rev(dir: &Path) -> Result<String> {
    git(dir, &["rev-parse", "HEAD"])
}

/// Clone `spec` into the site and pin whatever it landed on.
pub fn add(paths: &Paths, spec: &Spec, kind: Kind, rev: Option<&str>) -> Result<Pin> {
    let dir = paths.install_dir(kind, &spec.name);
    if dir.exists() {
        bail!(
            "{} is already installed; update or remove it first",
            spec.name
        );
    }
    let parent = dir
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", dir.display()))?;
    std::fs::create_dir_all(parent)?;
    let target = dir.display().to_string();
    git(parent, &["clone", "--quiet", &spec.url, &target])?;
    if let Some(rev) = rev {
        git(&dir, &["checkout", "--quiet", rev])?;
    }
    check_manifest(&dir, &spec.name)?;
    Ok(Pin {
        url: spec.url.clone(),
        rev: head_rev(&dir)?,
        kind,
    })
}

/// Pull the newest commit for one installed plugin and re-pin it.
pub fn update(paths: &Paths, name: &str) -> Result<Pin> {
    let (kind, dir) = paths
        .find(name)
        .ok_or_else(|| anyhow!("'{name}' is not installed"))?;
    let url = git(&dir, &["remote", "get-url", "origin"])?;
    git(&dir, &["fetch", "--quiet", "origin"])?;
    let branch = git(&dir, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .unwrap_or_else(|_| "origin/HEAD".to_string());
    git(&dir, &["checkout", "--quiet", "--force", &branch])?;
    check_manifest(&dir, name)?;
    Ok(Pin {
        url,
        rev: head_rev(&dir)?,
        kind,
    })
}

/// Install whatever the lockfile names and the disk does not have, at the
/// exact revision it names. This is what makes a config repository restore
/// the editor a project was cut on.
pub fn sync(paths: &Paths, lock: &Lock) -> Result<Vec<String>> {
    let mut restored = Vec::new();
    for (name, pin) in &lock.plugins {
        if paths.find(name).is_some() {
            continue;
        }
        let spec = Spec {
            url: pin.url.clone(),
            name: name.clone(),
        };
        add(paths, &spec, pin.kind, Some(&pin.rev))?;
        restored.push(name.clone());
    }
    Ok(restored)
}

/// Delete an installed plugin. Only ever inside this program's own group.
pub fn remove(paths: &Paths, name: &str) -> Result<()> {
    let (_, dir) = paths
        .find(name)
        .ok_or_else(|| anyhow!("'{name}' is not installed"))?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))
}

/// Every plugin this program installed, with what it is pinned to.
#[must_use]
pub fn list(paths: &Paths, lock: &Lock) -> Vec<String> {
    let mut lines = Vec::new();
    for kind in [Kind::Start, Kind::Opt] {
        let dir = paths.site.join("pack").join(GROUP).join(kind.dir());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let pinned = lock.plugins.get(&name).map_or_else(
                || "unpinned".to_string(),
                |p| p.rev.chars().take(9).collect(),
            );
            lines.push(format!("{name}  {}  {pinned}", kind.dir()));
        }
    }
    lines
}

/// Refuse a plugin whose manifest disagrees with the directory it is in, or
/// that needs an API this build's editor does not offer.
///
/// The check happens here so a bad install fails at install time, in a
/// terminal, rather than as a notice in the middle of an edit.
fn check_manifest(dir: &Path, name: &str) -> Result<()> {
    let file = dir.join("davimci.toml");
    if !file.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&file)?;
    let manifest = davimci_lua::Manifest::parse(&text, &file.display().to_string())
        .map_err(|e| anyhow!("{e}"))?;
    if manifest.name != name {
        bail!(
            "{} calls this plugin '{}' but it installs as '{name}'",
            file.display(),
            manifest.name
        );
    }
    if !manifest.api.accepts(davimci_lua::API_VERSION) {
        bail!(
            "{name} needs davimci api {} and this build offers {}",
            manifest.api,
            davimci_lua::API_VERSION
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_shorthand_spec_is_github_and_a_url_is_taken_as_written() {
        let s = Spec::parse("user/beatgrid").unwrap();
        assert_eq!(s.url, "https://github.com/user/beatgrid");
        assert_eq!(s.name, "beatgrid");

        let s = Spec::parse("https://git.example.org/a/b.git").unwrap();
        assert_eq!(s.url, "https://git.example.org/a/b.git");
        assert_eq!(s.name, "b");

        let s = Spec::parse("git.example.org/a/b").unwrap();
        assert_eq!(s.url, "https://git.example.org/a/b");

        // A local checkout is how a plugin is written.
        let s = Spec::parse("/tmp/beatgrid").unwrap();
        assert_eq!(s.url, "/tmp/beatgrid");
        assert_eq!(s.name, "beatgrid");

        assert!(Spec::parse("beatgrid").is_err());
        assert!(Spec::parse("  ").is_err());
    }

    #[test]
    fn a_lockfile_round_trips_and_a_missing_one_is_empty() {
        let dir = std::env::temp_dir().join(format!("davimci-pack-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("davimci-lock.json");
        assert_eq!(Lock::read(&path).unwrap(), Lock::default());

        let mut lock = Lock::default();
        lock.plugins.insert(
            "beatgrid".into(),
            Pin {
                url: "https://github.com/user/beatgrid".into(),
                rev: "9c1f0aa".into(),
                kind: Kind::Opt,
            },
        );
        lock.write(&path).unwrap();
        assert_eq!(Lock::read(&path).unwrap(), lock);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn everything_installed_lands_in_this_programs_own_group() {
        let paths = Paths {
            config: "/c".into(),
            site: "/s".into(),
        };
        assert_eq!(
            paths.install_dir(Kind::Start, "beatgrid"),
            PathBuf::from("/s/pack/fetched/start/beatgrid")
        );
        assert_eq!(paths.lockfile(), PathBuf::from("/c/davimci-lock.json"));
    }

    /// A manifest that disagrees with the directory would be a plugin the
    /// editor could not speak for, so the install fails rather than the
    /// session.
    #[test]
    fn an_install_is_refused_when_the_manifest_names_another_plugin() {
        let dir = std::env::temp_dir().join(format!("davimci-pack-mf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("davimci.toml"),
            "name = \"other\"\napi = \"^1.0\"\n",
        )
        .unwrap();
        let e = check_manifest(&dir, "beatgrid").unwrap_err();
        assert!(e.to_string().contains("installs as 'beatgrid'"), "{e}");

        std::fs::write(
            dir.join("davimci.toml"),
            "name = \"beatgrid\"\napi = \"^9.0\"\n",
        )
        .unwrap();
        let e = check_manifest(&dir, "beatgrid").unwrap_err();
        assert!(e.to_string().contains("api"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
