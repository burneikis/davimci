//! `docs/keymap.md` is generated, and this is what stops it drifting.
//!
//! `just docs` regenerates it; anything else that changes a binding fails
//! here until the document is regenerated with it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/keymap.md")
}

#[test]
fn the_documented_keymap_matches_the_table() {
    let generated = davimci_keys::docs::keymap_markdown();
    let path = doc_path();

    if std::env::var_os("DAVIMCI_UPDATE_DOCS").is_some() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&path, &generated).unwrap();
        return;
    }

    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} is missing; run `just docs`", path.display()));
    assert_eq!(
        on_disk, generated,
        "docs/keymap.md is out of date; run `just docs`"
    );
}
