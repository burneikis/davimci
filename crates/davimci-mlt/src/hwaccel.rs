//! The hardware-decode capability probe.
//!
//! Enforces two rules from the GPU plan: acceleration is opt-in and never
//! makes a machine worse off, and every failure degrades to software decode
//! with one complete sentence rather than an error. The decision is taken
//! *per source*, because hardware decode plus readback loses to software
//! decode on short-GOP or small pictures.
//!
//! Nothing here touches MLT or ffmpeg. It answers "is there a device, and is
//! this source worth handing to it", which keeps the whole policy unit
//! testable without a GPU - the property the lavapipe and CI runs depend on.

use std::path::{Path, PathBuf};

use davimci_backend::{AccelerationStatus, DecodePolicy, EncodePolicy, HardwareEncode};

/// The codecs whose long-GOP decode is expensive enough that a hardware
/// decode plus a readback still wins. Anything not listed stays on the CPU:
/// intra-only and legacy codecs decode faster than the surface can be copied
/// back.
const LONG_GOP: &[&str] = &["h264", "hevc", "h265", "vp9", "av1", "mpeg2video"];

/// Below this many pixels the readback dominates and software decode wins,
/// measured per source rather than assumed globally. 1280x720.
const MIN_PIXELS: u64 = 1280 * 720;

/// Where Linux exposes render nodes. A render node is the unprivileged
/// device: the card node needs a DRM master and is the wrong thing to open.
const DRI: &str = "/dev/dri";

/// What one source would be decoded with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The ffmpeg hwaccel name, as MLT's `hwaccel` property takes it.
    pub method: &'static str,
    /// The render node, as MLT's `hwaccel_device` property takes it.
    pub device: String,
}

/// The software encoders that have a VAAPI equivalent, keyed by the encoder
/// name a preset resolves to. Same codec on both sides: a substitution that
/// changed the codec would change what the preset promised.
const ENCODERS: &[(&str, &str)] = &[
    ("libx264", "h264_vaapi"),
    ("libx265", "hevc_vaapi"),
    ("libvpx-vp9", "vp9_vaapi"),
];

/// What an export will actually encode with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeChoice {
    /// The software encoder the preset resolved to.
    Software,
    /// A hardware encoder that meets the preset, plus the device to open.
    Hardware { encoder: String, device: String },
    /// The job must not start: a required hardware encode cannot be
    /// delivered. Carries the complete sentence to refuse it with.
    Refused(String),
}

/// The session's decode policy plus what the probe found.
#[derive(Debug, Clone)]
pub struct Acceleration {
    policy: DecodePolicy,
    encode: EncodePolicy,
    /// Hardware encoders that have been tried, and whether they worked.
    ///
    /// A render node says nothing about *encode* entrypoints: plenty of
    /// cards decode H.264 and cannot encode it. Presence is not capability,
    /// so an encoder is only trusted once it has produced a file.
    verified: std::collections::BTreeMap<String, bool>,
    device: Option<String>,
    /// Why there is no device, when there is none.
    unavailable: Option<String>,
    /// Set by a decode that failed after the probe passed. Sticky for the
    /// session: a device that dropped out once is not asked again mid-edit.
    failure: Option<String>,
}

impl Default for Acceleration {
    fn default() -> Self {
        Self::new(DecodePolicy::default())
    }
}

impl Acceleration {
    /// Probe the machine and start on `policy`.
    #[must_use]
    pub fn new(policy: DecodePolicy) -> Self {
        Self::with_devices(policy, &render_nodes(Path::new(DRI)))
    }

    /// The probe with its device list injected, so the no-device and
    /// device-present paths are both testable without a GPU.
    #[must_use]
    pub fn with_devices(policy: DecodePolicy, devices: &[PathBuf]) -> Self {
        let device = devices.first().map(|d| d.display().to_string());
        let unavailable = device.is_none().then(|| {
            "Hardware decode is off because this machine has no usable render device; \
             davimci is decoding in software."
                .to_string()
        });
        Self {
            policy,
            encode: EncodePolicy::default(),
            verified: std::collections::BTreeMap::new(),
            device,
            unavailable,
            failure: None,
        }
    }

    /// What a trial encode found, or `None` if this encoder has not been
    /// tried yet.
    #[must_use]
    pub fn encoder_verified(&self, encoder: &str) -> Option<bool> {
        self.verified.get(encoder).copied()
    }

    /// Record what a trial encode found. Sticky for the session: a card
    /// without an encode entrypoint will not grow one.
    pub fn mark_encoder(&mut self, encoder: &str, works: bool) {
        self.verified.insert(encoder.to_string(), works);
    }

    /// The sentence that refuses a required hardware encode this machine
    /// cannot perform.
    #[must_use]
    pub fn encoder_refusal(encoder: &str) -> String {
        format!(
            "This export was refused because the preset requires a hardware encoder and this \
             machine's graphics driver has no working {encoder} encoder."
        )
    }

    #[must_use]
    pub fn encode_policy(&self) -> EncodePolicy {
        self.encode
    }

