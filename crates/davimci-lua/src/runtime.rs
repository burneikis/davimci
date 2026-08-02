//! The Lua runtime: one interpreter, one registry, and the error isolation
//! that keeps a bad plugin from taking the editor with it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

use davimci_core::{Classify, ErrorClass, Notice};
use davimci_keys::{Action, Key, LeafAction, Mode};
use mlua::{Function, Lua, Table, Value, Variadic};

use crate::api;
use crate::config::TimelineConfig;
use crate::error::{LuaError, tidy};
use crate::event::{Dispatch, Event, HandlerFailure};
use crate::motion::{MotionAnswer, MotionEnv};
use crate::preset::ExportPreset;
use crate::registry::{HandlerId, KeyBinding, Rhs, State};
use crate::request::{Opts, Request, parse_editor_command};

/// How much of Lua a chunk may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    /// The user's own `~/.config/davimci` tree: full standard library.
    Trusted,
    /// Project-local `.davimci.lua`: no `os`, no `io`, no `load`/`dofile`, and
    /// a `require` that resolves nothing but the `davimci.*` modules.
    Restricted,
}

/// A clip handed to a user text object (spec §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipInfo {
    pub start: u64,
    pub end: u64,
    /// The clip plus its adjoining transitions. Equal to the core range
    /// until Phase 9f adds transitions.
    pub with_transitions_start: u64,
    pub with_transitions_end: u64,
}

/// Which form of a text object to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectForm {
    Inner,
    Around,
}

/// The Lua interpreter plus everything user config registered into it.
pub struct Runtime {
    lua: Lua,
    state: Rc<RefCell<State>>,
    /// Callbacks that threw once and are therefore dead for the session.
    disabled: RefCell<BTreeSet<HandlerId>>,
    notices: RefCell<Vec<Notice>>,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("state", &self.state.borrow())
            .field("disabled", &self.disabled.borrow())
            .field("notices", &self.notices.borrow())
            .finish()
    }
}

impl Runtime {
    /// A runtime with the `davimci.*` modules installed and nothing loaded.
    pub fn new() -> Result<Self, LuaError> {
        let lua = Lua::new();
        let state = Rc::new(RefCell::new(State::default()));
        api::install(&lua, &state).map_err(|e| LuaError::Runtime(tidy(&e)))?;
        Ok(Self {
            lua,
            state,
            disabled: RefCell::new(BTreeSet::new()),
            notices: RefCell::new(Vec::new()),
        })
    }

    /// Run a chunk. `name` is what appears in error messages.
    pub fn exec(&self, source: &str, name: &str, sandbox: Sandbox) -> Result<(), LuaError> {
        let chunk = self.lua.load(source).set_name(name);
        let result = match sandbox {
            Sandbox::Trusted => chunk.exec(),
            Sandbox::Restricted => {
                let env = self
                    .restricted_env()
                    .map_err(|e| LuaError::Runtime(tidy(&e)))?;
                chunk.set_environment(env).exec()
            }
        };
        result.map_err(|e| LuaError::Load {
            path: name.to_string(),
            reason: tidy(&e),
        })
    }

