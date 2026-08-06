//! The `davimci.*` module surface.
//!
//! Every module is a plain table published into `package.loaded`, so the
//! documented `require("davimci.keymap")` form works without a `package.path`
//! that would let a config `require` arbitrary files off disk.
//!
//! No module here can edit a timeline. Calls that mean "do something" append
//! a [`Request`]; the host runs them through the command layer. That is the
//! single write path rule holding at the plugin boundary.

use std::cell::RefCell;
use std::rc::Rc;

use davimci_keys::Key;
use mlua::{Function, Lua, Table, Value, Variadic};

use crate::error::LuaError;
use crate::preset::{ExportPreset, SubtitleSelection, TrackSelection, parse_resolution};
use crate::registry::{Autocmd, KeyBinding, ObjectDef, Rhs, State, parse_mode};
use crate::request::{OptValue, Opts, Request, parse_editor_command};

/// The v1 event list. A typo in an event name binds a handler
/// that would never fire, so it is rejected at registration.
pub const EVENTS: &[&str] = &[
    "PlayheadMoved",
    "SplitPerformed",
    "ClipDeleted",
    "ClipInserted",
    "ModeChanged",
    "BeforeExport",
    "AfterExport",
    "ProjectLoaded",
];

type Shared = Rc<RefCell<State>>;

fn err(e: LuaError) -> mlua::Error {
    e.into()
}

pub(crate) fn opts_from_table(t: &Table) -> mlua::Result<Opts> {
    let mut out = Opts::new();
    for pair in t.pairs::<String, Value>() {
        let (k, v) = pair?;
        let value = match v {
            Value::String(s) => OptValue::Str(s.to_str()?.to_string()),
            // A Lua option is a number either way; integers beyond the f64
            // mantissa are not values an option carries.
            #[allow(
                clippy::cast_precision_loss,
                reason = "a Lua number is an f64, so this is the type the value already had"
            )]
            Value::Integer(i) => OptValue::Num(i as f64),
            Value::Number(n) => OptValue::Num(n),
            Value::Boolean(b) => OptValue::Bool(b),
            other => {
                return Err(err(LuaError::Config(format!(
                    "option '{k}' must be a string, number, or boolean, not {}",
                    other.type_name()
                ))));
            }
        };
        out.insert(k, value);
    }
    Ok(out)
}

fn string_list(t: &Table) -> mlua::Result<Vec<String>> {
    let mut out = Vec::new();
    for v in t.clone().sequence_values::<String>() {
        out.push(v?);
    }
    Ok(out)
}

/// Install every module into `lua`, backed by `state`.
pub(crate) fn install(lua: &Lua, state: &Shared) -> mlua::Result<()> {
    let davimci = lua.create_table()?;
    davimci.set("version", env!("CARGO_PKG_VERSION"))?;

    let modules: [(&str, Table); 8] = [
        ("keymap", keymap_module(lua, state)?),
        ("motions", motions_module(lua, state)?),
        ("textobject", textobject_module(lua, state)?),
        ("export", export_module(lua, state)?),
        ("timeline", timeline_module(lua, state)?),
        ("media", media_module(lua, state)?),
        ("autocmd", autocmd_module(lua, state)?),
        ("transition", transition_module(lua, state)?),
    ];
    let editor = editor_module(lua, state)?;

    let loaded: Table = lua.globals().get::<Table>("package")?.get("loaded")?;
    for (name, table) in modules {
        loaded.set(format!("davimci.{name}"), table.clone())?;
        davimci.set(name, table)?;
    }
    loaded.set("davimci.editor", editor.clone())?;
    davimci.set("editor", editor)?;
    loaded.set("davimci", davimci.clone())?;
    lua.globals().set("davimci", davimci)?;
    Ok(())
}

