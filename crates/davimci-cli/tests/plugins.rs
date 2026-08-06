//! User Lua config, driven through the assembled editor.
//!
//! The point of these is the seam, not the runtime: `davimci-lua` is already
//! tested in `davimci-lua`. What is asserted here is that a
//! config the user wrote reaches a running editor - its keymaps into the
//! grammar, its presets into `:export`, its callbacks into the undo log.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use davimci_app::{App, Event};
use davimci_backend::MockBackend;
use davimci_cli::{Editor, Plugins, Workspace};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_core::{Fps, Resolution, Timeline};
use davimci_keys::Key;
use davimci_lua::{ConfigPaths, DenyAll};
use davimci_present::{Host as PresentHost, Presenter};

struct Scratch(PathBuf);

impl Scratch {
    fn with_config(tag: &str, files: &[(&str, &str)]) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("davimci-plugin-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn timeline() -> Timeline {
    fixture(&[
        ("V1", &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")]),
        ("A1", &[(0, 300, "music")]),
    ])
}

/// An editor with this config tree loaded, assembled the way `main` does it.
fn editor_with(config: &Scratch) -> (App, Editor, Vec<davimci_app::Message>) {
    let mut plugins = Plugins::load(
        Some(&ConfigPaths::new(config.path())),
        config.path(),
        &DenyAll,
    );
    let notices = plugins.take_notices();
    let keymap = plugins.keymap();

    let session = Session::new(timeline());
    let mut ws = Workspace::new(config.path().to_path_buf()).without_autosave();
    ws.set_current_session(session.clone());
    let presenter = Presenter::new(
        PresentHost::Embedded,
        Resolution {
            width: 32,
            height: 16,
        },
        Fps::FPS_60,
    );
    let mut editor = Editor::new(
        ws,
        Box::new(MockBackend::new(Resolution {
            width: 8,
            height: 4,
        })),
        presenter,
    )
    .with_plugins(plugins);
    let mut app = App::with_keymap(session, keymap);
    for notice in notices.clone() {
        app.notify(notice);
    }
    editor.prime(app.session());
    (app, editor, notices)
}

fn feed(app: &mut App, editor: &mut Editor, keys: &str) {
    for k in Key::parse_str(keys) {
        app.key(k, editor);
    }
}

fn clips(app: &App) -> usize {
    app.session().timeline().tracks()[0].clips().len()
}

#[test]
fn a_mapped_key_edits_and_one_undo_takes_it_back() {
    // The whole point of item 1: a binding the user wrote reaches the
    // grammar, and the edit it asks for is an ordinary undo-tree entry.
    let config = Scratch::with_config(
        "mapped",
        &[(
            "keymaps.lua",
            r#"require("davimci.keymap").map("normal", "gz", "editor.split_at_playhead")"#,
        )],
    );
    let (mut app, mut editor, notices) = editor_with(&config);
    assert!(notices.is_empty(), "{notices:?}");
    let before = clips(&app);

    feed(&mut app, &mut editor, "<Right>gz");
    assert_eq!(clips(&app), before + 1, "the mapped key did not split");

    feed(&mut app, &mut editor, "u");
    assert_eq!(clips(&app), before, "one undo did not take the split back");
}

#[test]
fn a_lua_callback_edits_through_the_command_layer() {
    // A function right-hand side queues a request; the editor runs it through
    // the same `Session::exec` a keystroke would.
    let config = Scratch::with_config(
        "callback",
        &[(
            "keymaps.lua",
            r#"
            local map = require("davimci.keymap").map
            local editor = require("davimci.editor")
            map("normal", "gz", function()
              editor.split_at_playhead()
              editor.message("split by a plugin")
            end, { interrupt = true })
            "#,
        )],
    );
    let (mut app, mut editor, _) = editor_with(&config);
    let before = clips(&app);

    feed(&mut app, &mut editor, "<Right>gz");
    assert_eq!(clips(&app), before + 1, "the callback's edit did not land");
    assert!(
        app.messages()
            .history()
            .any(|m| m.text.contains("split by a plugin")),
        "the callback's message never reached the status line"
    );

    feed(&mut app, &mut editor, "u");
    assert_eq!(clips(&app), before, "the plugin edit was not one undo step");
}

#[test]
fn a_throwing_callback_disables_itself_and_leaves_the_session_editable() {
    let config = Scratch::with_config(
        "throwing",
        &[(
            "keymaps.lua",
            r#"
            local map = require("davimci.keymap").map
            map("normal", "gz", function() error("no") end)
            "#,
        )],
    );
    let (mut app, mut editor, _) = editor_with(&config);
    let before = clips(&app);

    feed(&mut app, &mut editor, "gz");
    let complaints = app
        .messages()
        .history()
        .filter(|m| m.severity != davimci_app::Severity::Info)
        .count();
    assert!(complaints >= 1, "the failure was never reported");

    // Pressing it again is silent: the handler is dead for the session.
    feed(&mut app, &mut editor, "gz");
    let after = app
        .messages()
        .history()
        .filter(|m| m.severity != davimci_app::Severity::Info)
        .count();
    assert_eq!(after, complaints, "a disabled callback complained twice");

    // And the editor still edits.
    feed(&mut app, &mut editor, "<Right>s");
    assert_eq!(clips(&app), before + 1, "the session stopped editing");
}

#[test]
fn a_lua_export_preset_reaches_a_real_export() {
    let config = Scratch::with_config(
        "preset",
        &[(
            "init.lua",
            r#"require("davimci.export").preset("youtube_1080p", {
                 container = "mp4",
                 video_codec = "h264",
                 audio_codec = "aac",
                 resolution = "1920x1080",
               })"#,
        )],
    );
    let (mut app, mut editor, notices) = editor_with(&config);
    assert!(notices.is_empty(), "{notices:?}");
    assert!(
        editor
            .exporter()
            .list_presets()
            .iter()
            .any(|p| p.contains("youtube_1080p")),
        "the preset never reached the registry"
    );

