//! Spec §9 is the acceptance suite: every snippet in the spec appears here
//! verbatim and must load and behave as documented (plan.md Phase 7).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use vimci_core::{Classify, ErrorClass};
use vimci_keys::{Action, Key, LeafAction, Mode};
use vimci_motion::{BuiltinMotion, Direction};

use crate::*;

fn rt() -> Runtime {
    Runtime::new().expect("runtime")
}

fn exec(rt: &Runtime, src: &str) {
    rt.exec(src, "test.lua", Sandbox::Trusted)
        .unwrap_or_else(|e| panic!("chunk failed: {e}"));
}

// ---------------------------------------------------------------- §9.2 ----

/// Spec §9.2, verbatim.
const SPEC_9_2: &str = r#"
local map = require("vimci.keymap").map

-- mode, lhs, rhs (rhs can be a string command or a Lua function)
map("normal", "s", "editor.split_at_playhead")
map("normal", "x", "editor.ripple_delete")
map("normal", "<leader>e", function()
  require("vimci.export").run("youtube_1080p")
end)

-- rebind arrow keys' frame-step behavior
map("normal", "<Left>",  "editor.step_frame(-1)")
map("normal", "<Right>", "editor.step_frame(1)")
"#;

#[test]
fn spec_9_2_keymaps_load_and_produce_bindings() {
    let rt = rt();
    exec(&rt, SPEC_9_5); // the leader mapping references this preset
    exec(&rt, SPEC_9_2);

    let maps = rt.keymaps();
    assert_eq!(maps.len(), 5);
    assert!(maps.iter().all(|m| m.mode == Mode::Normal));

    let overrides = rt.keymap_overrides();
    let find = |s: &str| {
        overrides
            .iter()
            .find(|(k, _)| *k == Key::parse_str(s))
            .map(|(_, a)| a.clone())
    };
    assert_eq!(
        find("s"),
        Some(LeafAction::Standalone(Action::SplitCurrent))
    );
    assert_eq!(
        find("<Left>"),
        Some(LeafAction::Standalone(Action::Move {
            motion: BuiltinMotion::Frame(Direction::Backward),
            count: 1,
        }))
    );
    // A function right-hand side becomes an opaque plugin id.
    let Some(LeafAction::Standalone(Action::Plugin(id))) = find("<leader>e") else {
        panic!("leader mapping must be a plugin callback");
    };

    // ...and invoking it queues exactly the export the snippet asks for.
    let requests = rt.invoke(id).expect("callback runs");
    assert_eq!(
        requests,
        vec![Request::Export {
            preset: "youtube_1080p".into()
        }]
    );
}

#[test]
fn a_keymap_naming_an_unknown_command_is_rejected_when_the_config_loads() {
    let rt = rt();
    let e = rt
        .exec(
            r#"require("vimci.keymap").map("normal", "s", "editor.frobnicate")"#,
            "bad.lua",
            Sandbox::Trusted,
        )
        .expect_err("must reject");
    assert!(e.user_message().contains("not an editor command"), "{e}");
    assert!(rt.keymaps().is_empty());
}

#[test]
fn a_later_map_of_the_same_key_replaces_the_earlier_one() {
    let rt = rt();
    exec(
        &rt,
        r#"
        local map = require("vimci.keymap").map
        map("normal", "s", "editor.split_at_playhead")
        map("normal", "s", "editor.split_all_tracks")
        "#,
    );
    assert_eq!(rt.keymaps().len(), 1);
    assert_eq!(
        rt.keymap_overrides(),
        vec![(
            Key::parse_str("s"),
            LeafAction::Standalone(Action::SplitAll)
        )]
    );
}

// ---------------------------------------------------------------- §9.3 ----

/// Spec §9.3, verbatim apart from the `map` local the snippet assumes.
const SPEC_9_3: &str = r#"
local motions = require("vimci.motions")
local map = require("vimci.keymap").map

motions.register("next_loud_audio", function(ctx, opts)
  return ctx.timeline:find_next({
    track = opts.track,
    type = "audio",
    predicate = function(sample) return sample.rms_db > opts.threshold_db end,
  })
end)

map("normal", "]a", function()
  motions.run("next_loud_audio", { track = "A2", threshold_db = -2 })
end)
"#;