fn keymap_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    let map = lua.create_function(
        move |_, (mode, lhs, rhs, opts): (String, String, Value, Option<Table>)| {
        let Some(mode) = parse_mode(&mode) else {
            return Err(err(LuaError::Config(format!(
                "'{mode}' is not a mode (known: normal, visual, visual-line, visual-block, insert, command)"
            ))));
        };
        let keys = Key::parse_str(&lhs);
        if keys.is_empty() {
            return Err(err(LuaError::Config(
                "a keymap needs a left-hand side to bind".into(),
            )));
        }
        let mut st = st.borrow_mut();
        let rhs = match rhs {
            Value::String(s) => {
                let cmd = s.to_str()?.to_string();
                if parse_editor_command(&cmd).is_none() {
                    return Err(err(LuaError::Config(format!(
                        "keymap '{lhs}' names '{cmd}', which is not an editor command"
                    ))));
                }
                Rhs::Command(cmd)
            }
            Value::Function(f) => {
                let id = st.next_id();
                st.callbacks.insert(id, f);
                Rhs::Callback(id)
            }
            other => {
                return Err(err(LuaError::Config(format!(
                    "keymap '{lhs}' must map to a command string or a function, not {}",
                    other.type_name()
                ))));
            }
        };
        let interrupt = match &opts {
            Some(t) => t.get::<Option<bool>>("interrupt")?.unwrap_or(false),
            None => false,
        };
        st.keymaps.retain(|b| !(b.mode == mode && b.keys == keys));
        st.keymaps.push(KeyBinding {
            mode,
            keys,
            rhs,
            interrupt,
        });
        Ok(())
        },
    )?;
    t.set("map", map)?;
    Ok(t)
}

fn motions_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    t.set(
        "register",
        lua.create_function(move |_, (name, f): (String, Function)| {
            st.borrow_mut().motions.insert(name, f);
            Ok(())
        })?,
    )?;
    let st = Rc::clone(state);
    t.set(
        "run",
        lua.create_function(move |_, (name, opts): (String, Option<Table>)| {
            let opts = match &opts {
                Some(t) => opts_from_table(t)?,
                None => Opts::new(),
            };
            let mut st = st.borrow_mut();
            if !st.motions.contains_key(&name) {
                return Err(err(LuaError::NoSuchMotion(name)));
            }
            st.requests.push(Request::Motion { name, opts });
            Ok(())
        })?,
    )?;
    Ok(t)
}

fn textobject_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    t.set(
        "register",
        lua.create_function(move |_, (name, def): (String, Table)| {
            let inner: Option<Function> = def.get("inner")?;
            let around: Option<Function> = def.get("around")?;
            if inner.is_none() && around.is_none() {
                return Err(err(LuaError::Config(format!(
                    "text object '{name}' defines neither an inner nor an around form"
                ))));
            }
            st.borrow_mut().objects.insert(
                name.clone(),
                ObjectDef {
                    name,
                    inner,
                    around,
                },
            );
            Ok(())
        })?,
    )?;
    Ok(t)
}

/// `davimci.transition.register(name, { service = ..., <prop> = ... })`
///.
fn transition_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    t.set(
        "register",
        lua.create_function(move |_, (name, def): (String, Table)| {
            let service: Option<String> = def.get("service")?;
            let Some(service) = service.filter(|s| !s.trim().is_empty()) else {
                return Err(err(LuaError::Config(format!(
                    "transition '{name}' does not say which service renders it"
                ))));
            };
            // Everything else is a backend property, passed through as
            // written: this crate does not know what any of them mean.
            let mut props = Vec::new();
            for pair in def.clone().pairs::<String, mlua::Value>() {
                let (key, value) = pair?;
                if key == "service" {
                    continue;
                }
                props.push((key, value.to_string()?));
            }
            props.sort();
            st.borrow_mut().transitions.insert(
                name.clone(),
                crate::registry::TransitionDef {
                    name,
                    service,
                    props,
                },
            );
            Ok(())
        })?,
    )?;
    Ok(t)
}

fn export_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    t.set(
        "preset",
        lua.create_function(move |_, (name, def): (String, Table)| {
            let preset = preset_from_table(&name, &def)?;
            preset.validate().map_err(err)?;
            st.borrow_mut().presets.insert(name, preset);
            Ok(())
        })?,
    )?;
    let st = Rc::clone(state);
    t.set(
        "run",
        lua.create_function(move |_, name: String| {
            let mut st = st.borrow_mut();
            if !st.presets.contains_key(&name) {
                return Err(err(LuaError::NoSuchPreset(name)));
            }
            st.requests.push(Request::Export { preset: name });
            Ok(())
        })?,
    )?;
    Ok(t)
}

