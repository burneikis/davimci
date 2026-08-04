//! Stable identifiers for timeline entities.
//!
//! Ids are opaque and monotonic per timeline. They are never reused, so a
//! command log can refer to a clip that a later command deleted without
//! accidentally naming a different clip.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! define_id {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(pub u64);

        impl $name {
            #[must_use]
            pub fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
    };
}

define_id!(ClipId, "c", "Identifies a clip within one timeline.");
define_id!(TrackId, "t", "Identifies a track within one timeline.");
define_id!(GroupId, "g", "Identifies a per-clip linkage group.");

/// Monotonic id source. Held by the timeline; never reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IdGen {
    next: u64,
}

impl IdGen {
    #[must_use]
    pub fn new() -> Self {
        Self { next: 1 }
    }

    fn bump(&mut self) -> u64 {
        if self.next == 0 {
            self.next = 1;
        }
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }

    pub fn clip(&mut self) -> ClipId {
        ClipId(self.bump())
    }

    pub fn track(&mut self) -> TrackId {
        TrackId(self.bump())
    }

    pub fn group(&mut self) -> GroupId {
        GroupId(self.bump())
    }

    /// The id the next allocation would use.
    #[must_use]
    pub fn peek(self) -> u64 {
        self.next.max(1)
    }

    /// Force the next id. Callers must have checked that nothing on the
    /// timeline uses it - see [`crate::Timeline::set_id_cursor`].
    pub(crate) fn set(&mut self, next: u64) {
        self.next = next.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_across_kinds() {
        let mut ids = IdGen::new();
        let a = ids.clip().get();
        let b = ids.track().get();
        let c = ids.group().get();
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    #[test]
    fn the_cursor_can_be_rewound_and_advanced() {
        let mut ids = IdGen::new();
        let a = ids.clip();
        let _ = ids.clip();
        ids.set(a.get());
        assert_eq!(ids.clip(), a);
        ids.set(0);
        assert_eq!(ids.peek(), 1);
        ids.set(50);
        assert_eq!(ids.clip().get(), 50);
    }

    #[test]
    fn display_is_prefixed() {
        assert_eq!(ClipId(3).to_string(), "c3");
        assert_eq!(TrackId(4).to_string(), "t4");
        assert_eq!(GroupId(5).to_string(), "g5");
    }
}
