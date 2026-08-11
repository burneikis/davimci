//! The key namespaces core does not take.
//!
//! A plugin needs somewhere to bind that a future core binding will not take
//! from under it, the way vim leaves `<Space>` and the bracket pairs alone.
//! The reservation is only worth stating if it is checked, so this is the
//! check: what core holds in those namespaces today is listed here by name,
//! and anything new fails until it is either moved out or argued into the
//! list.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_keys::Key;
use davimci_keys::keymap::default_bindings;

/// Core bindings that predate the reservation. Nothing joins this list:
/// `<Space>` is the transport and mix pair every editor has under a leader,
/// and `gP`/`gT` are the shifted halves of `gp`/`gt` rather than new keys.
const GRANDFATHERED: &[&str] = &[
    "<Space><Space>",
    "<Space>l",
    "<Space>m",
    "<Space>p",
    "<Space>s",
    "gP",
    "gT",
    "zZ",
    "[t",
    "]t",
];

/// Whether `keys` falls in a namespace reserved for plugins and configs.
fn is_reserved(keys: &[Key]) -> bool {
    match keys {
        [
            Key::Char('[' | ']') | Key::Named(davimci_keys::Named::Space),
            _,
            ..,
        ] => true,
        [Key::Char('g' | 'z'), Key::Char(c), ..] => c.is_ascii_uppercase(),
        _ => false,
    }
}

#[test]
fn core_takes_no_new_binding_in_a_reserved_namespace() {
    let mut trespassers: Vec<String> = default_bindings()
        .into_iter()
        .map(|(keys, _)| keys)
        .filter(|keys| is_reserved(keys))
        .map(|keys| davimci_keys::docs::render(&keys))
        .filter(|rendered| !GRANDFATHERED.contains(&rendered.as_str()))
        .collect();
    trespassers.sort();
    assert!(
        trespassers.is_empty(),
        "these core bindings sit in a namespace reserved for plugins: {trespassers:?}. \
         Bind them elsewhere, or say why they belong in GRANDFATHERED."
    );
}

/// The reservation is worthless if the list of exceptions quietly grows to
/// cover everything, so the exceptions themselves are pinned.
#[test]
fn every_grandfathered_binding_still_exists() {
    let bound: Vec<String> = default_bindings()
        .into_iter()
        .map(|(keys, _)| davimci_keys::docs::render(&keys))
        .collect();
    for old in GRANDFATHERED {
        assert!(
            bound.contains(&(*old).to_string()),
            "'{old}' is grandfathered but no longer bound; drop it from the list"
        );
    }
}