fn preset_from_table(name: &str, def: &Table) -> mlua::Result<ExportPreset> {
    let container: String = def
        .get::<Option<String>>("container")?
        .unwrap_or_else(|| "mkv".into());
    let video_codec: String = def
        .get::<Option<String>>("video_codec")?
        .unwrap_or_else(|| "h264".into());
    let audio_codec: String = def
        .get::<Option<String>>("audio_codec")?
        .unwrap_or_else(|| "aac".into());
    let resolution = match def.get::<Option<String>>("resolution")? {
        None => None,
        Some(s) => Some(parse_resolution(&s).ok_or_else(|| {
            err(LuaError::Config(format!(
                "export preset '{name}' has resolution '{s}', which is not WIDTHxHEIGHT"
            )))
        })?),
    };
    let audio_tracks = match def.get::<Value>("audio_tracks")? {
        Value::Nil => TrackSelection::All,
        Value::String(s) => match s.to_str()?.as_ref() {
            "all" => TrackSelection::All,
            "none" => TrackSelection::None,
            other => {
                return Err(err(LuaError::Config(format!(
                    "export preset '{name}' has audio_tracks '{other}' (expected 'all', 'none', or a list)"
                ))));
            }
        },
        Value::Table(t) => TrackSelection::Named(string_list(&t)?),
        other => {
            return Err(err(LuaError::Config(format!(
                "export preset '{name}' has audio_tracks of type {}",
                other.type_name()
            ))));
        }
    };
    let subtitle_tracks = match def.get::<Value>("subtitle_tracks")? {
        Value::Nil => SubtitleSelection::None,
        Value::String(s) => match s.to_str()?.as_ref() {
            "burned" => SubtitleSelection::Burned,
            "sidecar" => SubtitleSelection::Sidecar,
            "embedded" => SubtitleSelection::Embedded,
            "none" => SubtitleSelection::None,
            other => {
                return Err(err(LuaError::Config(format!(
                    "export preset '{name}' has subtitle_tracks '{other}' (expected 'burned', 'sidecar', 'embedded', 'none', or a list)"
                ))));
            }
        },
        Value::Table(t) => SubtitleSelection::Named(string_list(&t)?),
        other => {
            return Err(err(LuaError::Config(format!(
                "export preset '{name}' has subtitle_tracks of type {}",
                other.type_name()
            ))));
        }
    };
    Ok(ExportPreset {
        name: name.to_string(),
        container,
        video_codec,
        audio_codec,
        resolution,
        fps: None,
        audio_tracks,
        subtitle_tracks,
    })
}

fn timeline_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    t.set(
        "configure",
        lua.create_function(move |_, cfg: Table| {
            // Validate into a copy first: a config that is half-applied when
            // it hits a typo is worse than one that is not applied at all.
            let mut next = st.borrow().timeline.clone();
            if let Some(list) = cfg.get::<Option<Table>>("jump_points")? {
                next.set_sources(&string_list(&list)?).map_err(err)?;
            }
            if let Some(map) = cfg.get::<Option<Table>>("jump_point_density_per_zoom")? {
                let mut entries: Vec<(u8, String)> = Vec::new();
                for pair in map.pairs::<i64, String>() {
                    let (level, kind) = pair?;
                    let level = u8::try_from(level).map_err(|_| {
                        err(LuaError::Config(format!(
                            "zoom level {level} is outside the 0-255 range"
                        )))
                    })?;
                    entries.push((level, kind));
                }
                next.set_density(&entries).map_err(err)?;
            }
            if let Some(keys) = cfg.get::<Option<Table>>("frame_step_keys")? {
                let parsed: Vec<Vec<Key>> = string_list(&keys)?
                    .iter()
                    .map(|s| Key::parse_str(s))
                    .collect();
                if parsed.iter().any(Vec::is_empty) {
                    return Err(err(LuaError::Config(
                        "frame_step_keys contains an empty key string".into(),
                    )));
                }
                next.frame_step_keys = parsed;
            }
            st.borrow_mut().timeline = next;
            Ok(())
        })?,
    )?;
    Ok(t)
}

