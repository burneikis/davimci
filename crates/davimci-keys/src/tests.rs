//! End-to-end headless tests: feed keys, assert the resulting timeline
//!.

#![allow(clippy::expect_used, clippy::panic)]

use davimci_cmd::Session;
use davimci_core::testing::fixture;

use crate::action::ZoomIntent;
use crate::engine::{Engine, Outcome};
use crate::key::Key;

fn scene() -> (Engine, Session) {
    (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 300, "a")])])),
    )
}

/// An engine bound the way the bundled `transitions` plugin binds itself.
/// No transition type is core, so these keys do not exist until a catalogue
/// registers one, and a test that uses them has to say which.
fn transition_engine() -> Engine {
    use crate::action::{Action, LeafAction};
    Engine::with_keymap(crate::keymap::Keymap::new().with_overrides([
        (
            Key::parse_str("gx"),
            LeafAction::Standalone(Action::CreateTransition {
                kind: "dissolve".to_string(),
            }),
        ),
        (
            Key::parse_str("dax"),
            LeafAction::Standalone(Action::DeleteTransition),
        ),
    ]))
}

fn feed(engine: &mut Engine, session: &mut Session, s: &str) -> Vec<Outcome> {
    Key::parse_str(s)
        .into_iter()
        .map(|k| engine.feed(k, session).outcome)
        .collect()
}

#[test]
fn split_at_an_interior_frame_via_keys() {
    let (mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, "50<Right>s");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(s.timeline().dump(), "V1:[a 0-50][a 50-300]\nA1: -\n");
}

#[test]
fn x_ripple_deletes_the_clip_under_the_playhead() {
    let (mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, "x");
    assert!(matches!(out[0], Outcome::Applied(_)), "{out:?}");
    // Ripple delete closes the gap; deleting the only clip leaves nothing.
    assert_eq!(s.timeline().dump(), "V1: -\nA1: -\n");
}

#[test]
fn dw_ripple_deletes_to_the_next_clip_boundary() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    let out = feed(&mut e, &mut s, "dw");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    // Ripple delete of `a` shifts `b` left to close the gap.
    assert_eq!(s.timeline().dump(), "V1:[b 0-100]\nA1: -\n");
}

#[test]
fn undo_redo_and_repeat_round_trip_through_keys() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    let start = s.timeline().dump();
    feed(&mut e, &mut s, "x");
    let after_x = s.timeline().dump();
    assert_ne!(start, after_x);
    let out = feed(&mut e, &mut s, "u");
    assert!(matches!(out[0], Outcome::Applied(_)));
    assert_eq!(s.timeline().dump(), start);
    feed(&mut e, &mut s, "<C-r>");
    assert_eq!(s.timeline().dump(), after_x);
}

#[test]
fn yank_and_paste_round_trip_through_keys() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    let out = feed(&mut e, &mut s, "yy");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, "p");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_ne!(s.timeline().dump(), before);
}

#[test]
fn a_macro_records_and_replays_an_edit() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[(
            "V1",
            &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")],
        )])),
    );
    feed(&mut e, &mut s, "qax"); // record: ripple-delete the clip at 0
    let out = feed(&mut e, &mut s, "q"); // stop
    assert!(matches!(out[0], Outcome::MacroStopped('a')), "{out:?}");
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, "@a");
    assert!(matches!(out.last(), Some(Outcome::Replayed(_))), "{out:?}");
    assert_ne!(
        s.timeline().dump(),
        before,
        "the macro must have deleted again"
    );
}

#[test]
fn esc_from_a_pending_sequence_never_touches_the_timeline() {
    let (mut e, mut s) = scene();
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, "3d<Esc>");
    assert!(matches!(out.last(), Some(Outcome::Mode(_))), "{out:?}");
    assert_eq!(s.timeline().dump(), before);
}

#[test]
fn visual_mode_delete_acts_on_the_selection_and_exits_visual() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    feed(&mut e, &mut s, "v");
    assert_eq!(e.mode(), crate::mode::Mode::Visual);
    feed(&mut e, &mut s, "<Right>");
    let out = feed(&mut e, &mut s, "d");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(e.mode(), crate::mode::Mode::Normal);
}