    /// Read and run a file.
    pub fn exec_file(&self, path: &Path, sandbox: Sandbox) -> Result<(), LuaError> {
        let source = std::fs::read_to_string(path).map_err(|e| LuaError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        self.exec(&source, &path.display().to_string(), sandbox)
    }

    /// The whitelist a project-local config sees. Nothing here can touch the
    /// filesystem, spawn a process, or compile new chunks.
    fn restricted_env(&self) -> mlua::Result<Table> {
        let g = self.lua.globals();
        let env = self.lua.create_table()?;
        for name in [
            "assert", "error", "ipairs", "next", "pairs", "pcall", "print", "select", "tonumber",
            "tostring", "type", "unpack", "xpcall", "math", "string", "table", "davimci",
        ] {
            let v: Value = g.get(name)?;
            if !v.is_nil() {
                env.set(name, v)?;
            }
        }
        // A `require` that resolves the davimci modules and nothing else, so
        // the documented `require("davimci.keymap")` form still works.
        let loaded: Table = g.get::<Table>("package")?.get("loaded")?;
        let require = self.lua.create_function(move |_, name: String| {
            if !name.starts_with("davimci") {
                return Err(mlua::Error::runtime(format!(
                    "a project-local config may not require '{name}'"
                )));
            }
            loaded.get::<Value>(name)
        })?;
        env.set("require", require)?;
        env.set("_G", env.clone())?;
        Ok(env)
    }

    /// Requests queued by Lua since the last drain.
    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut self.state.borrow_mut().requests)
    }

    /// Status-line notices produced by error isolation since the last drain.
    pub fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut self.notices.borrow_mut())
    }

    pub(crate) fn push_notice(&self, err: &LuaError) {
        self.notices.borrow_mut().push(Notice::from_error(err));
    }

    #[must_use]
    pub fn keymaps(&self) -> Vec<KeyBinding> {
        self.state.borrow().keymaps.clone()
    }

    /// User bindings in the shape `davimci_keys::Keymap` layers over the
    /// defaults. A Lua function becomes [`Action::Plugin`], which the host
    /// hands back to [`Runtime::invoke`].
    ///
    /// Only `NORMAL` bindings are returned today: `davimci_keys::Keymap` is one
    /// table for every mode, so a mode-specific override cannot be expressed
    /// without changing that table's shape.
    #[must_use]
    pub fn keymap_overrides(&self) -> Vec<(Vec<Key>, LeafAction)> {
        self.state
            .borrow()
            .keymaps
            .iter()
            .filter(|b| b.mode == Mode::Normal)
            .filter_map(|b| {
                let action = match &b.rhs {
                    Rhs::Command(c) => parse_editor_command(c)?,
                    Rhs::Callback(id) => Action::Plugin(*id),
                };
                Some((b.keys.clone(), LeafAction::Standalone(action)))
            })
            .collect()
    }

    #[must_use]
    pub fn timeline_config(&self) -> TimelineConfig {
        self.state.borrow().timeline.clone()
    }

    #[must_use]
    pub fn preset_names(&self) -> Vec<String> {
        self.state.borrow().presets.keys().cloned().collect()
    }

    pub fn preset(&self, name: &str) -> Result<ExportPreset, LuaError> {
        self.state
            .borrow()
            .presets
            .get(name)
            .cloned()
            .ok_or_else(|| LuaError::NoSuchPreset(name.to_string()))
    }

    #[must_use]
    pub fn motion_names(&self) -> Vec<String> {
        self.state.borrow().motions.keys().cloned().collect()
    }

    #[must_use]
    pub fn object_names(&self) -> Vec<String> {
        self.state.borrow().objects.keys().cloned().collect()
    }

    /// Whether a callback has been disabled by throwing.
    #[must_use]
    pub fn is_disabled(&self, id: HandlerId) -> bool {
        self.disabled.borrow().contains(&id)
    }

    /// Run a keymap callback ([`Action::Plugin`]) and return whatever edits
    /// it asked for. A throwing callback is disabled for the session and
    /// reported; the timeline is untouched either way, because Lua only ever
    /// queues requests.
    pub fn invoke(&self, id: HandlerId) -> Result<Vec<Request>, LuaError> {
        if self.is_disabled(id) {
            return Ok(Vec::new());
        }
        let func = self.state.borrow().callbacks.get(&id).cloned();
        let Some(func) = func else {
            return Err(LuaError::Config(format!("no callback {id} is registered")));
        };
        match func.call::<()>(()) {
            Ok(()) => Ok(self.take_requests()),
            Err(e) => {
                // Discard anything the callback queued before it threw: a
                // half-run handler must not half-edit the timeline.
                let _ = self.take_requests();
                self.disabled.borrow_mut().insert(id);
                let err = LuaError::callback(format!("keymap callback {id}"), &e);
                self.push_notice(&err);
                Err(err)
            }
        }
    }

    /// Fire an event at every enabled handler bound to it (spec §9.8).
    ///
    /// A handler refuses a cancellable event either by returning `false`
    /// (optionally with a message) or by raising an error. Raising also
    /// disables it, per the Phase 0 recoverable policy; returning `false` is
    /// a deliberate veto and leaves the handler in place.
    pub fn dispatch(&self, event: &Event) -> Dispatch {
        let mut out = Dispatch::default();
        let payload = match event.to_table(&self.lua) {
            Ok(t) => t,
            Err(e) => {
                let err = LuaError::callback(event.name(), &e);
                self.push_notice(&err);
                out.failures.push(HandlerFailure {
                    id: 0,
                    event: event.name().to_string(),
                    message: err.user_message(),
                });
                return out;
            }
        };
        let handlers: Vec<(HandlerId, Function)> = self
            .state
            .borrow()
            .autocmds
            .iter()
            .filter(|a| a.enabled && a.event == event.name())
            .map(|a| (a.id, a.func.clone()))
            .collect();

        for (id, func) in handlers {
            match func.call::<Variadic<Value>>(payload.clone()) {
                Ok(values) => {
                    out.requests.append(&mut self.take_requests());
                    if event.is_cancellable()
                        && let Some(reason) = veto(&values)
                    {
                        out.cancelled = Some(reason);
                        return out;
                    }
                }
                Err(e) => {
                    let _ = self.take_requests();
                    self.disable_autocmd(id);
                    let err = LuaError::callback(event.name(), &e);
                    self.push_notice(&err);
                    out.failures.push(HandlerFailure {
                        id,
                        event: event.name().to_string(),
                        message: err.user_message(),
                    });
                    if event.is_cancellable() {
                        out.cancelled = Some(err.user_message());
                        return out;
                    }
                }
            }
        }
        out
    }

    fn disable_autocmd(&self, id: HandlerId) {
        if let Some(a) = self
            .state
            .borrow_mut()
            .autocmds
            .iter_mut()
            .find(|a| a.id == id)
        {
            a.enabled = false;
        }
        self.disabled.borrow_mut().insert(id);
    }

    /// Run a registered motion (spec §9.3) against a snapshot.
    ///
    /// The snapshot is the whole contract: a motion sees frames and samples,
    /// never a live timeline, so it cannot mutate anything and needs no
    /// backend to test.
    pub fn run_motion(
        &self,
        name: &str,
        opts: &Opts,
        env: &MotionEnv,
    ) -> Result<MotionAnswer, LuaError> {
        let func = self
            .state
            .borrow()
            .motions
            .get(name)
            .cloned()
            .ok_or_else(|| LuaError::NoSuchMotion(name.to_string()))?;
        let pending = Rc::new(Cell::new(false));
        let ctx = self
            .motion_ctx(env, &pending)
            .map_err(|e| LuaError::callback(name, &e))?;
        let lua_opts = self
            .opts_table(opts)
            .map_err(|e| LuaError::callback(name, &e))?;
        match func.call::<Value>((ctx, lua_opts)) {
            Ok(v) => {
                if pending.get() {
                    return Ok(MotionAnswer::Pending);
                }
                Ok(match v {
                    Value::Integer(i) if i >= 0 => MotionAnswer::Found(i as u64),
                    Value::Number(n) if n >= 0.0 => MotionAnswer::Found(n as u64),
                    Value::Table(t) => match t.get::<Option<u64>>("frame") {
                        Ok(Some(f)) => MotionAnswer::Found(f),
                        _ => MotionAnswer::NoMatch,
                    },
                    _ => MotionAnswer::NoMatch,
                })
            }
            Err(e) => {
                let err = LuaError::callback(name, &e);
                self.push_notice(&err);
                Err(err)
            }
        }
    }

    /// Resolve a user text object (spec §9.4) to a frame range.
    pub fn run_object(
        &self,
        name: &str,
        form: ObjectForm,
        clip: ClipInfo,
    ) -> Result<Option<(u64, u64)>, LuaError> {
        let func = {
            let st = self.state.borrow();
            let def = st
                .objects
                .get(name)
                .ok_or_else(|| LuaError::NoSuchObject(name.to_string()))?;
            match form {
                ObjectForm::Inner => def.inner.clone(),
                ObjectForm::Around => def.around.clone().or_else(|| def.inner.clone()),
            }
        };
        let Some(func) = func else {
            return Ok(None);
        };
        let table = self
            .clip_table(clip)
            .map_err(|e| LuaError::callback(name, &e))?;
        match func.call::<Value>(table) {
            Ok(Value::Table(t)) => {
                let start: Option<u64> = t.get("start").ok().flatten();
                let end: Option<u64> = t.get("end").ok().flatten();
                match (start, end) {
                    (Some(s), Some(e)) if e >= s => Ok(Some((s, e))),
                    _ => Err(LuaError::Config(format!(
                        "text object '{name}' returned a range without a valid start and end"
                    ))),
                }
            }
            Ok(_) => Ok(None),
            Err(e) => {
                let err = LuaError::callback(name, &e);
                self.push_notice(&err);
                Err(err)
            }
        }
    }

    fn clip_table(&self, clip: ClipInfo) -> mlua::Result<Table> {
        let range = |s: u64, e: u64| -> mlua::Result<Table> {
            let t = self.lua.create_table()?;
            t.set("start", s)?;
            t.set("end", e)?;
            Ok(t)
        };
        let t = self.lua.create_table()?;
        t.set("start", clip.start)?;
        t.set("end", clip.end)?;
        t.set("core_range", range(clip.start, clip.end)?)?;
        t.set(
            "range_with_transitions",
            range(clip.with_transitions_start, clip.with_transitions_end)?,
        )?;
        Ok(t)
    }

    fn opts_table(&self, opts: &Opts) -> mlua::Result<Table> {
        use crate::request::OptValue;
        let t = self.lua.create_table()?;
        for (k, v) in opts.iter() {
            match v {
                OptValue::Str(s) => t.set(k.as_str(), s.as_str())?,
                OptValue::Num(n) => t.set(k.as_str(), *n)?,
                OptValue::Bool(b) => t.set(k.as_str(), *b)?,
            }
        }
        Ok(t)
    }

    /// The `ctx` a motion receives: the playhead, the focused track, and a
    /// `timeline` with `find_next`.
    fn motion_ctx(&self, env: &MotionEnv, pending: &Rc<Cell<bool>>) -> mlua::Result<Table> {
        let ctx = self.lua.create_table()?;
        ctx.set("playhead", env.playhead)?;
        ctx.set("track", env.focused_track.as_str())?;

        let env = env.clone();
        let pending = Rc::clone(pending);
        let lua = self.lua.clone();
        let find_next = self.lua.create_function(move |_, args: Variadic<Value>| {
            // Accept both `timeline:find_next(q)` and
            // `timeline.find_next(q)`: a config should not fail over a
            // colon.
            let query = args
                .iter()
                .rev()
                .find_map(|v| match v {
                    Value::Table(t) => Some(t.clone()),
                    _ => None,
                })
                .ok_or_else(|| mlua::Error::runtime("find_next needs a query table"))?;
            find_next(&lua, &env, &pending, &query)
        })?;
        let timeline = self.lua.create_table()?;
        timeline.set("find_next", find_next)?;
        ctx.set("timeline", timeline)?;
        Ok(ctx)
    }
}

