//! The `:set` property registry.
//!
//! One command, one typed registry: parsing a property name and its value is
//! a pure function here, so every setter is proved without a filesystem, a
//! backend or a window. Execution lives in `excmd`/`editor`, which are the
//! only layers that own a session or a preview.

pub use davimci_app::Numbers;
use davimci_core::{ClipProps, Fps, Frame, Resolution, Transition};

use crate::audio::FadeEnd;
use crate::error::CliError;

/// Which number of a clip's transform is being set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformField {
    X,
    Y,
    Scale,
    Opacity,
}

impl TransformField {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::X => "clip.x",
            Self::Y => "clip.y",
            Self::Scale => "clip.scale",
            Self::Opacity => "clip.opacity",
        }
    }
}

/// A parsed, range-checked `:set`. Constructing one is the whole validation:
/// a `Setting` that exists is a `Setting` that can be applied.
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    /// `clip.x|y|scale|opacity`
    Transform(TransformField, f32),
    /// `clip.gain <db>`
    Gain(f32),
    /// `clip.fade_in|fade_out <ms>`
    Fade(FadeEnd, u64),
    /// `transition.duration <frames>`
    TransitionDuration(u64),
    /// `transition.type <name>`
    TransitionType(String),
    /// `timeline.fps <rate>`
    TimelineFps(Fps),
    /// `timeline.resolution <WxH>`
    TimelineResolution(Resolution),
    /// `preview on|off` - a view setting, never an edit.
    Preview(bool),
    /// `previewheight auto|<rows>|<percent>%` - the terminal's inline preview
    /// band; `0` is off. Inert outside the terminal frontend.
    PreviewHeight(PreviewHeight),
    /// `previewprotocol auto|kitty|sixel|blocks` - as above.
    PreviewProtocol(PreviewProtocol),
    /// `numbers none|absolute|relative` - how the terminal ruler labels its
    /// jump points. Inert outside the terminal frontend.
    Numbers(Numbers),
}

/// What `:set previewheight` accepts.
///
/// Only the terminal can turn any of these into rows: two of the three depend
/// on the screen, and `Auto` depends on the picture as well. The registry's
/// job is to reject what is not one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewHeight {
    /// `0` - no band.
    Off,
    /// A row count, capped by the terminal.
    Rows(u16),
    /// `50%` of the screen, recomputed on resize.
    Percent(u8),
    /// As many rows as the picture can fill at the current width.
    Auto,
}

impl PreviewHeight {
    /// The value as `:set previewheight` would take it back.
    #[must_use]
    pub fn value(self) -> String {
        match self {
            Self::Off => "0".into(),
            Self::Rows(rows) => rows.to_string(),
            Self::Percent(pc) => format!("{pc}%"),
            Self::Auto => "auto".into(),
        }
    }

    /// How the setting reads back, for the status line and completion.
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Off => "inline preview off".into(),
            Self::Rows(rows) => format!("inline preview {rows} rows"),
            Self::Percent(pc) => format!("inline preview {pc}% of the screen"),
            Self::Auto => "inline preview auto".into(),
        }
    }
}

/// What `:set previewprotocol` accepts. `Auto` defers to the terminal
/// frontend's own detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewProtocol {
    Auto,
    Kitty,
    Sixel,
    Blocks,
}

impl PreviewProtocol {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Kitty => "kitty",
            Self::Sixel => "sixel",
            Self::Blocks => "blocks",
        }
    }
}

impl Setting {
    /// True for settings that only change what the session shows, and so
    /// must not enter the undo log.
    #[must_use]
    pub fn is_view_only(&self) -> bool {
        matches!(
            self,
            Self::Preview(_) | Self::PreviewHeight(_) | Self::PreviewProtocol(_) | Self::Numbers(_)
        )
    }
}

/// Every property `:set` accepts, for completion and for the error message
/// an unknown name produces.
pub const PROPERTIES: &[&str] = &[
    "clip.x",
    "clip.y",
    "clip.scale",
    "clip.opacity",
    "clip.gain",
    "clip.fade_in",
    "clip.fade_out",
    "transition.duration",
    "transition.type",
    "timeline.fps",
    "timeline.resolution",
    "preview",
    "previewheight",
    "previewprotocol",
    "numbers",
];

/// The values a property enumerates, for completion. Empty for a property
/// whose values cannot be listed - a number, a resolution, a transition name
/// the host installs - where the current value is offered instead.
#[must_use]
pub fn values(prop: &str) -> Vec<String> {
    let words: &[&str] = match prop {
        "preview" => &["on", "off"],
        "previewheight" => &["auto"],
        "previewprotocol" => &["auto", "kitty", "sixel", "blocks"],
        "numbers" => Numbers::NAMES,
        _ => &[],
    };
    words.iter().map(|w| (*w).to_string()).collect()
}

