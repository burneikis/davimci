//! The project format: last snapshot + the command log since it (spec §10.4).
//!
//! No I/O lives here - `vimci-cmd` stays a pure library (plan.md cross-cutting
//! rule 1). `vimci-cli` reads and writes the bytes; this module only turns a
//! session into JSON and back, and migrates older schema versions on the way
//! in.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use vimci_core::{Classify, ErrorClass, Timeline};

use crate::command::{Command, EditCommand};
use crate::error::CmdError;
use crate::session::Session;

/// Bumped whenever the on-disk schema changes. Every bump needs a migration
/// arm in [`migrate`] and a test that loads a document written by the
/// previous version.
pub const FORMAT_VERSION: u32 = 1;

/// A saved project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    /// The compacted state the log replays on top of.
    pub snapshot: Timeline,
    /// Commands applied after `snapshot`, in order.
    #[serde(default)]
    pub log: Vec<EditCommand>,
    /// The id cursor of the saved state. Replaying pinned commands mints no
    /// ids, so it is recorded rather than derived - reload is then exact.
    #[serde(default)]
    pub id_cursor: Option<u64>,
}

/// Things that can go wrong reading or writing a project.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    #[error("this project file is not valid vimci JSON: {0}")]
    Malformed(String),

    #[error("this project was written by a newer vimci (format version {0})")]
    TooNew(u32),

    #[error("this project's command log could not be replayed: {0}")]
    Unreplayable(String),
}

impl Classify for ProjectError {
    fn class(&self) -> ErrorClass {
        match self {
            // A file we cannot parse or replay is not something to keep
            // editing on top of (plan.md Phase 0, corruption policy).
            Self::Malformed(_) | Self::Unreplayable(_) => ErrorClass::Corruption,
            Self::TooNew(_) => ErrorClass::User,
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}

impl ProjectFile {
    /// Compact a session for saving.
    #[must_use]
    pub fn from_session(session: &Session) -> Self {
        let (snapshot, log) = session
            .history()
            .compacted()
            .unwrap_or_else(|| (session.timeline().clone(), Vec::new()));
        Self {
            version: FORMAT_VERSION,
            snapshot,
            log,
            id_cursor: Some(session.timeline().id_cursor()),
        }
    }

    pub fn to_json(&self) -> Result<String, ProjectError> {
        serde_json::to_string_pretty(self).map_err(|e| ProjectError::Malformed(e.to_string()))
    }

    /// Parse, migrating older schema versions first.
    pub fn from_json(text: &str) -> Result<Self, ProjectError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| ProjectError::Malformed(e.to_string()))?;
        let value = migrate(value)?;
        serde_json::from_value(value).map_err(|e| ProjectError::Malformed(e.to_string()))
    }

    /// Replay the log onto the snapshot to get the saved timeline.
    pub fn into_timeline(self) -> Result<Timeline, ProjectError> {
        let mut tl = self.snapshot;
        for cmd in &self.log {
            cmd.apply(&mut tl)
                .map_err(|e: CmdError| ProjectError::Unreplayable(e.to_string()))?;
        }
        if let Some(cursor) = self.id_cursor {
            tl.set_id_cursor(cursor);
        }
        Ok(tl)
    }

    /// Open as a fresh session. History before the save is not persisted in
    /// v1: the saved state becomes the new root.
    pub fn into_session(self) -> Result<Session, ProjectError> {
        Ok(Session::new(self.into_timeline()?))
    }
}

