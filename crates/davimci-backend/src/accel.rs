//! Acceleration policy: how pixels are produced, never what the timeline
//! holds.
//!
//! Acceleration is a session policy, not a [`crate::job::RenderJob`] setting
//! and never a command: it changes the cost of a frame, not its content, so
//! it stays out of the project file and out of the undo log. Every failure
//! here is recoverable - the caller keeps editing on the CPU path and is told
//! why in one complete sentence.

use std::fmt;

/// Whether the backend may decode in hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecodePolicy {
    /// Software decode for every source. The reference path, and the only
    /// one the golden-pixel and cross-frontend parity tests run against.
    #[default]
    Cpu,
    /// Hardware decode for the sources a probe says it helps, software for
    /// the rest. The decision is per source, not global.
    Auto,
}

impl DecodePolicy {
    pub const NAMES: &'static [&'static str] = &["cpu", "auto"];

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cpu" | "software" => Some(Self::Cpu),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Auto => "auto",
        }
    }
}

impl fmt::Display for DecodePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What acceleration the backend is actually doing, and why.
///
/// `detail` is a complete user-facing sentence, so a status line or a health
/// report can print it unchanged: "why is this slow" must be answerable
/// without reading a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccelerationStatus {
    pub policy: DecodePolicy,
    /// The hardware decoder in use, such as `vaapi`, or `None` on the CPU
    /// path.
    pub decoder: Option<String>,
    pub detail: String,
}

impl AccelerationStatus {
    /// The CPU path, with the reason it is the one in use.
    #[must_use]
    pub fn cpu(policy: DecodePolicy, detail: impl Into<String>) -> Self {
        Self {
            policy,
            decoder: None,
            detail: detail.into(),
        }
    }

    /// A hardware path that a probe accepted.
    #[must_use]
    pub fn hardware(decoder: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            policy: DecodePolicy::Auto,
            decoder: Some(decoder.into()),
            detail: detail.into(),
        }
    }

    /// A backend with no acceleration to offer at all, such as the mock.
    #[must_use]
    pub fn unsupported() -> Self {
        Self::cpu(
            DecodePolicy::Cpu,
            "This backend decodes in software only, so the decode setting has no effect.",
        )
    }

    #[must_use]
    pub fn is_hardware(&self) -> bool {
        self.decoder.is_some()
    }
}

impl Default for AccelerationStatus {
    fn default() -> Self {
        Self::unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decode_policy_round_trips_through_its_name() {
        for policy in [DecodePolicy::Cpu, DecodePolicy::Auto] {
            assert_eq!(DecodePolicy::parse(policy.name()), Some(policy));
        }
        assert_eq!(DecodePolicy::parse("gpu"), None);
    }

    #[test]
    fn the_default_policy_is_the_reference_path() {
        assert_eq!(DecodePolicy::default(), DecodePolicy::Cpu);
        assert!(!AccelerationStatus::default().is_hardware());
    }
}