    let out = config.path().join("cut.mp4");
    app.event(
        Event::Command(format!(":export {} youtube_1080p", out.display())),
        &mut editor,
    );
    let said = app.view().message.expect("a status line").text;
    assert!(said.contains("youtube_1080p"), "{said}");
    assert!(editor.exporter().is_running(), "{said}");
}

#[test]
fn a_before_export_handler_can_refuse_the_render() {
    let config = Scratch::with_config(
        "veto",
        &[(
            "init.lua",
            r#"require("davimci.autocmd").on("BeforeExport", function(ctx)
                 return false, "this cut is not ready"
               end)"#,
        )],
    );
    let (mut app, mut editor, _) = editor_with(&config);
    let out = config.path().join("cut.mkv");

    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    let said = app.view().message.expect("a status line").text;
    assert!(said.contains("this cut is not ready"), "{said}");
    assert!(!editor.exporter().is_running(), "the veto did not stop it");
}

#[test]
fn an_event_handler_edit_runs_on_the_next_tick_as_one_command() {
    // An edit must not happen inside the notification that an edit happened:
    // a handler's request is queued and run on the following tick, still
    // through the command layer.
    let config = Scratch::with_config(
        "event",
        &[(
            "init.lua",
            r#"
            local editor = require("davimci.editor")
            local fired = false
            require("davimci.autocmd").on("SplitPerformed", function(event)
              if not fired then
                fired = true
                editor.message("saw a split at " .. event.frame)
              end
            end)
            "#,
        )],
    );
    let (mut app, mut editor, _) = editor_with(&config);
    feed(&mut app, &mut editor, "<Right>s");
    app.event(Event::Tick, &mut editor);

    assert!(
        app.messages()
            .history()
            .any(|m| m.text.contains("saw a split at 1")),
        "the SplitPerformed handler never ran"
    );
}

#[test]
fn a_registered_motion_moves_the_playhead_and_never_writes() {
    // A motion is a pure query: it answers a frame, and the editor is what
    // moves. Analysis has not run, so a query over an audio track
    // reports "not yet" rather than a wrong frame; this one asks about the
    // video track, where there is nothing to wait for.
    let config = Scratch::with_config(
        "motion",
        &[(
            "init.lua",
            r#"
            local motions = require("davimci.motions")
            motions.register("to_ten", function(ctx, opts) return opts.frame end)
            require("davimci.keymap").map("normal", "gz", function()
              motions.run("to_ten", { frame = 10 })
            end)
            "#,
        )],
    );
    let (mut app, mut editor, _) = editor_with(&config);
    let before = clips(&app);

    feed(&mut app, &mut editor, "gz");
    assert_eq!(
        app.session().timeline().playhead().frame,
        davimci_core::Frame(10)
    );
    assert_eq!(clips(&app), before, "a motion edited the timeline");
    assert!(
        app.session().history().at_root(),
        "a motion reached the undo log"
    );
}