/// What every `:set` property currently holds, so completion can show a
/// free-form value rather than nothing.
///
/// A snapshot, not a handle: the registry stays parseable without a session,
/// and whoever owns the state decides when to take one.
#[derive(Debug, Clone, Default)]
pub struct CurrentSettings {
    pub preview: Option<bool>,
    pub preview_height: Option<PreviewHeight>,
    pub preview_protocol: Option<PreviewProtocol>,
    pub numbers: Option<Numbers>,
    pub fps: Option<Fps>,
    pub resolution: Option<Resolution>,
    /// The clip the next `:set clip.*` would act on.
    pub clip: Option<ClipProps>,
    /// The transition the next `:set transition.*` would act on.
    pub transition: Option<Transition>,
}

impl CurrentSettings {
    /// What `prop` reads right now, spelled as `:set` would take it back.
    #[must_use]
    pub fn value(&self, prop: &str) -> Option<String> {
        let clip = self.clip.as_ref();
        let ms = |frames: Frame| -> Option<String> {
            let fps = self.fps?;
            Some((crate::audio::ms_for_frames(frames.get(), fps)).to_string())
        };
        match prop {
            "clip.x" => clip.map(|c| c.transform.x.to_string()),
            "clip.y" => clip.map(|c| c.transform.y.to_string()),
            "clip.scale" => clip.map(|c| c.transform.scale.to_string()),
            "clip.opacity" => clip.map(|c| c.transform.opacity.to_string()),
            "clip.gain" => clip.map(|c| c.gain_db.to_string()),
            "clip.fade_in" => clip.and_then(|c| ms(c.fade_in)),
            "clip.fade_out" => clip.and_then(|c| ms(c.fade_out)),
            "transition.duration" => self
                .transition
                .as_ref()
                .map(|t| t.duration.get().to_string()),
            "transition.type" => self.transition.as_ref().map(|t| t.kind.clone()),
            "timeline.fps" => self.fps.map(|f| {
                if f.den == 1 {
                    f.num.to_string()
                } else {
                    format!("{}/{}", f.num, f.den)
                }
            }),
            "timeline.resolution" => self.resolution.map(|r| r.to_string()),
            "preview" => self
                .preview
                .map(|on| if on { "on" } else { "off" }.to_string()),
            "previewheight" => self.preview_height.map(PreviewHeight::value),
            "previewprotocol" => self.preview_protocol.map(|p| p.name().to_string()),
            "numbers" => self.numbers.map(|n| n.name().to_string()),
            _ => None,
        }
    }

    /// The completion candidates for `prop`: its enumerated values, with the
    /// current value first when it is not one of them.
    #[must_use]
    pub fn candidates(&self, prop: &str) -> Vec<String> {
        let mut out = values(prop);
        if let Some(current) = self.value(prop)
            && !out.contains(&current)
        {
            out.insert(0, current);
        }
        out
    }
}

fn bad(prop: &str, expected: &str) -> CliError {
    CliError::BadPropertyValue {
        prop: prop.to_string(),
        expected: expected.to_string(),
    }
}

fn number(prop: &str, value: &str, expected: &str) -> Result<f32, CliError> {
    value
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| bad(prop, expected))
}

fn ranged(prop: &str, value: &str, lo: f32, hi: f32) -> Result<f32, CliError> {
    let expected = format!("a number between {lo} and {hi}");
    let v = number(prop, value, &expected)?;
    if v < lo || v > hi {
        return Err(bad(prop, &expected));
    }
    Ok(v)
}