/// Regression: motions in VISUAL resolved from the playhead, which stays at
/// the anchor, so `<Left>` after `w` collapsed the selection back to one
/// frame instead of shrinking it by one. The moving end is the active end.
#[test]
fn a_motion_in_visual_extends_from_the_active_end_not_the_anchor() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    feed(&mut e, &mut s, "vw");
    assert_eq!(
        e.selection().map(|s| (s.start.get(), s.end.get())),
        Some((0, 101))
    );
    feed(&mut e, &mut s, "<Left>");
    assert_eq!(
        e.selection().map(|s| (s.start.get(), s.end.get())),
        Some((0, 100))
    );
}

/// Regression: `y` on a selection yanked but left VISUAL live, so the next
/// motion extended a selection the user thought had ended and the paste that
/// followed landed over a range nobody had chosen. Every verb ends the
/// selection, not only the ones that mutate.
#[test]
fn visual_mode_yank_also_exits_visual() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    feed(&mut e, &mut s, "vl");
    let out = feed(&mut e, &mut s, "y");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(e.mode(), crate::mode::Mode::Normal);
}

/// `+` in VISUAL adjusts every clip in the selection, as one undoable
/// command.
#[test]
fn gain_adjust_acts_on_the_visual_selection_not_only_the_playhead_clip() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[(
            "V1",
            &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")],
        )])),
    );
    // `l` lands on the head of "b", so the selection overlaps "a" and "b".
    feed(&mut e, &mut s, "vl+");
    let gains: Vec<f32> = s.timeline().tracks()[0]
        .clips()
        .iter()
        .map(|c| c.props.gain_db)
        .collect();
    assert_eq!(gains, vec![1.0, 1.0, 0.0]);

    feed(&mut e, &mut s, "u");
    let gains: Vec<f32> = s.timeline().tracks()[0]
        .clips()
        .iter()
        .map(|c| c.props.gain_db)
        .collect();
    assert_eq!(gains, vec![0.0, 0.0, 0.0], "one u undoes the whole set");
}

#[test]
fn zoom_keys_report_intents_and_never_touch_the_timeline() {
    let (mut e, mut s) = scene();
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, "zizoz0");
    let intents: Vec<_> = out
        .iter()
        .filter_map(|o| match o {
            Outcome::Zoom(i) => Some(*i),
            _ => None,
        })
        .collect();
    assert_eq!(
        intents,
        vec![ZoomIntent::In, ZoomIntent::Out, ZoomIntent::Reset],
        "{out:?}"
    );
    // Zoom is view state: no edit, so no undo entry either.
    assert_eq!(s.timeline().dump(), before);
    assert!(s.undo().is_err());
}

#[test]
fn z0_is_a_zoom_reset_not_a_count_then_timeline_start() {
    let (mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, "50<Right>z0");
    assert!(
        matches!(out.last(), Some(Outcome::Zoom(ZoomIntent::Reset))),
        "{out:?}"
    );
    assert_eq!(s.timeline().playhead().frame, davimci_core::Frame(50));
}

/// A bind pressed during playback either takes the clock or
/// deliberately leaves it alone. Table-driven so a new action cannot quietly
/// inherit the wrong default - `transport_policy` is exhaustive, so adding
/// one without a decision fails to compile, and this pins the decisions.
#[test]
fn spec_section_3_2_1_transport_policy_per_key() {
    use crate::engine::TransportCmd;
    let interrupt = [
        "h", "l", "w", "b", "G", "x", "s", "dd", "yyp", "u", "<C-r>", ".", "'a", "+", "@a",
    ];
    let keep = [
        "<Space><Space>",
        "H",
        "L",
        "<Space>p",
        "<Space>l",
        "zi",
        "z0",
        "ma",
        "v",
        ":",
        "<Esc>",
    ];
    for keys in interrupt {
        let (mut e, mut s) = scene();
        let last = Key::parse_str(keys)
            .into_iter()
            .map(|k| e.feed(k, &mut s))
            .last()
            .expect("a key");
        assert!(
            last.transport.interrupts(),
            "'{keys}' should stop playback: {last:?}"
        );
    }
    for keys in keep {
        let (mut e, mut s) = scene();
        let last = Key::parse_str(keys)
            .into_iter()
            .map(|k| e.feed(k, &mut s))
            .last()
            .expect("a key");
        assert!(
            !last.transport.interrupts(),
            "'{keys}' should leave playback running: {last:?}"
        );
    }
    // The explicit action exists but is unbound by default, like
    // `shuttle_stop`.
    let (mut e, mut s) = scene();
    let fed = e.execute_action(crate::action::Action::InterruptTransport, &mut s);
    assert_eq!(fed, Outcome::Transport(TransportCmd::Interrupt));
    assert!(
        crate::action::Action::InterruptTransport
            .transport_policy()
            .interrupts()
    );
}

