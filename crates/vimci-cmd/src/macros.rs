//! Macro record/replay buffers for `q` and `@` (spec §3, §11).
//!
//! A macro is a list of opaque input tokens, not a list of commands: vim
//! replays keystrokes, so `@a` re-runs motions and counts against wherever
//! the playhead is now. `vimci-keys` decides what a token means; this crate
//! only stores them, which keeps Phase 2 free of any key grammar.

use std::collections::BTreeMap;

use crate::error::CmdError;

/// Recording state and the register buffers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacroRecorder {
    registers: BTreeMap<char, Vec<String>>,
    recording: Option<(char, Vec<String>)>,
}

impl MacroRecorder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The register currently being recorded into, if any.
    #[must_use]
    pub fn recording(&self) -> Option<char> {
        self.recording.as_ref().map(|(r, _)| *r)
    }

    /// `q<reg>`: start recording. An uppercase register appends to the
    /// lowercase one, as in vim.
    pub fn start(&mut self, register: char) -> Result<(), CmdError> {
        if let Some(active) = self.recording() {
            return Err(CmdError::AlreadyRecording(active));
        }
        let seed = if register.is_uppercase() {
            self.registers
                .get(&register.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.recording = Some((register, seed));
        Ok(())
    }

    /// Record one input token. Ignored when not recording, so the caller can
    /// feed every key unconditionally.
    pub fn push(&mut self, token: impl Into<String>) {
        if let Some((_, buf)) = self.recording.as_mut() {
            buf.push(token.into());
        }
    }

    /// Drop the token just recorded - used for the trailing `q` that stops
    /// recording, which must not end up in the macro.
    pub fn pop(&mut self) {
        if let Some((_, buf)) = self.recording.as_mut() {
            buf.pop();
        }
    }

    /// `q`: stop recording and store the buffer. Returns the register used.
    pub fn stop(&mut self) -> Result<char, CmdError> {
        let (register, buf) = self.recording.take().ok_or(CmdError::NotRecording)?;
        self.registers
            .insert(register.to_ascii_lowercase(), buf.clone());
        self.registers.insert(register, buf);
        Ok(register)
    }

    /// `@<reg>`: the tokens to replay.
    pub fn replay(&self, register: char) -> Result<&[String], CmdError> {
        self.registers
            .get(&register.to_ascii_lowercase())
            .map(Vec::as_slice)
            .filter(|t| !t.is_empty())
            .ok_or(CmdError::NoSuchMacro(register))
    }

    /// Set a register directly, as `:let @a = ...` would.
    pub fn set(&mut self, register: char, tokens: Vec<String>) {
        self.registers.insert(register.to_ascii_lowercase(), tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn record_then_replay() {
        let mut m = MacroRecorder::new();
        assert_eq!(m.recording(), None);
        assert!(m.start('a').is_ok());
        assert_eq!(m.recording(), Some('a'));
        m.push("x");
        m.push("w");
        m.push("q");
        m.pop();
        assert_eq!(m.stop(), Ok('a'));
        assert_eq!(m.replay('a'), Ok(tokens(&["x", "w"]).as_slice()));
    }

    #[test]
    fn an_uppercase_register_appends() {
        let mut m = MacroRecorder::new();
        m.set('a', tokens(&["x"]));
        assert!(m.start('A').is_ok());
        m.push("w");
        assert!(m.stop().is_ok());
        assert_eq!(m.replay('a'), Ok(tokens(&["x", "w"]).as_slice()));
    }

    #[test]
    fn recording_twice_is_a_user_error() {
        let mut m = MacroRecorder::new();
        assert!(m.start('a').is_ok());
        assert_eq!(m.start('b'), Err(CmdError::AlreadyRecording('a')));
    }

    #[test]
    fn stopping_without_recording_is_a_user_error() {
        let mut m = MacroRecorder::new();
        assert_eq!(m.stop(), Err(CmdError::NotRecording));
    }

    #[test]
    fn an_empty_or_unknown_register_cannot_be_replayed() {
        let mut m = MacroRecorder::new();
        assert_eq!(m.replay('z'), Err(CmdError::NoSuchMacro('z')));
        assert!(m.start('z').is_ok());
        assert!(m.stop().is_ok());
        assert_eq!(m.replay('z'), Err(CmdError::NoSuchMacro('z')));
    }
}