    /// `:set encode cpu|auto`. Reports what exports will now do.
    pub fn set_encode_policy(&mut self, encode: EncodePolicy) -> AccelerationStatus {
        self.encode = encode;
        self.status()
    }

    /// What to encode one export with.
    ///
    /// `software` is the encoder the preset resolved to, which is what a
    /// hardware encode has to match: the substitution keeps the codec and
    /// only changes who does the work.
    #[must_use]
    pub fn encoder_for(&self, software: &str, request: HardwareEncode) -> EncodeChoice {
        // `:set encode auto` is the session asking for acceleration wherever
        // it meets the preset, which is exactly what `Preferred` means.
        let request = match (request, self.encode) {
            (HardwareEncode::Off, EncodePolicy::Auto) => HardwareEncode::Preferred,
            (other, _) => other,
        };
        if !request.wanted() || (self.encode == EncodePolicy::Cpu && !request.required()) {
            return EncodeChoice::Software;
        }
        let mapped = ENCODERS
            .iter()
            .find(|(cpu, _)| *cpu == software)
            .map(|(_, gpu)| (*gpu).to_string());
        match (mapped, self.device.clone()) {
            (Some(encoder), Some(device)) => EncodeChoice::Hardware { encoder, device },
            (None, _) if request.required() => EncodeChoice::Refused(format!(
                "This export was refused because the preset requires a hardware encoder and no \
                 hardware encoder produces what {software} produces."
            )),
            (_, None) if request.required() => EncodeChoice::Refused(
                "This export was refused because the preset requires a hardware encoder and this \
                 machine has no usable render device."
                    .into(),
            ),
            // Preferred, not required: an export that cannot be accelerated
            // is still an export, and nothing was promised about its speed.
            _ => EncodeChoice::Software,
        }
    }

    #[must_use]
    pub fn policy(&self) -> DecodePolicy {
        self.policy
    }

    /// Switch policy. Returns the status to show the user, which says what
    /// the session is actually doing rather than what was asked for.
    pub fn set_policy(&mut self, policy: DecodePolicy) -> AccelerationStatus {
        self.policy = policy;
        // A new opt-in deserves a fresh attempt: the sticky failure describes
        // the previous session state, not a permanent property of the device.
        if policy == DecodePolicy::Auto {
            self.failure = None;
        }
        self.status()
    }

    /// A decode that failed with hardware after the probe accepted the
    /// device. Sticky, so the rest of the session stays on software rather
    /// than failing once per frame.
    pub fn record_failure(&mut self, reason: &str) -> AccelerationStatus {
        self.failure = Some(format!(
            "Hardware decode stopped working ({reason}), so davimci has fallen back to \
             software decode for this session."
        ));
        self.status()
    }

    /// What the session is doing, as a complete sentence.
    #[must_use]
    pub fn status(&self) -> AccelerationStatus {
        self.decode_status().with_encode(self.encode)
    }

    fn decode_status(&self) -> AccelerationStatus {
        if self.policy == DecodePolicy::Cpu {
            return AccelerationStatus::cpu(
                self.policy,
                "Hardware decode is off because the decode setting is cpu.",
            );
        }
        if let Some(reason) = self.failure.as_ref().or(self.unavailable.as_ref()) {
            return AccelerationStatus::cpu(self.policy, reason.clone());
        }
        match &self.device {
            Some(device) => AccelerationStatus::hardware(
                "vaapi",
                format!(
                    "Hardware decode is using VAAPI on {device} for long-GOP sources; \
                     everything else decodes in software."
                ),
            ),
            None => AccelerationStatus::cpu(
                self.policy,
                "Hardware decode is off because no render device was found; davimci is \
                 decoding in software."
                    .to_string(),
            ),
        }
    }

    /// Whether hardware is available at all right now, ignoring the source.
    #[must_use]
    fn active(&self) -> bool {
        self.policy == DecodePolicy::Auto && self.failure.is_none() && self.device.is_some()
    }

    /// What to decode one source with, given its codec and picture size.
    ///
    /// `None` means software, which is the answer for an unknown codec: a
    /// codec the probe cannot reason about is not worth a readback on a
    /// guess.
    #[must_use]
    pub fn choose(&self, codec: Option<&str>, pixels: u64) -> Option<Choice> {
        if !self.active() {
            return None;
        }
        let codec = codec?.to_ascii_lowercase();
        if !LONG_GOP.contains(&codec.as_str()) || pixels < MIN_PIXELS {
            return None;
        }
        Some(Choice {
            method: "vaapi",
            device: self.device.clone()?,
        })
    }
}