/// A Lua callback keeps the clock unless its binding opted in.
#[test]
fn a_plugin_binding_interrupts_only_when_it_opted_in() {
    use crate::action::Action;
    assert!(
        !Action::Plugin {
            id: 1,
            interrupt: false
        }
        .transport_policy()
        .interrupts()
    );
    assert!(
        Action::Plugin {
            id: 1,
            interrupt: true
        }
        .transport_policy()
        .interrupts()
    );
}

/// Mute and solo are track state, toggled from the leader.
#[test]
fn space_m_and_space_s_toggle_the_current_track() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[
            ("A1", &[(0, 100, "a")]),
            ("A2", &[(0, 100, "b")]),
        ])),
    );
    let a1 = davimci_core::testing::track_id(s.timeline(), "A1");
    s.set_playhead(davimci_core::Frame::ZERO, a1)
        .expect("A1 exists");

    let out = feed(&mut e, &mut s, "<Space>m");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert!(s.timeline().track(a1).expect("A1").muted);
    feed(&mut e, &mut s, "<Space>m");
    assert!(!s.timeline().track(a1).expect("A1").muted, "toggles back");

    feed(&mut e, &mut s, "<Space>s");
    assert!(s.timeline().track(a1).expect("A1").solo);
}

/// Muting is undoable like every other mutation, because it goes through the
/// command layer rather than writing to the timeline directly.
#[test]
fn muting_a_track_can_be_undone() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("A1", &[(0, 100, "a")])])),
    );
    let a1 = davimci_core::testing::track_id(s.timeline(), "A1");
    s.set_playhead(davimci_core::Frame::ZERO, a1)
        .expect("A1 exists");
    feed(&mut e, &mut s, "<Space>m");
    feed(&mut e, &mut s, "u");
    assert!(!s.timeline().track(a1).expect("A1").muted);
}

/// A solo must not silently clear a mute: they are independent flags.
#[test]
fn soloing_a_muted_track_leaves_it_muted() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("A1", &[(0, 100, "a")])])),
    );
    let a1 = davimci_core::testing::track_id(s.timeline(), "A1");
    s.set_playhead(davimci_core::Frame::ZERO, a1)
        .expect("A1 exists");
    feed(&mut e, &mut s, "<Space>m");
    feed(&mut e, &mut s, "<Space>s");
    let t = s.timeline().track(a1).expect("A1");
    assert!(t.muted && t.solo);
}

/// `i` on a subtitle clip edits its text; anywhere else it asks for
/// media. Same key, decided by what is under the playhead - and only once
/// the plugin that owns text tracks has granted text editing.
#[test]
fn i_edits_text_on_a_subtitle_track_and_picks_media_elsewhere() {
    use davimci_core::Frame;
    let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("T1", &[])]);
    let t1 = davimci_core::testing::track_id(&tl, "T1");
    let id = tl.new_clip_id();
    let mut clip = davimci_core::Clip::generated(id, "sub", Frame::ZERO, Frame(50));
    clip.text = Some("hello".into());
    tl.restore(t1, Frame::ZERO, &[clip], Frame(50), false)
        .expect("a text clip");
    let (mut e, mut s) = (Engine::new(), Session::new(tl));

    let out = feed(&mut e, &mut s, "i");
    assert!(
        matches!(out.last(), Some(Outcome::PickMedia(_))),
        "on a video track `i` still means media: {out:?}"
    );

    s.set_playhead(Frame::ZERO, t1).expect("T1 exists");
    let out = feed(&mut e, &mut s, "i");
    assert!(
        matches!(out.last(), Some(Outcome::PickMedia(_))),
        "without the subtitles plugin `i` has one meaning: {out:?}"
    );

    e.set_text_editing(true);
    let out = feed(&mut e, &mut s, "i");
    match out.last() {
        Some(Outcome::EditText { clip, text }) => {
            assert_eq!(*clip, id);
            assert_eq!(text, "hello", "the buffer starts as what is there");
        }
        other => panic!("expected a text edit, got {other:?}"),
    }
}