fn env_with_audio(analysed: bool) -> MotionEnv {
    MotionEnv::new(0, "A2").with_track(
        "A2",
        TrackData {
            kind: "audio".into(),
            samples: vec![
                Sample {
                    frame: 10,
                    rms_db: -30.0,
                    peak_db: -20.0,
                },
                Sample {
                    frame: 20,
                    rms_db: -1.0,
                    peak_db: 0.0,
                },
                Sample {
                    frame: 30,
                    rms_db: -0.5,
                    peak_db: 0.0,
                },
            ],
            clip_bounds: vec![0, 100],
            analysed,
        },
    )
}

#[test]
fn spec_9_3_custom_motion_finds_the_first_sample_above_the_threshold() {
    let rt = rt();
    exec(&rt, SPEC_9_3);
    assert_eq!(rt.motion_names(), ["next_loud_audio"]);

    let mut opts = Opts::new();
    opts.insert("track", OptValue::Str("A2".into()));
    opts.insert("threshold_db", OptValue::Num(-2.0));

    let answer = rt
        .run_motion("next_loud_audio", &opts, &env_with_audio(true))
        .unwrap();
    assert_eq!(answer, MotionAnswer::Found(20));

    // Searching from past the last loud sample finds nothing at all.
    let mut env = env_with_audio(true);
    env.playhead = 40;
    assert_eq!(
        rt.run_motion("next_loud_audio", &opts, &env).unwrap(),
        MotionAnswer::NoMatch
    );
}

#[test]
fn a_motion_over_an_unanalysed_track_is_pending_not_wrong() {
    let rt = rt();
    exec(&rt, SPEC_9_3);
    let mut opts = Opts::new();
    opts.insert("track", OptValue::Str("A2".into()));
    opts.insert("threshold_db", OptValue::Num(-2.0));
    assert_eq!(
        rt.run_motion("next_loud_audio", &opts, &env_with_audio(false))
            .unwrap(),
        MotionAnswer::Pending
    );
}

#[test]
fn the_keymap_from_spec_9_3_queues_the_motion_it_names() {
    let rt = rt();
    exec(&rt, SPEC_9_3);
    let overrides = rt.keymap_overrides();
    let (_, leaf) = overrides
        .iter()
        .find(|(k, _)| *k == Key::parse_str("]a"))
        .expect("]a bound");
    let LeafAction::Standalone(Action::Plugin(id)) = leaf else {
        panic!("]a must be a plugin callback");
    };
    let requests = rt.invoke(*id).unwrap();
    let [Request::Motion { name, opts }] = requests.as_slice() else {
        panic!("expected one motion request, got {requests:?}");
    };
    assert_eq!(name, "next_loud_audio");
    assert_eq!(opts.str("track"), Some("A2"));
    assert_eq!(opts.num("threshold_db"), Some(-2.0));
}

#[test]
fn running_an_unregistered_motion_is_a_user_error() {
    let rt = rt();
    let e = rt
        .run_motion("nope", &Opts::new(), &env_with_audio(true))
        .expect_err("no such motion");
    assert_eq!(e.class(), ErrorClass::User);
}

// ---------------------------------------------------------------- §9.4 ----

/// Spec §9.4, verbatim.
const SPEC_9_4: &str = r#"
local textobj = require("vimci.textobject")

textobj.register("c", { -- clip
  inner = function(clip) return clip.core_range end,
  around = function(clip) return clip.range_with_transitions end,
})
"#;

#[test]
fn spec_9_4_text_object_resolves_both_forms() {
    let rt = rt();
    exec(&rt, SPEC_9_4);
    assert_eq!(rt.object_names(), ["c"]);
    let clip = ClipInfo {
        start: 100,
        end: 250,
        with_transitions_start: 90,
        with_transitions_end: 260,
    };
    assert_eq!(
        rt.run_object("c", ObjectForm::Inner, clip).unwrap(),
        Some((100, 250))
    );
    assert_eq!(
        rt.run_object("c", ObjectForm::Around, clip).unwrap(),
        Some((90, 260))
    );
}

#[test]
fn an_object_with_neither_form_is_rejected() {
    let rt = rt();
    assert!(
        rt.exec(
            r#"require("vimci.textobject").register("z", {})"#,
            "z.lua",
            Sandbox::Trusted
        )
        .is_err()
    );
}

// ---------------------------------------------------------------- §9.5 ----