/// The render nodes on this machine, sorted so the choice is deterministic.
fn render_nodes(dri: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dri) else {
        return Vec::new();
    };
    let mut nodes: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn with_device(policy: DecodePolicy) -> Acceleration {
        Acceleration::with_devices(policy, &[PathBuf::from("/dev/dri/renderD128")])
    }

    #[test]
    fn no_device_degrades_to_software_with_one_sentence() {
        let accel = Acceleration::with_devices(DecodePolicy::Auto, &[]);
        let status = accel.status();
        assert!(!status.is_hardware());
        assert!(status.detail.ends_with('.'));
        assert!(accel.choose(Some("h264"), 1920 * 1080).is_none());
    }

    #[test]
    fn a_device_that_cannot_decode_the_codec_stays_on_software() {
        let accel = with_device(DecodePolicy::Auto);
        assert!(accel.status().is_hardware());
        // Intra-only and unknown codecs are not worth the readback.
        assert!(accel.choose(Some("prores"), 3840 * 2160).is_none());
        assert!(accel.choose(None, 3840 * 2160).is_none());
    }

    #[test]
    fn a_mid_session_failure_is_sticky_and_recoverable() {
        let mut accel = with_device(DecodePolicy::Auto);
        assert!(accel.choose(Some("hevc"), 3840 * 2160).is_some());
        let status = accel.record_failure("the VAAPI driver stopped responding");
        assert!(!status.is_hardware());
        assert!(status.detail.ends_with('.'));
        assert!(accel.choose(Some("hevc"), 3840 * 2160).is_none());
    }

    #[test]
    fn an_export_that_asks_for_nothing_gets_the_software_encoder() {
        let accel = with_device(DecodePolicy::Auto);
        assert_eq!(
            accel.encoder_for("libx264", HardwareEncode::Off),
            EncodeChoice::Software
        );
        // The session default is cpu, so `Preferred` alone changes nothing.
        assert_eq!(
            accel.encoder_for("libx264", HardwareEncode::Preferred),
            EncodeChoice::Software
        );
    }

    #[test]
    fn set_encode_auto_substitutes_the_encoder_for_the_same_codec() {
        let mut accel = with_device(DecodePolicy::Cpu);
        let status = accel.set_encode_policy(EncodePolicy::Auto);
        assert_eq!(status.encode, EncodePolicy::Auto);
        assert_eq!(
            accel.encoder_for("libx265", HardwareEncode::Preferred),
            EncodeChoice::Hardware {
                encoder: "hevc_vaapi".into(),
                device: "/dev/dri/renderD128".into(),
            }
        );
        // A preset that asked for nothing is accelerated too: that is what
        // the session policy is for.
        assert!(matches!(
            accel.encoder_for("libx264", HardwareEncode::Off),
            EncodeChoice::Hardware { .. }
        ));
        // A codec with no hardware encoder is not a refusal when nothing was
        // promised - it is simply encoded in software.
        assert_eq!(
            accel.encoder_for("prores_ks", HardwareEncode::Preferred),
            EncodeChoice::Software
        );
    }

    #[test]
    fn a_required_hardware_encode_is_refused_rather_than_downgraded() {
        let accel = with_device(DecodePolicy::Cpu);
        // Required beats the session policy: the preset is binding.
        assert!(matches!(
            accel.encoder_for("libx264", HardwareEncode::Required),
            EncodeChoice::Hardware { .. }
        ));
        let EncodeChoice::Refused(reason) =
            accel.encoder_for("prores_ks", HardwareEncode::Required)
        else {
            panic!("a codec with no hardware encoder must refuse");
        };
        assert!(reason.ends_with('.'));

        let none = Acceleration::with_devices(DecodePolicy::Cpu, &[]);
        let EncodeChoice::Refused(reason) = none.encoder_for("libx264", HardwareEncode::Required)
        else {
            panic!("a machine with no device must refuse a required hardware encode");
        };
        assert!(reason.ends_with('.'));
        // The same export without the requirement still runs.
        assert_eq!(
            none.encoder_for("libx264", HardwareEncode::Preferred),
            EncodeChoice::Software
        );
    }

    #[test]
    fn the_cpu_policy_never_chooses_hardware() {
        let accel = with_device(DecodePolicy::Cpu);
        assert!(accel.choose(Some("h264"), 3840 * 2160).is_none());
        assert!(!accel.status().is_hardware());
    }

    #[test]
    fn a_small_picture_stays_on_software_because_readback_dominates() {
        let accel = with_device(DecodePolicy::Auto);
        assert!(accel.choose(Some("h264"), 640 * 480).is_none());
        assert!(accel.choose(Some("h264"), 1920 * 1080).is_some());
    }

    #[test]
    fn asking_for_auto_again_retries_after_a_failure() {
        let mut accel = with_device(DecodePolicy::Auto);
        accel.record_failure("the device disappeared");
        let status = accel.set_policy(DecodePolicy::Auto);
        assert!(status.is_hardware());
    }

    #[test]
    fn only_render_nodes_are_offered() {
        let dir = std::env::temp_dir().join("davimci-hwaccel-probe");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("card0"), b"").unwrap();
        std::fs::write(dir.join("renderD129"), b"").unwrap();
        std::fs::write(dir.join("renderD128"), b"").unwrap();
        let nodes = render_nodes(&dir);
        assert_eq!(nodes, vec![dir.join("renderD128"), dir.join("renderD129")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_device_directory_is_not_an_error() {
        assert!(render_nodes(Path::new("/definitely/not/here")).is_empty());
    }
}