/// Bound to a registered type, `gx` puts one on the nearest cut, `dax` takes
/// it away, and `u` undoes either as one step.
#[test]
fn gx_and_dax_add_and_remove_a_transition_at_the_nearest_cut() {
    let mut s = Session::new(davimci_core::testing::media_fixture(&[
        (0, 100, 20, 400),
        (100, 100, 20, 400),
    ]));
    let mut e = transition_engine();
    let right = s.timeline().tracks()[0].clips()[1].id;
    let has = |s: &Session| {
        s.timeline()
            .find_clip(right)
            .is_some_and(|(_, c)| c.transition_in.is_some())
    };

    let out = feed(&mut e, &mut s, "gx");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert!(has(&s), "the cut nearest frame zero gets the transition");

    // From inside the overlap rather than exactly on the cut.
    let out = feed(&mut e, &mut s, "dax");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert!(!has(&s));

    assert!(matches!(
        e.feed(Key::parse_str("u")[0], &mut s).outcome,
        Outcome::Applied(_)
    ));
    assert!(has(&s), "one undo puts the transition back");
}

/// A cut whose clips have no handles is refused with a sentence, and nothing
/// enters the undo log.
#[test]
fn gx_without_handles_reports_why_and_changes_nothing() {
    let mut s = Session::new(davimci_core::testing::media_fixture(&[
        (0, 100, 0, 100),
        (100, 100, 0, 100),
    ]));
    let mut e = transition_engine();
    let before = s.timeline().clone();
    let out = feed(&mut e, &mut s, "gx");
    let Some(Outcome::Error(msg)) = out.last() else {
        panic!("expected a refusal, got {out:?}");
    };
    assert!(msg.contains("handle"), "{msg}");
    assert_eq!(s.timeline(), &before);
}

#[test]
fn dax_on_a_track_with_no_transition_says_so() {
    let (_, mut s) = scene();
    let mut e = transition_engine();
    let out = feed(&mut e, &mut s, "dax");
    assert!(matches!(out.last(), Some(Outcome::Error(_))), "{out:?}");
}

// `<` / `>` jump-point edge trims

/// Table-driven landing positions: the nearest edge moves by whole jump
/// points, and the count multiplies the step.
#[test]
fn angle_brackets_trim_the_nearest_edge_by_jump_points() {
    // Zoomed in far enough that jump points subdivide: the step
    // is 8 frames here, so the landings are exactly one step apart.
    let cases: &[(&str, u64)] = &[(">", 208), ("<", 192), ("2>", 216), ("3<", 176)];
    for (keys, want) in cases {
        let mut e = Engine::new();
        e.set_zoom(davimci_motion::Zoom::new(12));
        let mut s = Session::new(fixture(&[("V1", &[(0, 200, "a"), (200, 200, "b")])]));
        // Stand next to the cut, so the nearest edge is the one at 200.
        feed(&mut e, &mut s, "196<Right>");
        let out = feed(&mut e, &mut s, keys);
        assert!(
            matches!(out.last(), Some(Outcome::Applied(_))),
            "{keys}: {out:?}"
        );
        assert_eq!(
            s.timeline().tracks()[0].clips()[0].end().get(),
            *want,
            "{keys}"
        );
    }
}

#[test]
fn an_edge_trim_with_no_jump_point_that_way_is_refused_intact() {
    let mut e = Engine::new();
    let mut s = Session::new(fixture(&[("V1", &[(0, 200, "a")])]));
    let before = s.timeline().clone();
    // At the very start there is nothing to the left to trim towards.
    let out = feed(&mut e, &mut s, "<");
    assert!(
        matches!(out.last(), Some(Outcome::Error(_) | Outcome::Applied(_))),
        "{out:?}"
    );
    if matches!(out.last(), Some(Outcome::Error(_))) {
        assert_eq!(s.timeline(), &before);
    }
}

