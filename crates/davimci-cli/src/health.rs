//! `:checkhealth`: what is wrong with this session's plugins, in sentences.
//!
//! The report is a pure function of what was discovered, so the three things
//! it can catch - an API a plugin cannot use, an external program it needs
//! and does not have, and a name two plugins both claim - are testable with
//! no plugins installed and no editor running.

use std::collections::BTreeMap;
use std::path::Path;

use davimci_lua::{API_VERSION, Plugin};

/// Build the report. `active` answers whether a plugin ran this session and
/// `available` whether an external program can be found.
pub fn report(
    plugins: &[Plugin],
    active: &dyn Fn(&str) -> bool,
    available: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut lines = vec![format!("davimci plugin api {API_VERSION}")];
    let installed = plugins.iter().filter(|p| !p.is_builtin()).count();
    lines.push(format!(
        "{} bundled, {installed} installed",
        plugins.len() - installed
    ));

    for plugin in plugins {
        let name = plugin.name();
        let where_from = if plugin.is_builtin() {
            "bundled".to_string()
        } else {
            plugin
                .root()
                .map_or_else(|| "installed".to_string(), |r| r.display().to_string())
        };
        if plugin.compatible() {
            let state = if active(name) { "running" } else { "off" };
            lines.push(format!(
                "OK   {name} {} ({where_from}, {state})",
                plugin.manifest.version
            ));
        } else {
            lines.push(format!("WARN {}", plugin.incompatible_notice()));
        }
        for program in &plugin.manifest.requires {
            if !available(program) {
                lines.push(format!(
                    "WARN {name} needs '{program}', which is not on PATH; install it or the plugin will fail where it uses it"
                ));
            }
        }
    }

    lines.extend(conflicts(plugins));
    if lines.iter().all(|l| !l.starts_with("WARN")) {
        lines.push("no problems found".into());
    }
    lines
}

/// Names more than one plugin claims. Load order decides the winner, and the
/// user is the only one who can say whether that is the one they wanted.
fn conflicts(plugins: &[Plugin]) -> Vec<String> {
    let mut owners: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for plugin in plugins {
        let p = &plugin.manifest.provides;
        for (kind, names) in [
            ("transition", &p.transitions),
            ("motion", &p.motions),
            ("track kind", &p.track_kinds),
        ] {
            for name in names {
                owners
                    .entry((kind, name.as_str()))
                    .or_default()
                    .push(plugin.name());
            }
        }
    }
    owners
        .into_iter()
        .filter(|(_, who)| who.len() > 1)
        .map(|((kind, name), who)| {
            let last = who.last().copied().unwrap_or_default();
            format!(
                "WARN the {kind} '{name}' is claimed by {}; the last loaded wins, which is {last}",
                who.join(" and ")
            )
        })
        .collect()
}

/// Whether `program` is an executable somewhere on `PATH`.
#[must_use]
pub fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_lua::{Manifest, Source};

    fn plugin(manifest: &str) -> Plugin {
        Plugin {
            manifest: Manifest::parse(manifest, "davimci.toml").unwrap(),
            source: Source::Builtin(""),
        }
    }

    #[test]
    fn a_healthy_session_says_so_and_names_nothing() {
        let plugins = [plugin("name = \"silence\"\napi = \"^1.0\"\n")];
        let lines = report(&plugins, &|_| true, &|_| true);
        assert!(lines.iter().any(|l| l.contains("silence")), "{lines:?}");
        assert!(
            lines.contains(&"no problems found".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn an_api_range_this_build_cannot_meet_is_reported() {
        let plugins = [plugin("name = \"future\"\napi = \">=9.0\"\n")];
        let lines = report(&plugins, &|_| false, &|_| true);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("WARN") && l.contains("api")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_missing_external_program_is_reported_against_the_plugin_that_needs_it() {
        let plugins = [plugin(
            "name = \"beats\"\napi = \"^1.0\"\nrequires = [\"aubio\"]\n",
        )];
        let lines = report(&plugins, &|_| true, &|p| p != "aubio");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("beats needs 'aubio'") && l.contains("PATH")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_name_two_plugins_claim_is_reported_with_the_winner() {
        let plugins = [
            plugin("name = \"a\"\napi = \"^1.0\"\n\n[provides]\nmotions = [\"next_beat\"]\n"),
            plugin("name = \"b\"\napi = \"^1.0\"\n\n[provides]\nmotions = [\"next_beat\"]\n"),
        ];
        let lines = report(&plugins, &|_| true, &|_| true);
        let line = lines
            .iter()
            .find(|l| l.contains("next_beat"))
            .expect("no conflict reported");
        assert!(
            line.contains("a and b") && line.contains("which is b"),
            "{line}"
        );
    }
}