/// Bring an older document up to [`FORMAT_VERSION`].
///
/// The hook exists from day one so a schema change is a migration arm rather
/// than a breaking change. Version 0 is pre-release: no `version` field and
/// no command log.
pub fn migrate(mut value: Value) -> Result<Value, ProjectError> {
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    if version > FORMAT_VERSION {
        return Err(ProjectError::TooNew(version));
    }
    let Some(obj) = value.as_object_mut() else {
        return Err(ProjectError::Malformed(
            "the top level of a project must be an object".into(),
        ));
    };
    if version == 0 {
        if !obj.contains_key("snapshot") {
            return Err(ProjectError::Malformed(
                "the project has no timeline in it".into(),
            ));
        }
        obj.entry("log").or_insert_with(|| Value::Array(vec![]));
        obj.insert("version".into(), Value::from(FORMAT_VERSION));
    }
    Ok(value)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use vimci_core::Frame;
    use vimci_core::testing::{fixture, track_id};

    fn saved_session() -> Session {
        let mut s = Session::new(fixture(&[("V1", &[(0, 300, "a")])]));
        s.set_snapshot_interval(0);
        let track = track_id(s.timeline(), "V1");
        for frame in [100, 200] {
            s.exec(&EditCommand::Split {
                track,
                frame: Frame(frame),
                new_id: None,
            })
            .unwrap();
        }
        s
    }

    #[test]
    fn a_project_round_trips_to_the_same_timeline() {
        let s = saved_session();
        let file = ProjectFile::from_session(&s);
        assert_eq!(file.version, FORMAT_VERSION);
        assert_eq!(
            file.log.len(),
            2,
            "the log carries the edits since the root"
        );

        let text = file.to_json().unwrap();
        let reloaded = ProjectFile::from_json(&text).unwrap();
        let timeline = reloaded.into_timeline().unwrap();
        assert_eq!(&timeline, s.timeline(), "reload must be byte-identical");
    }

    #[test]
    fn a_save_point_stores_a_snapshot_with_an_empty_log() {
        let mut s = saved_session();
        s.mark_saved();
        let file = ProjectFile::from_session(&s);
        assert!(file.log.is_empty());
        assert_eq!(&file.snapshot, s.timeline());
    }

    #[test]
    fn opening_a_project_starts_a_fresh_history() {
        let s = saved_session();
        let text = ProjectFile::from_session(&s).to_json().unwrap();
        let opened = ProjectFile::from_json(&text)
            .unwrap()
            .into_session()
            .unwrap();
        assert_eq!(opened.timeline(), s.timeline());
        assert!(opened.undolist().is_empty());
    }

    #[test]
    fn a_version_zero_document_is_migrated_forward() {
        let s = saved_session();
        let snapshot = serde_json::to_value(s.timeline()).unwrap();
        // Version 0: no `version` field, no command log.
        let v0 = serde_json::json!({ "snapshot": snapshot });
        let file = ProjectFile::from_json(&v0.to_string()).unwrap();
        assert_eq!(file.version, FORMAT_VERSION);
        assert!(file.log.is_empty());
        assert_eq!(&file.into_timeline().unwrap(), s.timeline());
    }

    #[test]
    fn a_newer_format_is_refused_with_a_readable_message() {
        let text = serde_json::json!({ "version": FORMAT_VERSION + 1, "snapshot": {} }).to_string();
        let err = ProjectFile::from_json(&text).unwrap_err();
        assert_eq!(err, ProjectError::TooNew(FORMAT_VERSION + 1));
        assert_eq!(err.class(), ErrorClass::User);
        assert!(err.user_message().contains("newer vimci"));
    }

    #[test]
    fn a_malformed_document_is_corruption_not_a_crash() {
        for text in ["", "{", "[]", "{\"version\":0}", "null"] {
            let err = ProjectFile::from_json(text).unwrap_err();
            assert_eq!(err.class(), ErrorClass::Corruption, "{text}");
            assert!(!err.user_message().is_empty());
        }
    }

    #[test]
    fn an_unreplayable_log_is_reported_not_applied() {
        let s = saved_session();
        let mut file = ProjectFile::from_session(&s);
        file.log.push(EditCommand::Join {
            track: track_id(s.timeline(), "V1"),
            frame: Frame(7),
        });
        let err = file.into_timeline().unwrap_err();
        assert!(matches!(err, ProjectError::Unreplayable(_)));
        assert_eq!(err.class(), ErrorClass::Corruption);
    }

    /// Cheap deterministic fuzz of the deserializer: no input may panic, and
    /// every rejection must carry a message (plan.md Phase 2).
    #[test]
    fn the_deserializer_never_panics_on_junk() {
        let good = ProjectFile::from_session(&saved_session())
            .to_json()
            .unwrap();
        let bytes = good.as_bytes();
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        for _ in 0..2000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let cut = (seed as usize) % bytes.len();
            let mut mutated = bytes.to_vec();
            mutated.truncate(cut);
            if seed.is_multiple_of(3) {
                mutated.push(b'}');
            }
            if seed.is_multiple_of(5) && cut > 0 {
                let at = (seed >> 8) as usize % cut;
                if let Some(b) = mutated.get_mut(at) {
                    *b = (seed >> 16) as u8 | 0x20;
                }
            }
            let text = String::from_utf8_lossy(&mutated).into_owned();
            if let Err(e) = ProjectFile::from_json(&text) {
                assert!(!e.user_message().is_empty());
            }
        }
    }
}