/// Parse `:set <prop> <value>`. Both an unknown property and an
/// out-of-range value are user errors, named in the sentence they produce.
pub fn parse(prop: &str, value: &str) -> Result<Setting, CliError> {
    match prop {
        "clip.x" => Ok(Setting::Transform(
            TransformField::X,
            number(prop, value, "a number of pixels")?,
        )),
        "clip.y" => Ok(Setting::Transform(
            TransformField::Y,
            number(prop, value, "a number of pixels")?,
        )),
        // A negative or zero scale is a clip that is not there; a mirrored
        // clip is a different feature, not a scale of -1.
        "clip.scale" => Ok(Setting::Transform(
            TransformField::Scale,
            ranged(prop, value, f32::MIN_POSITIVE, 100.0)?,
        )),
        "clip.opacity" => Ok(Setting::Transform(
            TransformField::Opacity,
            ranged(prop, value, 0.0, 1.0)?,
        )),
        "clip.gain" => Ok(Setting::Gain(ranged(prop, value, -96.0, 24.0)?)),
        "clip.fade_in" | "clip.fade_out" => {
            let ms = value
                .parse::<u64>()
                .map_err(|_| bad(prop, "a duration in milliseconds"))?;
            let end = if prop.ends_with("in") {
                FadeEnd::In
            } else {
                FadeEnd::Out
            };
            Ok(Setting::Fade(end, ms))
        }
        "transition.duration" => {
            let frames = value
                .parse::<u64>()
                .ok()
                .filter(|f| *f > 0)
                .ok_or_else(|| bad(prop, "a positive number of frames"))?;
            Ok(Setting::TransitionDuration(frames))
        }
        "transition.type" => {
            if value.is_empty() {
                return Err(bad(prop, "a transition name"));
            }
            Ok(Setting::TransitionType(value.to_string()))
        }
        "timeline.fps" => parse_fps(value)
            .map(Setting::TimelineFps)
            .ok_or_else(|| bad(prop, "a framerate such as 25, 60 or 30000/1001")),
        "timeline.resolution" => parse_resolution(value)
            .map(Setting::TimelineResolution)
            .ok_or_else(|| bad(prop, "a resolution such as 1920x1080")),
        "preview" => match value {
            "on" | "true" | "1" => Ok(Setting::Preview(true)),
            "off" | "false" | "0" => Ok(Setting::Preview(false)),
            _ => Err(bad(prop, "on or off")),
        },
        // The upper bound is half the screen, which only the terminal knows;
        // it clamps, so any row count parses here.
        "previewheight" => preview_height(prop, value).map(Setting::PreviewHeight),
        "previewprotocol" => match value {
            "auto" => Ok(Setting::PreviewProtocol(PreviewProtocol::Auto)),
            "kitty" => Ok(Setting::PreviewProtocol(PreviewProtocol::Kitty)),
            "sixel" => Ok(Setting::PreviewProtocol(PreviewProtocol::Sixel)),
            "blocks" => Ok(Setting::PreviewProtocol(PreviewProtocol::Blocks)),
            _ => Err(bad(prop, "auto, kitty, sixel or blocks")),
        },
        "numbers" => Numbers::parse(value)
            .map(Setting::Numbers)
            .ok_or_else(|| bad(prop, "none, absolute, relative or both")),
        other => Err(CliError::UnknownProperty(other.to_string())),
    }
}

fn preview_height(prop: &str, value: &str) -> Result<PreviewHeight, CliError> {
    let expected = "auto, a number of rows, or a percentage such as 50%";
    if value == "auto" {
        return Ok(PreviewHeight::Auto);
    }
    if let Some(pc) = value.strip_suffix('%') {
        // 0% is off, and above 100 is meaningless rather than merely capped:
        // a typo in a percentage should be heard about.
        return match pc.parse::<u8>() {
            Ok(0) => Ok(PreviewHeight::Off),
            Ok(pc) if pc <= 100 => Ok(PreviewHeight::Percent(pc)),
            _ => Err(bad(prop, expected)),
        };
    }
    match value.parse::<u16>() {
        Ok(0) => Ok(PreviewHeight::Off),
        Ok(rows) => Ok(PreviewHeight::Rows(rows)),
        Err(_) => Err(bad(prop, expected)),
    }
}

/// `25`, `29.97` or an exact `30000/1001`. Decimals near a broadcast rate
/// snap to the exact ratio, since `29.97` is never what the user means
/// literally.
fn parse_fps(value: &str) -> Option<Fps> {
    if let Some((num, den)) = value.split_once('/') {
        return Fps::new(num.trim().parse().ok()?, den.trim().parse().ok()?).ok();
    }
    let rate: f64 = value.parse().ok()?;
    if !rate.is_finite() || rate <= 0.0 {
        return None;
    }
    for exact in [Fps::FPS_23_976, Fps::FPS_29_97, Fps::FPS_59_94] {
        if (exact.as_f64() - rate).abs() < 0.01 {
            return Some(exact);
        }
    }
    let whole = rate.round();
    ((whole - rate).abs() < f64::EPSILON && whole <= f64::from(u32::MAX))
        .then(|| Fps::new(whole as u32, 1).ok())
        .flatten()
}

