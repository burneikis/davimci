//! Ruler jump-point numbers, decided once for every frontend.
//!
//! Which tick gets a number, what that number reads, and which numbers are
//! dropped because they would collide is view logic, so it lives here rather
//! than in the window or the terminal: `davimci-gui` and `davimci-tui` differ
//! only in the unit they measure a label in.

use crate::view::ViewState;

/// What the ruler labels its jump points with.
///
/// The terminal and window rulers are vim's line-number gutter turned on its
/// side: jump points are the lines, so `Relative` prints the count a motion
/// needs (`3l` lands on the tick labelled `3`) and `Absolute` prints the frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Numbers {
    #[default]
    Off,
    Absolute,
    Relative,
    /// Vim's `number` plus `relativenumber`: the playhead's own tick reads
    /// its absolute frame, every other tick reads the count that lands on it.
    Both,
}

impl Numbers {
    /// Every value `:set numbers` accepts, canonical spelling first, for
    /// completion.
    pub const NAMES: &'static [&'static str] = &["none", "absolute", "relative", "both", "current"];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::Absolute => "absolute",
            Self::Relative => "relative",
            Self::Both => "both",
        }
    }

    /// How the setting reads back on the status line.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Off => "no ruler numbers",
            Self::Absolute => "absolute ruler numbers",
            Self::Relative => "relative ruler numbers",
            Self::Both => "absolute at the playhead, relative elsewhere",
        }
    }

    /// Parse a `--numbers` argument or a `:set numbers` value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" | "off" | "no" => Some(Self::Off),
            "absolute" | "abs" | "on" => Some(Self::Absolute),
            "relative" | "rel" => Some(Self::Relative),
            "both" | "current" | "hybrid" => Some(Self::Both),
            _ => None,
        }
    }
}

/// How wide a label is in the caller's own unit - terminal cells for the TUI,
/// pixels for the GUI. All of these are in that one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelMetrics {
    /// Width of the ruler, measured from the column-zero end of it.
    pub width: u32,
    /// Distance from a tick to the start of its own label.
    pub gap: u32,
    /// Advance of one digit.
    pub digit: u32,
    /// Fixed extra a label needs beyond its digits, such as the padding a
    /// text box puts before its first glyph.
    pub padding: u32,
    /// Clear space demanded after a label, so two numbers do not read as one.
    pub separation: u32,
    /// Whether a label may extend past the next tick. A window draws its ticks
    /// over the numbers, so a long number crossing one still reads; a terminal
    /// cell holds a single glyph, so there a label must stop short of the next
    /// tick or lose it.
    pub cross_ticks: bool,
}

impl LabelMetrics {
    /// One cell per digit, one cell of gap: the terminal.
    #[must_use]
    pub fn cells(width: u32) -> Self {
        Self {
            width,
            gap: 1,
            digit: 1,
            padding: 0,
            separation: 0,
            cross_ticks: false,
        }
    }
}

/// One number as drawn: where it starts, how wide it is, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// Offset of the label's first unit from the ruler's origin.
    pub offset: u32,
    pub width: u32,
    pub text: String,
}

