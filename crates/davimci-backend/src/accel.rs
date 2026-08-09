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

/// Whether the backend may encode in hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncodePolicy {
    /// Software encode for every export. The reference path, and the one
    /// every `ffprobe` export assertion runs against.
    #[default]
    Cpu,
    /// Hardware encode where it can meet the preset, software otherwise.
    Auto,
}

impl EncodePolicy {
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

impl fmt::Display for EncodePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What one export asks of a hardware encoder.
///
/// Export correctness outranks export speed, which is what the third variant
/// is for: a preset that names a hardware encode is refused when the machine
/// cannot deliver it, never quietly re-encoded in software at a different
/// quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HardwareEncode {
    /// Software, whatever the machine has.
    #[default]
    Off,
    /// Hardware where it meets the preset; software otherwise, silently,
    /// because nothing was promised.
    Preferred,
    /// Hardware or nothing: the job is refused before it starts.
    Required,
}

impl HardwareEncode {
    #[must_use]
    pub fn wanted(self) -> bool {
        !matches!(self, Self::Off)
    }

    #[must_use]
    pub fn required(self) -> bool {
        matches!(self, Self::Required)
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
    /// The encoder policy in force, reported alongside decode so one health
    /// line answers "what is accelerated".
    pub encode: EncodePolicy,
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
            encode: EncodePolicy::default(),
            decoder: None,
            detail: detail.into(),
        }
    }

    /// A hardware path that a probe accepted.
    #[must_use]
    pub fn hardware(decoder: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            policy: DecodePolicy::Auto,
            encode: EncodePolicy::default(),
            decoder: Some(decoder.into()),
            detail: detail.into(),
        }
    }

    /// The same status with the encode policy filled in.
    #[must_use]
    pub fn with_encode(mut self, encode: EncodePolicy) -> Self {
        self.encode = encode;
        self
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
        assert_eq!(EncodePolicy::default(), EncodePolicy::Cpu);
        assert_eq!(HardwareEncode::default(), HardwareEncode::Off);
        assert!(!AccelerationStatus::default().is_hardware());
    }

    #[test]
    fn only_a_required_hardware_encode_may_refuse_a_job() {
        assert!(!HardwareEncode::Off.wanted());
        assert!(HardwareEncode::Preferred.wanted());
        assert!(!HardwareEncode::Preferred.required());
        assert!(HardwareEncode::Required.required());
    }

    #[test]
    fn an_encode_policy_round_trips_through_its_name() {
        for policy in [EncodePolicy::Cpu, EncodePolicy::Auto] {
            assert_eq!(EncodePolicy::parse(policy.name()), Some(policy));
        }
        assert_eq!(EncodePolicy::parse("vaapi"), None);
    }
}