/// `1920x1080`, with either `x` or `X` between the two.
fn parse_resolution(value: &str) -> Option<Resolution> {
    let (w, h) = value.split_once(['x', 'X'])?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some(Resolution {
        width: w,
        height: h,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_core::Classify;

    /// Table-driven: property, input, the setting it must produce.
    #[test]
    fn accepted_properties_parse_to_their_setting() {
        let cases: &[(&str, &str, Setting)] = &[
            (
                "clip.x",
                "-40",
                Setting::Transform(TransformField::X, -40.0),
            ),
            (
                "clip.y",
                "12.5",
                Setting::Transform(TransformField::Y, 12.5),
            ),
            (
                "clip.scale",
                "0.5",
                Setting::Transform(TransformField::Scale, 0.5),
            ),
            (
                "clip.opacity",
                "1",
                Setting::Transform(TransformField::Opacity, 1.0),
            ),
            ("clip.gain", "-6", Setting::Gain(-6.0)),
            ("clip.fade_in", "250", Setting::Fade(FadeEnd::In, 250)),
            ("clip.fade_out", "80", Setting::Fade(FadeEnd::Out, 80)),
            ("transition.duration", "30", Setting::TransitionDuration(30)),
            (
                "transition.type",
                "wipe",
                Setting::TransitionType("wipe".into()),
            ),
            ("timeline.fps", "25", Setting::TimelineFps(Fps::FPS_25)),
            (
                "timeline.fps",
                "29.97",
                Setting::TimelineFps(Fps::FPS_29_97),
            ),
            (
                "timeline.fps",
                "30000/1001",
                Setting::TimelineFps(Fps::FPS_29_97),
            ),
            (
                "timeline.resolution",
                "1280x720",
                Setting::TimelineResolution(Resolution {
                    width: 1280,
                    height: 720,
                }),
            ),
            ("preview", "off", Setting::Preview(false)),
            ("preview", "on", Setting::Preview(true)),
            (
                "previewheight",
                "0",
                Setting::PreviewHeight(PreviewHeight::Off),
            ),
            (
                "previewheight",
                "8",
                Setting::PreviewHeight(PreviewHeight::Rows(8)),
            ),
            (
                "previewheight",
                "auto",
                Setting::PreviewHeight(PreviewHeight::Auto),
            ),
            (
                "previewheight",
                "50%",
                Setting::PreviewHeight(PreviewHeight::Percent(50)),
            ),
            (
                "previewheight",
                "0%",
                Setting::PreviewHeight(PreviewHeight::Off),
            ),
            (
                "previewprotocol",
                "auto",
                Setting::PreviewProtocol(PreviewProtocol::Auto),
            ),
            (
                "previewprotocol",
                "sixel",
                Setting::PreviewProtocol(PreviewProtocol::Sixel),
            ),
            ("numbers", "none", Setting::Numbers(Numbers::Off)),
            ("numbers", "absolute", Setting::Numbers(Numbers::Absolute)),
            ("numbers", "relative", Setting::Numbers(Numbers::Relative)),
            ("numbers", "both", Setting::Numbers(Numbers::Both)),
            ("numbers", "current", Setting::Numbers(Numbers::Both)),
        ];
        for (prop, value, want) in cases {
            assert_eq!(
                parse(prop, value).ok().as_ref(),
                Some(want),
                "{prop} {value}"
            );
        }
    }

    #[test]
    fn out_of_range_and_unknown_properties_are_user_errors_naming_the_property() {
        for (prop, value) in [
            ("clip.opacity", "2"),
            ("clip.scale", "0"),
            ("clip.gain", "99"),
            ("clip.x", "left"),
            ("transition.duration", "0"),
            ("timeline.fps", "0"),
            ("timeline.resolution", "1920"),
            ("preview", "maybe"),
            ("previewheight", "tall"),
            ("previewheight", "-1"),
            ("previewheight", "101%"),
            ("previewheight", "half%"),
            ("previewheight", "%"),
            ("previewprotocol", "iterm"),
            ("numbers", "sideways"),
            ("clip.wobble", "1"),
        ] {
            let e = parse(prop, value).expect_err("must reject");
            assert_eq!(e.class(), davimci_core::ErrorClass::User);
            assert!(
                e.user_message().contains(prop),
                "{prop}: {}",
                e.user_message()
            );
        }
    }

    #[test]
    fn view_settings_never_enter_the_undo_log() {
        assert!(parse("preview", "off").unwrap().is_view_only());
        assert!(parse("previewheight", "6").unwrap().is_view_only());
        assert!(parse("previewprotocol", "kitty").unwrap().is_view_only());
        assert!(parse("numbers", "relative").unwrap().is_view_only());
        assert!(!parse("clip.gain", "0").unwrap().is_view_only());
    }
}