/// A registered object is typeable, and the verb acts on the range
/// the config returned - through the ordinary command layer, so it undoes.
#[test]
fn a_registered_text_object_is_typeable_and_its_range_is_what_gets_deleted() {
    let dir = Scratch::with_config(
        "textobject",
        &[(
            "init.lua",
            r#"
local textobj = require("davimci.textobject")
textobj.register("c", {
  inner = function(clip) return { start = clip.core_range.start + 10, ["end"] = clip.core_range["end"] - 10 } end,
  around = function(clip) return clip.range_with_transitions end,
})
"#,
        )],
    );
    let (mut app, mut editor, _) = editor_with(&dir);
    let v1_end = |app: &App| {
        app.session().timeline().tracks()[0]
            .clips()
            .last()
            .map_or(0, |c| c.end().get())
    };
    let before = v1_end(&app);
    feed(&mut app, &mut editor, "dic");
    // The config's inner form is 20 frames narrower than the clip, so that
    // is exactly what the ripple delete took.
    assert_eq!(
        v1_end(&app),
        before - 80,
        "the config's range was not what got deleted"
    );
    // And it is one ordinary command.
    app.session_mut().undo().unwrap();
    assert_eq!(v1_end(&app), before);
}

/// A transition type a config registered reaches the backend and
/// the projected graph names the service the config asked for.
#[test]
fn a_registered_transition_type_reaches_the_projected_graph() {
    let dir = Scratch::with_config(
        "transition",
        &[(
            "init.lua",
            r#"
require("davimci.transition").register("sparkle", {
  service = "frei0r.sparkle",
  density = "3",
})
"#,
        )],
    );
    let mut plugins = Plugins::load(
        Some(&davimci_lua::ConfigPaths::new(dir.path())),
        dir.path(),
        &DenyAll,
    );
    let _ = plugins.take_notices();
    let defs = plugins.transitions();
    assert_eq!(defs.len(), 1, "the config's type did not reach the seam");
    assert_eq!(defs[0].service, "frei0r.sparkle");

    // Installed the way the editor installs it, through the backend trait.
    let mut backend = MockBackend::new(Resolution {
        width: 8,
        height: 4,
    });
    // A backend with no registry refuses rather than pretending.
    assert!(
        davimci_backend::RenderBackend::register_transition(&mut backend, defs[0].clone()).is_err()
    );
    davimci_mlt::transitions::register(&defs[0].name, &defs[0].service, defs[0].props.clone());

    // Plant one and project: the XML names the config's service.
    let tl = fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])]);
    let clip = tl.tracks()[0].clips()[1].id;
    let mut session = Session::new(tl);
    session
        .exec(&davimci_cmd::EditCommand::SetTransition {
            track: session.timeline().tracks()[0].id,
            clip,
            transition: Some(davimci_core::Transition::new(
                "sparkle",
                davimci_core::Frame(10),
            )),
        })
        .unwrap();
    let xml = davimci_mlt::to_xml(&davimci_mlt::Projection::of(session.timeline()));
    assert!(
        xml.contains("frei0r.sparkle"),
        "the registered service is not in the graph:\n{xml}"
    );

    // And a project using a type this build has never heard of still opens,
    // degrading to a dissolve rather than failing the render.
    session
        .exec(&davimci_cmd::EditCommand::SetTransition {
            track: session.timeline().tracks()[0].id,
            clip,
            transition: Some(davimci_core::Transition::new(
                "no_such_type",
                davimci_core::Frame(10),
            )),
        })
        .unwrap();
    let xml = davimci_mlt::to_xml(&davimci_mlt::Projection::of(session.timeline()));
    assert!(
        xml.contains("luma"),
        "the unknown type did not degrade:\n{xml}"
    );
}