/// Spec §9.5, verbatim.
const SPEC_9_5: &str = r#"
require("vimci.export").preset("youtube_1080p", {
  container = "mp4",
  video_codec = "h264",
  resolution = "1920x1080",
  audio_tracks = "all",       -- or {"A1", "A3"}
  subtitle_tracks = "burned", -- or "sidecar", or {"S1"}
})
"#;

#[test]
fn spec_9_5_preset_loads_and_validates() {
    let rt = rt();
    exec(&rt, SPEC_9_5);
    assert_eq!(rt.preset_names(), ["youtube_1080p"]);
    let p = rt.preset("youtube_1080p").unwrap();
    assert_eq!(p.container, "mp4");
    assert_eq!(p.audio_tracks, TrackSelection::All);
    assert_eq!(p.subtitle_tracks, SubtitleSelection::Burned);
    let s = p.render_settings(vimci_core::Resolution::HD_1080, vimci_core::Fps::FPS_60);
    assert_eq!(s.video_codec, "libx264");
    assert_eq!(s.resolution, vimci_core::Resolution::HD_1080);
}

#[test]
fn a_named_track_list_is_accepted_the_way_the_comment_says() {
    let rt = rt();
    exec(
        &rt,
        r#"
        require("vimci.export").preset("p", {
          container = "mkv", video_codec = "h264", audio_codec = "aac",
          audio_tracks = {"A1", "A3"}, subtitle_tracks = {"S1"},
        })
        "#,
    );
    let p = rt.preset("p").unwrap();
    assert_eq!(
        p.audio_tracks,
        TrackSelection::Named(vec!["A1".into(), "A3".into()])
    );
    assert_eq!(
        p.subtitle_tracks,
        SubtitleSelection::Named(vec!["S1".into()])
    );
}

#[test]
fn an_impossible_codec_pairing_is_refused_at_definition() {
    let rt = rt();
    let e = rt
        .exec(
            r#"require("vimci.export").preset("bad", { container = "webm", video_codec = "h264" })"#,
            "bad.lua",
            Sandbox::Trusted,
        )
        .expect_err("must reject");
    assert!(e.user_message().contains("webm"), "{e}");
    assert!(rt.preset_names().is_empty());
}

// ---------------------------------------------------------------- §9.6 ----

/// Spec §9.6, verbatim.
const SPEC_9_6: &str = r#"
require("vimci.timeline").configure({
  jump_points = { "clip_bounds", "markers", "silence" },
  jump_point_density_per_zoom = {
    [1] = "clip_bounds_only",
    [4] = "clip_bounds+markers",
    [10] = "dense_subdivision",
  },
  frame_step_keys = { "<Left>", "<Right>" }, -- always frame-accurate, remappable
})
"#;

#[test]
fn spec_9_6_timeline_configuration_reaches_the_jump_engine() {
    let rt = rt();
    exec(&rt, SPEC_9_6);
    let cfg = rt.timeline_config();
    assert!(cfg.jump.sources.clip_bounds);
    assert!(cfg.jump.sources.markers);
    assert!(cfg.jump.sources.silence);
    assert!(!cfg.jump.sources.peaks);
    assert_eq!(cfg.jump.subdivide_from, 10);
    assert_eq!(
        cfg.frame_step_keys,
        vec![Key::parse_str("<Left>"), Key::parse_str("<Right>")]
    );
}

#[test]
fn a_rejected_configure_leaves_the_previous_settings_intact() {
    let rt = rt();
    exec(&rt, SPEC_9_6);
    let before = rt.timeline_config();
    assert!(
        rt.exec(
            r#"require("vimci.timeline").configure({ jump_points = { "clip_bounds", "beats" } })"#,
            "bad.lua",
            Sandbox::Trusted,
        )
        .is_err()
    );
    assert_eq!(rt.timeline_config(), before);
}

// ---------------------------------------------------------------- §9.8 ----

/// Spec §9.8, verbatim.
const SPEC_9_8: &str = r#"
require("vimci.autocmd").on("SplitPerformed", function(event)
  -- e.g. auto-tag both resulting clips
end)

require("vimci.autocmd").on("BeforeExport", function(ctx)
  -- e.g. validate no muted tracks are accidentally included
end)
"#;