fn media_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    t.set(
        "import",
        lua.create_function(move |_, path: String| {
            st.borrow_mut().requests.push(Request::Import { path });
            Ok(())
        })?,
    )?;
    let st = Rc::clone(state);
    t.set(
        "analyze",
        lua.create_function(move |_, track: Option<String>| {
            st.borrow_mut().requests.push(Request::Analyze { track });
            Ok(())
        })?,
    )?;
    Ok(t)
}

fn autocmd_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let st = Rc::clone(state);
    t.set(
        "on",
        lua.create_function(move |_, (event, f): (String, Function)| {
            if !EVENTS.contains(&event.as_str()) {
                return Err(err(LuaError::Config(format!(
                    "'{event}' is not a davimci event (known: {})",
                    EVENTS.join(", ")
                ))));
            }
            let mut st = st.borrow_mut();
            let id = st.next_id();
            st.autocmds.push(Autocmd {
                id,
                event,
                func: f,
                enabled: true,
            });
            Ok(id)
        })?,
    )?;
    let st = Rc::clone(state);
    t.set(
        "off",
        lua.create_function(move |_, id: u32| {
            let mut st = st.borrow_mut();
            let before = st.autocmds.len();
            st.autocmds.retain(|a| a.id != id);
            Ok(st.autocmds.len() != before)
        })?,
    )?;
    Ok(t)
}

/// Editor commands, one Lua function per name, all queueing rather than
/// editing. `step_frame` is the only one taking an argument.
fn editor_module(lua: &Lua, state: &Shared) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for name in [
        "split_at_playhead",
        "split_all_tracks",
        "ripple_delete",
        "undo",
        "redo",
        "repeat",
        "paste",
        "paste_before",
        "play_pause",
        "interrupt_transport",
    ] {
        let st = Rc::clone(state);
        let f = lua.create_function(move |_, _: Variadic<Value>| {
            let Some(action) = parse_editor_command(name) else {
                return Err(err(LuaError::Config(format!("editor.{name} is unbound"))));
            };
            st.borrow_mut().requests.push(Request::Edit(action));
            Ok(())
        })?;
        t.set(name, f)?;
    }
    for name in ["step_frame", "step_jump_point"] {
        let st = Rc::clone(state);
        let f = lua.create_function(move |_, n: i64| {
            let Some(action) = parse_editor_command(&format!("{name}({n})")) else {
                return Err(err(LuaError::Config(format!(
                    "editor.{name}({n}) is not a movement"
                ))));
            };
            st.borrow_mut().requests.push(Request::Edit(action));
            Ok(())
        })?;
        t.set(name, f)?;
    }
    let st = Rc::clone(state);
    t.set(
        "message",
        lua.create_function(move |_, text: String| {
            st.borrow_mut().requests.push(Request::Message(text));
            Ok(())
        })?,
    )?;
    let st = Rc::clone(state);
    t.set(
        "set",
        // Queued as a request like every other change, so a config-set
        // property goes through the same registry, validation and undo rules
        // as one typed at `:`. The name and value are not checked
        // here because this crate does not own the registry.
        lua.create_function(move |_, (property, value): (String, Value)| {
            let value = match value {
                Value::String(s) => s.to_str()?.to_string(),
                Value::Integer(n) => n.to_string(),
                Value::Number(n) => n.to_string(),
                Value::Boolean(b) => if b { "on" } else { "off" }.to_string(),
                other => {
                    return Err(err(LuaError::Config(format!(
                        "editor.set('{property}', ...) needs a string, number or boolean, not a {}",
                        other.type_name()
                    ))));
                }
            };
            st.borrow_mut()
                .requests
                .push(Request::Set { property, value });
            Ok(())
        })?,
    )?;
    Ok(t)
}
