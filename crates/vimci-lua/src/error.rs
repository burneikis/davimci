//! Errors from the Lua layer (plan.md Phase 0, Phase 7).
//!
//! Everything here is either a *user* error (a config file says something
//! impossible - an unknown export container, an unbindable command) or a
//! *recoverable* one (a user callback threw; the handler is disabled for the
//! session and editing continues). Nothing the Lua layer does may take the
//! editor down, so no variant classifies as `Corruption`.

use vimci_core::{Classify, ErrorClass};

/// An error raised while loading or running user Lua.
#[derive(Debug, thiserror::Error)]
pub enum LuaError {
    #[error("could not read {path}: {reason}")]
    Io { path: String, reason: String },

    #[error("{path} failed to load: {reason}")]
    Load { path: String, reason: String },

    #[error("{name} raised an error: {reason}")]
    Callback { name: String, reason: String },

    #[error("no motion named '{0}' is registered")]
    NoSuchMotion(String),

    #[error("no text object named '{0}' is registered")]
    NoSuchObject(String),

    #[error("no export preset named '{0}' is defined")]
    NoSuchPreset(String),

    #[error("{0}")]
    Config(String),

    #[error("the Lua runtime could not be created: {0}")]
    Runtime(String),
}

impl LuaError {
    pub(crate) fn callback(name: impl Into<String>, err: &mlua::Error) -> Self {
        Self::Callback {
            name: name.into(),
            reason: tidy(err),
        }
    }
}

/// mlua's `Display` is multi-line and full of chunk noise; the status line
/// gets the first line only, which is the message the user wrote.
pub(crate) fn tidy(err: &mlua::Error) -> String {
    let full = err.to_string();
    let first = full.lines().next().unwrap_or_default().trim();
    let first = first.strip_prefix("runtime error: ").unwrap_or(first);
    let first = first.strip_prefix("syntax error: ").unwrap_or(first);
    if first.is_empty() {
        "unknown Lua error".to_string()
    } else {
        first.to_string()
    }
}

impl Classify for LuaError {
    fn class(&self) -> ErrorClass {
        match self {
            // A config that says something impossible is the user's mistake,
            // reported and skipped.
            Self::Config(_)
            | Self::NoSuchMotion(_)
            | Self::NoSuchObject(_)
            | Self::NoSuchPreset(_) => ErrorClass::User,
            // Everything else degrades locally: the file or handler is
            // dropped for the session, the editor keeps running.
            Self::Io { .. } | Self::Load { .. } | Self::Callback { .. } | Self::Runtime(_) => {
                ErrorClass::Recoverable
            }
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}

impl From<LuaError> for mlua::Error {
    fn from(e: LuaError) -> Self {
        mlua::Error::runtime(e.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn no_lua_error_is_fatal() {
        for e in [
            LuaError::Config("bad".into()),
            LuaError::NoSuchMotion("x".into()),
            LuaError::Io {
                path: "/tmp/x".into(),
                reason: "missing".into(),
            },
            LuaError::Callback {
                name: "h".into(),
                reason: "boom".into(),
            },
        ] {
            assert!(e.class().is_continuable(), "{e} must not be fatal");
            assert!(!e.user_message().is_empty());
        }
    }

    #[test]
    fn a_lua_message_is_stripped_to_one_line() {
        let lua = mlua::Lua::new();
        let err = lua
            .load("error('no muted tracks allowed')")
            .exec()
            .expect_err("chunk must fail");
        let msg = tidy(&err);
        assert!(msg.contains("no muted tracks allowed"), "{msg}");
        assert!(!msg.contains('\n'));
    }
}