/// `find_next` over a [`MotionEnv`] snapshot: the first sample past `from`
/// for which the user predicate holds.
fn find_next(
    lua: &Lua,
    env: &MotionEnv,
    pending: &Rc<Cell<bool>>,
    query: &Table,
) -> mlua::Result<Value> {
    let track_name: String = query
        .get::<Option<String>>("track")?
        .unwrap_or_else(|| env.focused_track.clone());
    let Some(track) = env.tracks.get(&track_name) else {
        return Err(mlua::Error::runtime(format!(
            "track {track_name} does not exist"
        )));
    };
    if let Some(kind) = query.get::<Option<String>>("type")?
        && kind != track.kind
    {
        return Err(mlua::Error::runtime(format!(
            "track {track_name} is a {} track, not {kind}",
            track.kind
        )));
    }
    if !track.analysed {
        // Never answer from half-finished analysis: the caller would move
        // the playhead to a wrong frame and never learn it was wrong.
        pending.set(true);
        return Ok(Value::Nil);
    }
    let from = query.get::<Option<u64>>("from")?.unwrap_or(env.playhead);
    let backward = matches!(
        query.get::<Option<String>>("direction")?.as_deref(),
        Some("backward")
    );
    let predicate: Option<Function> = query.get("predicate")?;

    let mut candidates: Vec<&crate::motion::Sample> = track
        .samples
        .iter()
        .filter(|s| {
            if backward {
                s.frame < from
            } else {
                s.frame > from
            }
        })
        .collect();
    if backward {
        candidates.reverse();
    }
    for s in candidates {
        let matched = match &predicate {
            None => true,
            Some(p) => {
                let t = lua.create_table()?;
                t.set("frame", s.frame)?;
                t.set("rms_db", s.rms_db)?;
                t.set("peak_db", s.peak_db)?;
                p.call::<bool>(t)?
            }
        };
        if matched {
            return Ok(Value::Integer(s.frame as i64));
        }
    }
    Ok(Value::Nil)
}