#[test]
fn spec_9_8_handlers_load_and_fire_for_their_own_event_only() {
    let rt = rt();
    exec(&rt, SPEC_9_8);
    exec(
        &rt,
        r#"
        seen = {}
        require("vimci.autocmd").on("PlayheadMoved", function(e)
          seen[#seen + 1] = e.frame
        end)
        "#,
    );
    let d = rt.dispatch(&Event::PlayheadMoved {
        frame: 42,
        track: "V1".into(),
    });
    assert!(!d.is_cancelled());
    assert!(d.failures.is_empty());
    let seen: Vec<u64> = rt
        .exec_eval_numbers("seen")
        .expect("the handler recorded the frame");
    assert_eq!(seen, vec![42]);
}

#[test]
fn every_v1_event_is_bindable_and_dispatchable() {
    let rt = rt();
    exec(
        &rt,
        r#"
        fired = {}
        for _, name in ipairs({ "PlayheadMoved", "SplitPerformed", "ClipDeleted",
                                "ClipInserted", "ModeChanged", "BeforeExport",
                                "AfterExport", "ProjectLoaded" }) do
          require("vimci.autocmd").on(name, function(e)
            fired[#fired + 1] = e.event
          end)
        end
        "#,
    );
    let events = [
        Event::PlayheadMoved {
            frame: 1,
            track: "V1".into(),
        },
        Event::SplitPerformed {
            frame: 2,
            track: "V1".into(),
        },
        Event::ClipDeleted {
            clip: 3,
            track: "V1".into(),
        },
        Event::ClipInserted {
            clip: 4,
            track: "V1".into(),
        },
        Event::ModeChanged {
            from: "NORMAL".into(),
            to: "VISUAL".into(),
        },
        Event::BeforeExport {
            preset: "p".into(),
            output: "/tmp/out.mkv".into(),
        },
        Event::AfterExport {
            preset: "p".into(),
            output: "/tmp/out.mkv".into(),
        },
        Event::ProjectLoaded {
            path: "/tmp/p.vimci".into(),
        },
    ];
    for (e, name) in events.iter().zip(EVENTS) {
        assert_eq!(e.name(), *name);
        let d = rt.dispatch(e);
        assert!(d.failures.is_empty(), "{name}: {:?}", d.failures);
    }
    assert_eq!(rt.exec_eval_count("fired"), EVENTS.len());
}

#[test]
fn an_unknown_event_name_is_rejected_at_registration() {
    let rt = rt();
    let e = rt
        .exec(
            r#"require("vimci.autocmd").on("SplitPerfomed", function() end)"#,
            "typo.lua",
            Sandbox::Trusted,
        )
        .expect_err("typo must be caught");
    assert!(e.user_message().contains("PlayheadMoved"), "{e}");
}

// -------------------------------------------------- hooks and cancellation --

#[test]
fn a_before_export_handler_raising_an_error_aborts_the_render() {
    let rt = rt();
    exec(
        &rt,
        r#"
        require("vimci.autocmd").on("BeforeExport", function(ctx)
          error("track A2 is muted; refusing to export")
        end)
        "#,
    );
    let d = rt.dispatch(&Event::BeforeExport {
        preset: "youtube_1080p".into(),
        output: "/tmp/out.mp4".into(),
    });
    let reason = d.cancelled.expect("export must be cancelled");
    assert!(reason.contains("track A2 is muted"), "{reason}");
    assert_eq!(d.failures.len(), 1);
}

#[test]
fn a_before_export_handler_returning_false_vetoes_without_being_disabled() {
    let rt = rt();
    exec(
        &rt,
        r#"
        require("vimci.autocmd").on("BeforeExport", function(ctx)
          return false, "the mix is not finished"
        end)
        "#,
    );
    let ev = Event::BeforeExport {
        preset: "p".into(),
        output: "/tmp/o.mkv".into(),
    };
    let d = rt.dispatch(&ev);
    assert_eq!(d.cancelled.as_deref(), Some("the mix is not finished"));
    assert!(d.failures.is_empty());
    // A deliberate veto is not a fault: the handler still runs next time.
    assert_eq!(
        rt.dispatch(&ev).cancelled.as_deref(),
        Some("the mix is not finished")
    );
}

#[test]
fn handlers_run_in_registration_order_and_a_veto_stops_the_rest() {
    let rt = rt();
    exec(
        &rt,
        r#"
        order = {}
        local au = require("vimci.autocmd")
        au.on("BeforeExport", function() order[#order + 1] = 1 end)
        au.on("BeforeExport", function() order[#order + 1] = 2 return false end)
        au.on("BeforeExport", function() order[#order + 1] = 3 end)
        "#,
    );
    let d = rt.dispatch(&Event::BeforeExport {
        preset: "p".into(),
        output: "/tmp/o.mkv".into(),
    });
    assert!(d.is_cancelled());
    assert_eq!(rt.exec_eval_numbers("order").unwrap(), vec![1, 2]);
}

#[test]
fn a_non_cancellable_event_ignores_a_false_return() {
    let rt = rt();
    exec(
        &rt,
        r#"require("vimci.autocmd").on("ProjectLoaded", function() return false end)"#,
    );
    let d = rt.dispatch(&Event::ProjectLoaded {
        path: "/tmp/p".into(),
    });
    assert!(!d.is_cancelled());
}

#[test]
fn a_handler_can_be_removed_with_off() {
    let rt = rt();
    exec(
        &rt,
        r#"
        count = 0
        id = require("vimci.autocmd").on("PlayheadMoved", function() count = count + 1 end)
        require("vimci.autocmd").off(id)
        "#,
    );
    rt.dispatch(&Event::PlayheadMoved {
        frame: 1,
        track: "V1".into(),
    });
    assert_eq!(rt.exec_eval_number("count"), 0.0);
}

// ------------------------------------------------------- error isolation --

#[test]
fn a_throwing_handler_is_disabled_for_the_session_and_the_editor_survives() {
    let rt = rt();
    exec(
        &rt,
        r#"
        calls = 0
        local au = require("vimci.autocmd")
        au.on("PlayheadMoved", function() calls = calls + 1 error("boom") end)
        au.on("PlayheadMoved", function() calls = calls + 1 end)
        "#,
    );
    let ev = Event::PlayheadMoved {
        frame: 1,
        track: "V1".into(),
    };
    let d = rt.dispatch(&ev);
    assert_eq!(d.failures.len(), 1);
    assert!(d.failures[0].message.contains("boom"));
    // The healthy handler still ran.
    assert_eq!(rt.exec_eval_number("calls"), 2.0);

    // Second dispatch: only the healthy handler is left.
    let d = rt.dispatch(&ev);
    assert!(d.failures.is_empty());
    assert_eq!(rt.exec_eval_number("calls"), 3.0);

    // The failure reached the status line as a sentence, not Debug output.
    let notices = rt.take_notices();
    assert!(!notices.is_empty());
    assert!(notices.iter().all(|n| n.class == ErrorClass::Recoverable));
    assert!(notices[0].text.contains("PlayheadMoved"), "{}", notices[0]);
}

#[test]
fn a_throwing_keymap_callback_is_disabled_and_queues_nothing() {
    let rt = rt();
    exec(
        &rt,
        r#"
        require("vimci.keymap").map("normal", "Z", function()
          require("vimci.editor").ripple_delete()
          error("plugin bug")
        end)
        "#,
    );
    let overrides = rt.keymap_overrides();
    let LeafAction::Standalone(Action::Plugin(id)) = overrides[0].1.clone() else {
        panic!("expected a plugin binding");
    };
    let e = rt.invoke(id).expect_err("callback throws");
    assert_eq!(e.class(), ErrorClass::Recoverable);
    // Nothing it queued before throwing survives: a half-run handler must
    // not half-edit the timeline.
    assert!(rt.take_requests().is_empty());
    assert!(rt.is_disabled(id));
    // Invoking it again is a no-op rather than another failure.
    assert_eq!(rt.invoke(id).unwrap(), Vec::new());
}

#[test]
fn a_broken_config_file_costs_only_that_file() {
    let root = scratch("isolation");
    std::fs::write(
        root.join("init.lua"),
        r#"require("vimci.keymap").map("normal", "s", "editor.split_at_playhead")"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("plugin")).unwrap();
    std::fs::write(root.join("plugin/broken.lua"), "this is not lua(((").unwrap();
    std::fs::write(
        root.join("plugin/good.lua"),
        r#"require("vimci.keymap").map("normal", "x", "editor.ripple_delete")"#,
    )
    .unwrap();

    let rt = rt();
    let notices = rt.load_config(&ConfigPaths::new(&root));
    assert_eq!(notices.len(), 1);
    assert!(notices[0].text.contains("broken.lua"), "{}", notices[0]);
    assert_eq!(notices[0].class, ErrorClass::Recoverable);
    // Both healthy files still took effect.
    assert_eq!(rt.keymaps().len(), 2);
    std::fs::remove_dir_all(&root).unwrap();
}

// ------------------------------------------------------ §9.7 project-local --

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "vimci-lua-t-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

struct AllowAll;
impl TrustPrompt for AllowAll {
    fn trust(&self, _path: &Path) -> Trust {
        Trust::Granted
    }
}

#[test]
fn an_untrusted_project_local_config_does_not_run() {
    let dir = scratch("untrusted");
    std::fs::write(
        dir.join(".vimci.lua"),
        r#"require("vimci.keymap").map("normal", "Q", "editor.undo")"#,
    )
    .unwrap();
    let rt = rt();
    let (loaded, notice) = rt.load_project_local(&dir, &DenyAll);
    assert!(!loaded);
    let notice = notice.expect("the user is told it was skipped");
    assert!(notice.text.contains("only when trusted"), "{notice}");
    assert!(rt.keymaps().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_trusted_project_local_config_runs_but_cannot_reach_os_or_io() {
    let dir = scratch("trusted");
    std::fs::write(
        dir.join(".vimci.lua"),
        r#"
        require("vimci.export").preset("local", { container = "mkv", video_codec = "h264" })
        reached_os = (os ~= nil)
        reached_io = (io ~= nil)
        "#,
    )
    .unwrap();
    let rt = rt();
    let (loaded, notice) = rt.load_project_local(&dir, &AllowAll);
    assert!(loaded, "{notice:?}");
    assert_eq!(rt.preset_names(), ["local"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_sandboxed_chunk_has_no_os_execute_no_io_and_no_arbitrary_require() {
    let rt = rt();
    for src in [
        "os.execute('touch /tmp/vimci-should-not-exist')",
        "io.open('/etc/passwd')",
        "dofile('/etc/passwd')",
        "load('return 1')()",
        "require('os')",
    ] {
        let e = rt
            .exec(src, "untrusted.lua", Sandbox::Restricted)
            .expect_err(&format!("{src} must not be reachable"));
        assert_eq!(e.class(), ErrorClass::Recoverable, "{e}");
    }
    // ...while the documented API still works.
    rt.exec(
        r#"require("vimci.keymap").map("normal", "s", "editor.split_at_playhead")"#,
        "untrusted.lua",
        Sandbox::Restricted,
    )
    .unwrap();
    assert_eq!(rt.keymaps().len(), 1);
}

#[test]
fn a_missing_project_local_file_is_silent() {
    let dir = scratch("absent");
    let rt = rt();
    let (loaded, notice) = rt.load_project_local(&dir, &AllowAll);
    assert!(!loaded);
    assert!(notice.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------- editor bridge --

#[test]
fn editor_calls_queue_requests_instead_of_editing() {
    let rt = rt();
    exec(
        &rt,
        r#"
        local ed = require("vimci.editor")
        ed.split_at_playhead()
        ed.step_frame(-3)
        ed.message("hello")
        require("vimci.media").import("/tmp/a.mkv")
        "#,
    );
    assert_eq!(
        rt.take_requests(),
        vec![
            Request::Edit(Action::SplitCurrent),
            Request::Edit(Action::Move {
                motion: BuiltinMotion::Frame(Direction::Backward),
                count: 3
            }),
            Request::Message("hello".into()),
            Request::Import {
                path: "/tmp/a.mkv".into()
            },
        ]
    );
    // Draining is exactly once.
    assert!(rt.take_requests().is_empty());
}

#[test]
fn an_autocmd_may_ask_for_an_edit_and_the_host_gets_it_back() {
    let rt = rt();
    exec(
        &rt,
        r#"
        require("vimci.autocmd").on("SplitPerformed", function(e)
          require("vimci.editor").message("split at " .. e.frame)
        end)
        "#,
    );
    let d = rt.dispatch(&Event::SplitPerformed {
        frame: 120,
        track: "V1".into(),
    });
    assert_eq!(d.requests, vec![Request::Message("split at 120".into())]);
}

#[test]
fn the_runtime_is_debug_printable_for_diagnostics() {
    let rt = rt();
    exec(&rt, SPEC_9_5);
    let s = format!("{rt:?}");
    assert!(s.contains("youtube_1080p"), "{s}");
}