/// The numbers this ruler shows, in column order.
///
/// Every jump point is numbered, subdivisions included - the number is the
/// count that lands there, and a count is as useful mid-clip as at a cut. A
/// label is dropped where it would run into the number before it, or - where
/// the caller cannot draw a tick over a digit - where it would reach the next
/// tick. Either way a dense ruler thins out rather than smearing digits, and
/// the tick, never the number, is what survives.
#[must_use]
pub fn labels(view: &ViewState, numbers: Numbers, metrics: LabelMetrics) -> Vec<Label> {
    if numbers == Numbers::Off {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut clear_from = 0u32;
    for (i, tick) in view.ticks.iter().enumerate() {
        let text = match numbers {
            Numbers::Off => return Vec::new(),
            Numbers::Absolute => tick.frame.get().to_string(),
            Numbers::Relative => tick.relative.unsigned_abs().to_string(),
            Numbers::Both if tick.relative == 0 => tick.frame.get().to_string(),
            Numbers::Both => tick.relative.unsigned_abs().to_string(),
        };
        let width = text.chars().count() as u32 * metrics.digit + metrics.padding;
        let offset = tick.column.saturating_add(metrics.gap);
        let end = offset
            .saturating_add(width)
            .saturating_add(metrics.separation);
        if end > metrics.width || offset < clear_from {
            continue;
        }
        if !metrics.cross_ticks
            && let Some(next) = view.ticks.get(i + 1)
            && end > next.column
        {
            continue;
        }
        clear_from = end;
        out.push(Label {
            offset,
            width,
            text,
        });
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_core::Frame;

    /// A golden view with the ruler facts under test substituted in.
    fn view(ticks: &[(u32, i32)]) -> ViewState {
        let mut view = crate::fixtures::normal();
        view.ticks = ticks
            .iter()
            .map(|(column, relative)| crate::view::Tick {
                frame: Frame(u64::from(*column)),
                column: *column,
                major: true,
                relative: *relative,
            })
            .collect();
        view
    }

    fn texts(labels: &[Label]) -> Vec<(u32, &str)> {
        labels.iter().map(|l| (l.offset, l.text.as_str())).collect()
    }

    #[test]
    fn off_labels_nothing() {
        let v = view(&[(0, 0), (10, 1)]);
        assert!(labels(&v, Numbers::Off, LabelMetrics::cells(40)).is_empty());
    }

    #[test]
    fn relative_numbers_are_the_count_a_motion_needs_either_way() {
        let v = view(&[(0, -1), (10, 0), (20, 1), (30, 2)]);
        assert_eq!(
            texts(&labels(&v, Numbers::Relative, LabelMetrics::cells(40))),
            vec![(1, "1"), (11, "0"), (21, "1"), (31, "2")]
        );
    }

    #[test]
    fn absolute_numbers_are_the_frame_the_tick_sits_on() {
        let v = view(&[(0, 0), (10, 1)]);
        assert_eq!(
            texts(&labels(&v, Numbers::Absolute, LabelMetrics::cells(40))),
            vec![(1, "0"), (11, "10")]
        );
    }

    #[test]
    fn both_labels_the_playhead_absolutely_and_the_rest_relatively() {
        let v = view(&[(0, -1), (10, 0), (20, 1)]);
        assert_eq!(
            texts(&labels(&v, Numbers::Both, LabelMetrics::cells(40))),
            vec![(1, "1"), (11, "10"), (21, "1")]
        );
    }

    #[test]
    fn current_is_a_spelling_of_both() {
        assert_eq!(Numbers::parse("current"), Some(Numbers::Both));
        assert_eq!(Numbers::parse("both"), Some(Numbers::Both));
    }

    /// Two jump points a cell apart cannot both be labelled, and the ruler's
    /// last tick has nowhere to put its digits.
    #[test]
    fn a_label_that_would_reach_the_next_tick_or_the_edge_is_dropped() {
        let v = view(&[(0, 0), (1, 1), (2, 2)]);
        assert_eq!(
            texts(&labels(&v, Numbers::Relative, LabelMetrics::cells(40))),
            vec![(3, "2")],
            "only the last tick has room to its right"
        );
        let v = view(&[(39, 0)]);
        assert!(labels(&v, Numbers::Relative, LabelMetrics::cells(40)).is_empty());
    }

    /// The same ticks in a wider unit: a pixel ruler measures its digits in
    /// pixels, and may cross a tick because it draws the ticks over the
    /// numbers - so what it drops is only what would collide with the number
    /// before it.
    #[test]
    fn a_label_is_measured_in_the_callers_own_unit() {
        let v = view(&[(0, 0), (10, 1), (100, 2)]);
        let metrics = LabelMetrics {
            width: 400,
            gap: 2,
            digit: 6,
            padding: 4,
            separation: 6,
            cross_ticks: true,
        };
        assert_eq!(
            texts(&labels(&v, Numbers::Relative, metrics)),
            vec![(2, "0"), (102, "2")],
            "the number at column 10 would sit inside the one at column 0"
        );
        assert_eq!(
            texts(&labels(
                &v,
                Numbers::Relative,
                LabelMetrics {
                    cross_ticks: false,
                    ..metrics
                }
            )),
            vec![(12, "1"), (102, "2")],
            "a ruler that cannot draw over its digits drops the number at the \
             crowded tick, not the one after it"
        );
    }
}