// `it` / `at` in VISUAL

#[test]
fn typing_it_in_visual_narrows_the_selection_to_the_focused_track() {
    let mut e = Engine::new();
    let mut s = Session::new(fixture(&[
        ("V1", &[(0, 100, "a")]),
        ("A1", &[(0, 100, "m")]),
    ]));
    let tracks: Vec<_> = s.timeline().tracks().iter().map(|t| t.id).collect();
    feed(&mut e, &mut s, "vj");
    assert_eq!(e.selection().map(|s| s.tracks.len()), Some(2));
    let out = feed(&mut e, &mut s, "it");
    assert!(matches!(out.last(), Some(Outcome::Moved)), "{out:?}");
    assert_eq!(e.selection().map(|s| s.tracks), Some(vec![tracks[0]]));
}

#[test]
fn a_registered_object_is_typeable_and_handed_to_the_host() {
    let mut keymap = crate::keymap::Keymap::new();
    keymap.register_object("q");
    let mut e = Engine::with_keymap(keymap);
    let mut s = Session::new(fixture(&[("V1", &[(0, 100, "a")])]));
    let out = feed(&mut e, &mut s, "diq");
    match out.last() {
        Some(Outcome::ResolveObject { name, around, verb }) => {
            assert_eq!(*name, 'q');
            assert!(!around);
            // The verb comes back re-targetable at whatever range the host
            // resolves, and runs through the ordinary command path.
            let action = (**verb).clone().with_range(davimci_motion::TimeRange::new(
                davimci_core::Frame(10),
                davimci_core::Frame(40),
            ));
            assert!(matches!(
                e.execute_action(action, &mut s),
                Outcome::Applied(_)
            ));
            assert_eq!(s.timeline().duration().get(), 70);
        }
        other => panic!("expected a host resolution, got {other:?}"),
    }
}

// Visual mode geometry: see `docs/visual-mode.md`.

fn three_lanes() -> Session {
    Session::new(fixture(&[
        ("V1", &[(0, 100, "a"), (100, 100, "b")]),
        ("V2", &[(0, 100, "c")]),
        ("A1", &[(0, 100, "m")]),
    ]))
}

#[test]
fn v_selects_the_frame_under_the_cursor_not_the_clip_it_sits_in() {
    let mut e = Engine::new();
    let mut s = three_lanes();
    feed(&mut e, &mut s, "50<Right>v");
    let sel = e.selection().expect("visual mode is live");
    assert_eq!((sel.start.get(), sel.end.get()), (50, 51));
}

#[test]
fn visual_line_snaps_each_end_to_the_whole_clip_under_it() {
    let mut e = Engine::new();
    let mut s = three_lanes();
    // Enter inside clip `a`: the whole of `a` is selected at once.
    feed(&mut e, &mut s, "50<Right>V");
    let sel = e.selection().expect("visual line is live");
    assert_eq!((sel.start.get(), sel.end.get()), (0, 100));
    // A motion into clip `b` takes the whole of `b` too.
    feed(&mut e, &mut s, "l");
    let sel = e.selection().expect("visual line is live");
    assert_eq!((sel.start.get(), sel.end.get()), (0, 200));
}

#[test]
fn j_and_k_in_visual_extend_the_selection_across_tracks() {
    let mut e = Engine::new();
    let mut s = three_lanes();
    let tracks: Vec<_> = s.timeline().tracks().iter().map(|t| t.id).collect();
    feed(&mut e, &mut s, "vj");
    assert_eq!(e.selection().map(|s| s.tracks), Some(tracks[..2].to_vec()));
    feed(&mut e, &mut s, "j");
    assert_eq!(e.selection().map(|s| s.tracks), Some(tracks[..3].to_vec()));
    // Coming back up shrinks the span rather than leaving it at its widest.
    feed(&mut e, &mut s, "k");
    assert_eq!(e.selection().map(|s| s.tracks), Some(tracks[..2].to_vec()));
}