/// Did a handler veto a cancellable event? `false` or `nil, "reason"` do;
/// anything else does not.
fn veto(values: &[Value]) -> Option<String> {
    let first = values.first()?;
    if !matches!(first, Value::Boolean(false)) {
        return None;
    }
    let reason = values.get(1).and_then(|v| match v {
        Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
        _ => None,
    });
    Some(reason.unwrap_or_else(|| "a BeforeExport handler refused the export".to_string()))
}

/// Read-only access to Lua globals, so a test can assert on what a handler
/// actually did rather than on the fact that it was called.
#[cfg(test)]
impl Runtime {
    pub(crate) fn exec_eval_number(&self, global: &str) -> f64 {
        self.lua
            .load(format!("return {global}"))
            .eval::<f64>()
            .unwrap_or(f64::NAN)
    }

    pub(crate) fn exec_eval_numbers(&self, global: &str) -> Option<Vec<u64>> {
        self.lua
            .load(format!("return {global}"))
            .eval::<Vec<u64>>()
            .ok()
    }

    pub(crate) fn exec_eval_count(&self, global: &str) -> usize {
        self.lua
            .load(format!("return #{global}"))
            .eval::<usize>()
            .unwrap_or(0)
    }
}

impl Runtime {
    /// The class a caller should treat the last notices as. Convenience for
    /// frontends: the Lua layer never produces a fatal error.
    #[must_use]
    pub fn worst_class(notices: &[Notice]) -> ErrorClass {
        notices
            .iter()
            .map(|n| n.class)
            .max_by_key(|c| match c {
                ErrorClass::User => 0,
                ErrorClass::OfflineMedia => 1,
                ErrorClass::Recoverable => 2,
                ErrorClass::Corruption => 3,
            })
            .unwrap_or(ErrorClass::User)
    }
}
