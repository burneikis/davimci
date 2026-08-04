//! Transition types, mapped onto MLT services.
//!
//! The model stores a transition as a *name*, because types are extensible
//! and `davimci-core` may not know what MLT is. This is the one place that
//! turns a name into a service: video types composite with `luma`, audio
//! types cross-fade with `mix`, and an unknown name falls back to the
//! default rather than failing a render.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use davimci_core::TrackKind;

/// How one transition type is realised by the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionSpec {
    pub service: String,
    pub props: Vec<(String, String)>,
}

impl TransitionSpec {
    fn new(service: &str, props: &[(&str, &str)]) -> Self {
        Self {
            service: service.to_string(),
            props: props
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }
}

/// Built-in video transition types, by name.
///
/// A `luma` with no resource is a plain dissolve; the wipes are the same
/// service driven by a soft-edged geometry, which is how MLT itself expresses
/// them.
type Builtin = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const VIDEO: &[Builtin] = &[
    ("dissolve", "luma", &[]),
    ("wipe_left", "luma", &[("resource", "%luma01.pgm")]),
    (
        "wipe_right",
        "luma",
        &[("resource", "%luma01.pgm"), ("invert", "1")],
    ),
    ("wipe_up", "luma", &[("resource", "%luma03.pgm")]),
    (
        "wipe_down",
        "luma",
        &[("resource", "%luma03.pgm"), ("invert", "1")],
    ),
    ("iris", "luma", &[("resource", "%luma05.pgm")]),
];

/// Types registered at runtime, on top of the built-ins.
///
/// Process-global because [`spec`] is a pure lookup called from the
/// projection, which has no backend to ask; registering is the backend's job
/// and happens once, at config load.
fn registered() -> &'static RwLock<BTreeMap<String, TransitionSpec>> {
    static REGISTRY: OnceLock<RwLock<BTreeMap<String, TransitionSpec>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Add a transition type. A name that is already registered is replaced, so
/// reloading a config does not accumulate stale definitions.
pub fn register(name: &str, service: &str, props: Vec<(String, String)>) {
    if let Ok(mut map) = registered().write() {
        map.insert(
            name.to_string(),
            TransitionSpec {
                service: service.to_string(),
                props,
            },
        );
    }
}

/// Every registered type's name.
#[must_use]
pub fn registered_names() -> Vec<String> {
    registered()
        .read()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Every transition type a user may name.
#[must_use]
pub fn names() -> Vec<&'static str> {
    VIDEO.iter().map(|(n, _, _)| *n).collect()
}

/// Whether `name` is a transition type this build can render.
#[must_use]
pub fn is_known(name: &str) -> bool {
    VIDEO.iter().any(|(n, _, _)| *n == name)
        || registered().read().is_ok_and(|m| m.contains_key(name))
}

/// The service and properties for `kind` on a track of `track_kind`.
///
/// Audio never wipes: whatever the type is called, two overlapping audio
/// clips cross-fade, so an audio track always gets `mix`. Keeping the name in
/// the model regardless means switching the video type does not have to
/// rewrite the linked audio's transition.
#[must_use]
pub fn spec(kind: &str, track_kind: TrackKind) -> TransitionSpec {
    if track_kind == TrackKind::Audio {
        return TransitionSpec::new("mix", &[("start", "0"), ("end", "1")]);
    }
    if let Some(found) = registered().read().ok().and_then(|m| m.get(kind).cloned()) {
        return found;
    }
    VIDEO.iter().find(|(n, _, _)| *n == kind).map_or_else(
        // An unregistered name still renders: a project made with a config
        // type has to open in a build that does not have that config.
        || TransitionSpec::new("luma", &[]),
        |(_, service, props)| TransitionSpec::new(service, props),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_always_cross_fades_whatever_the_type_is_called() {
        assert_eq!(spec("wipe_left", TrackKind::Audio).service, "mix");
        assert_eq!(spec("dissolve", TrackKind::Audio).service, "mix");
    }

    #[test]
    fn a_dissolve_is_a_bare_luma_and_a_wipe_carries_a_geometry() {
        assert_eq!(spec("dissolve", TrackKind::Video).service, "luma");
        assert!(spec("dissolve", TrackKind::Video).props.is_empty());
        assert!(
            spec("wipe_left", TrackKind::Video)
                .props
                .iter()
                .any(|(k, _)| k == "resource")
        );
    }

    /// A registered type is rendered by the service it named,
    /// and shows up as a known type.
    #[test]
    fn a_registered_type_names_its_own_service() {
        register(
            "sparkle_test",
            "frei0r.sparkle",
            vec![("density".into(), "3".into())],
        );
        let video = spec("sparkle_test", TrackKind::Video);
        assert_eq!(video.service, "frei0r.sparkle");
        assert_eq!(video.props, vec![("density".to_string(), "3".to_string())]);
        assert!(is_known("sparkle_test"));
        assert!(registered_names().contains(&"sparkle_test".to_string()));
        // Audio still cross-fades, whatever a config called the type.
        assert_eq!(spec("sparkle_test", TrackKind::Audio).service, "mix");
    }

    /// An unknown name must still render: a project made with a Lua-defined
    /// type has to open in a build that does not have that type.
    #[test]
    fn an_unknown_type_degrades_to_a_dissolve() {
        assert_eq!(spec("sparkle", TrackKind::Video).service, "luma");
        assert!(!is_known("sparkle"));
        assert!(names().contains(&"dissolve"));
    }
}