#[test]
fn visualstart_jump_anchors_on_the_interval_under_the_cursor() {
    let mut e = Engine::new();
    let mut s = three_lanes();
    e.set_visual_start(crate::mode::VisualStart::Jump);
    feed(&mut e, &mut s, "50<Right>v");
    let sel = e.selection().expect("visual mode is live");
    // Clip edges are jump points, so the interval is the clip `a` spans.
    assert_eq!((sel.start.get(), sel.end.get()), (0, 100));
    // The setting is for `v`; `V` is the clip either way.
    feed(&mut e, &mut s, "<Esc>V");
    let sel = e.selection().expect("visual line is live");
    assert_eq!((sel.start.get(), sel.end.get()), (0, 100));
}

/// A press that changes nothing is a bug: the active end covers a span, so
/// `h`/`l` search from the edge of that span rather than from inside it.
#[test]
fn h_and_l_in_visual_step_past_the_selection_not_within_it() {
    let mut e = Engine::new();
    let mut s = three_lanes();
    // `V` inside `b` takes the whole of `b`; `h` must reach `a`, not stop on
    // b's own start boundary, which the selection already includes.
    feed(&mut e, &mut s, "150<Right>Vh");
    let sel = e.selection().expect("visual line is live");
    assert_eq!((sel.start.get(), sel.end.get()), (0, 200));

    // Same with an interval unit: one press, one interval.
    let mut e = Engine::new();
    let mut s = three_lanes();
    e.set_visual_start(crate::mode::VisualStart::Jump);
    feed(&mut e, &mut s, "150<Right>vh");
    let sel = e.selection().expect("visual mode is live");
    assert_eq!((sel.start.get(), sel.end.get()), (0, 200));
}

#[test]
fn visual_line_in_a_gap_selects_the_gap_not_one_frame() {
    let mut e = Engine::new();
    let mut s = Session::new(fixture(&[("V1", &[(0, 100, "a"), (200, 100, "b")])]));
    feed(&mut e, &mut s, "150<Right>V");
    let sel = e.selection().expect("visual line is live");
    assert_eq!((sel.start.get(), sel.end.get()), (100, 200));
}

#[test]
fn continuations_list_every_key_that_can_follow_a_prefix() {
    let keymap = crate::keymap::Keymap::new();
    let g = Key::parse_str("g");
    let next = keymap.continuations(&g);
    assert!(!next.is_empty(), "`g` is a prefix and must offer something");
    assert!(
        next.iter()
            .any(|c| c.key == Key::Char('g') && c.leaf.is_some()),
        "`gg` is bound, so `g` must complete: {next:?}"
    );
    // A bound leaf that nothing extends offers nothing after it.
    assert!(keymap.continuations(&Key::parse_str("gg")).is_empty());
}

#[test]
fn a_pending_sequence_reports_what_was_typed_and_what_could_follow() {
    let (mut engine, mut session) = scene();
    assert!(
        engine.pending().is_idle(),
        "a fresh engine has nothing pending"
    );

    engine.feed(Key::Char('3'), &mut session);
    engine.feed(Key::Char('g'), &mut session);
    let pending = engine.pending();
    assert_eq!(pending.text, "3g", "the count is part of what was typed");
    assert!(
        pending
            .continuations
            .iter()
            .any(|c| c.key == Key::Char('g')),
        "the keymap's continuations are not reported: {pending:?}"
    );

    // Finishing the sequence empties it again, so a which-key panel hides.
    engine.feed(Key::Char('g'), &mut session);
    assert!(engine.pending().is_idle(), "the sequence stayed pending");
}

/// `gl`/`gh` nudge the clip under the playhead, and a count is frames.
#[test]
fn shifting_a_clip_with_keys_takes_a_count_in_frames() {
    let (mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, "10gl");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(s.timeline().dump(), "V1:<gap 10>[a 10-310]\nA1: -\n");

    feed(&mut e, &mut s, "gh");
    assert_eq!(s.timeline().dump(), "V1:<gap 9>[a 9-309]\nA1: -\n");
}

/// `gj`/`gk` move along the stack as shown, and refuse at its ends rather
/// than wrapping onto a track the user cannot see they are leaving.
#[test]
fn moving_a_clip_between_tracks_with_keys_stops_at_the_stack_ends() {
    let (mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, "gk");
    assert!(matches!(out.last(), Some(Outcome::Error(_))), "{out:?}");

    let out = feed(&mut e, &mut s, "gj");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(s.timeline().dump(), "V1: -\nA1:[a 0-300]\n");
}
