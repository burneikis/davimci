//! Lua-driven integration test (plan.md Phase 7): a config file registers a
//! motion and keymaps, the harness feeds keys through `davimci-keys`, and the
//! assertion is the resulting timeline.
//!
//! This is the test that proves the whole path - config file, keymap table,
//! plugin callback, request queue, command layer - and that Lua reaches the
//! timeline only through a `Command`, so the edit is undoable like any other.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_keys::{Engine, Key, Keymap, Outcome};
use davimci_lua::{MotionAnswer, MotionEnv, Opts, Request, Runtime, Sample, Sandbox, TrackData};

const CONFIG: &str = r#"
local map = require("davimci.keymap").map
local motions = require("davimci.motions")

-- A plain command binding: `S` splits, `X` ripple-deletes.
map("normal", "S", "editor.split_at_playhead")
map("normal", "X", "editor.ripple_delete")

-- A function binding that asks for two edits at once.
map("normal", "Z", function()
  local ed = require("davimci.editor")
  ed.split_at_playhead()
  ed.message("split from lua")
end)

motions.register("next_loud_audio", function(ctx, opts)
  return ctx.timeline:find_next({
    track = opts.track,
    type = "audio",
    predicate = function(sample) return sample.rms_db > opts.threshold_db end,
  })
end)

map("normal", "]a", function()
  motions.run("next_loud_audio", { track = "A1", threshold_db = -2 })
end)
"#;

/// Feed keys, running any plugin callback the engine reports back, exactly
/// as a frontend would.
fn feed(engine: &mut Engine, session: &mut Session, rt: &Runtime, keys: &str) -> Vec<Outcome> {
    let mut out = Vec::new();
    for k in Key::parse_str(keys) {
        let o = engine.feed(k, session).outcome;
        if let Outcome::Plugin(id) = o {
            for req in rt.invoke(id).unwrap_or_default() {
                if let Request::Edit(action) = req {
                    out.push(engine.execute_action(action, session));
                } else {
                    out.push(Outcome::Applied(format!("{req:?}")));
                }
            }
        } else {
            out.push(o);
        }
    }
    out
}

fn scene() -> (Runtime, Engine, Session) {
    let rt = Runtime::new().unwrap();
    rt.exec(CONFIG, "init.lua", Sandbox::Trusted).unwrap();
    let engine = Engine::with_keymap(Keymap::new().with_overrides(rt.keymap_overrides()));
    let session = Session::new(fixture(&[("V1", &[(0, 300, "a")])]));
    (rt, engine, session)
}

#[test]
fn a_lua_command_binding_edits_the_timeline() {
    let (rt, mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, &rt, "50<Right>S");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(s.timeline().dump(), "V1:[a 0-50][a 50-300]\nA1: -\n");
}

#[test]
fn a_lua_callback_binding_edits_the_timeline_and_is_undoable() {
    let (rt, mut e, mut s) = scene();
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, &rt, "50<Right>Z");
    assert!(
        out.iter().any(|o| matches!(o, Outcome::Applied(_))),
        "{out:?}"
    );
    assert_eq!(s.timeline().dump(), "V1:[a 0-50][a 50-300]\nA1: -\n");

    // The edit went through a Command, so `u` restores the timeline exactly.
    s.undo().expect("undo");
    assert_eq!(s.timeline().dump(), before);
}

#[test]
fn the_default_binding_survives_being_mapped_around() {
    // `s` still splits: layering overrides must not disturb the defaults.
    let (rt, mut e, mut s) = scene();
    feed(&mut e, &mut s, &rt, "50<Right>s");
    assert_eq!(s.timeline().dump(), "V1:[a 0-50][a 50-300]\nA1: -\n");
}

#[test]
fn a_lua_motion_bound_to_a_key_resolves_against_the_analysis_snapshot() {
    let (rt, mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, &rt, "]a");
    // The binding asks the host to run the motion; the host owns the index.
    let Some(Outcome::Applied(desc)) = out.last() else {
        panic!("expected a queued motion request, got {out:?}");
    };
    assert!(desc.contains("next_loud_audio"), "{desc}");

    let env = MotionEnv::new(0, "A1").with_track(
        "A1",
        TrackData {
            kind: "audio".into(),
            samples: vec![
                Sample {
                    frame: 30,
                    rms_db: -40.0,
                    peak_db: -30.0,
                },
                Sample {
                    frame: 90,
                    rms_db: -1.5,
                    peak_db: 0.0,
                },
            ],
            clip_bounds: vec![0, 300],
            analysed: true,
        },
    );
    let mut opts = Opts::new();
    opts.insert("track", davimci_lua::OptValue::Str("A1".into()));
    opts.insert("threshold_db", davimci_lua::OptValue::Num(-2.0));
    assert_eq!(
        rt.run_motion("next_loud_audio", &opts, &env).unwrap(),
        MotionAnswer::Found(90)
    );
}
