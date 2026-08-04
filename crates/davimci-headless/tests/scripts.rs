//! Every scripted session in `tests/sessions/` is a test.
//!
//! Adding a case means adding a `.dvs` file - which is also the file a bug
//! report can carry and `davimci --script` can replay.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, NullHost};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_headless::Script;

fn session() -> Session {
    Session::new(fixture(&[
        ("V1", &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")]),
        ("A1", &[(0, 300, "music")]),
    ]))
}

#[test]
fn every_session_script_passes() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sessions");
    let mut ran = 0;
    for entry in std::fs::read_dir(&dir).expect("the sessions directory should exist") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "dvs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let script = Script::parse(&source)
            .unwrap_or_else(|e| panic!("{}: {e}", path.file_name().unwrap().to_string_lossy()));
        let report = script.run(&mut App::new(session()), &mut NullHost);
        assert!(report.passed(), "{}:\n{}", path.display(), report.summary());
        ran += 1;
    }
    assert!(ran >= 2, "only {ran} scripts ran; the glob is wrong");
}
