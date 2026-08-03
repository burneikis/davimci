//! MLT XML serialisation of a [`Projection`].
//!
//! Two jobs: it is what `melt`-compatible tooling and the golden projection
//! tests read, and it is the one place the graph's shape is written down in
//! full. It is a pure string function, so a ripple or compositing regression
//! shows up as an XML diff without rendering a single frame.

use std::fmt::Write as _;

use crate::projection::{Entry, FilterSpec, Projection, TrackProjection};

/// Serialise a projection as MLT XML.
#[must_use]
pub fn to_xml(p: &Projection) -> String {
    let mut out = String::new();
    // LC_NUMERIC is not decoration: MLT parses doubles with the C locale and
    // a comma-decimal locale would silently corrupt every rect and level.
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<mlt LC_NUMERIC=\"C\" version=\"7\" producer=\"tractor0\">\n");
    write_profile(&mut out, p);

    for (ti, track) in p.tracks.iter().enumerate() {
        for (ei, entry) in track.entries.iter().enumerate() {
            if let Entry::Clip(c) = entry {
                let _ = writeln!(
                    out,
                    "  <producer id=\"p{ti}_{ei}\" in=\"{}\" out=\"{}\">",
                    c.in_point.get(),
                    c.out_point.get()
                );
                prop(&mut out, "mlt_service", c.resource.service());
                prop(&mut out, "resource", &c.resource.resource());
                if let crate::projection::Resource::Text(t) = &c.resource {
                    prop(&mut out, "text", t);
                }
                if let crate::projection::Resource::Offline { path } = &c.resource {
                    prop(&mut out, "davimci.offline", path);
                }
                // One track per stream (spec §7): without these a
                // multi-stream file would decode its default stream on
                // every track.
                match c.stream {
                    Some(crate::projection::StreamSelect::Audio(s)) => {
                        prop(&mut out, "audio_index", &s.to_string());
                        prop(&mut out, "video_index", "-1");
                    }
                    Some(crate::projection::StreamSelect::Video(s)) => {
                        prop(&mut out, "video_index", &s.to_string());
                        prop(&mut out, "audio_index", "-1");
                    }
                    None => {}
                }
                prop(&mut out, "davimci.clip", &c.clip.to_string());
                prop(&mut out, "davimci.label", &c.label);
                for f in &c.filters {
                    write_filter(&mut out, f);
                }
                out.push_str("  </producer>\n");
            }
        }
    }

    for (ti, track) in p.tracks.iter().enumerate() {
        write_playlist(&mut out, ti, track);
    }

    out.push_str("  <tractor id=\"tractor0\">\n");
    for (ti, track) in p.tracks.iter().enumerate() {
        let _ = writeln!(
            out,
            "    <track producer=\"playlist{ti}\" hide=\"{}\"/>",
            track.hide()
        );
    }
    // Audio is summed by transitions, not by the multitrack: without these a
    // tractor plays one track's audio and drops the rest.
    for (n, b) in p.audio_mix_tracks().into_iter().enumerate() {
        let _ = writeln!(
            out,
            "    <transition id=\"mix{n}\">\n      \
             <property name=\"mlt_service\">mix</property>\n      \
             <property name=\"a_track\">0</property>\n      \
             <property name=\"b_track\">{b}</property>\n      \
             <property name=\"always_active\">1</property>\n      \
             <property name=\"sum\">1</property>\n    </transition>"
        );
    }
    out.push_str("  </tractor>\n");
    out.push_str("</mlt>\n");
    out
}

fn write_profile(out: &mut String, p: &Projection) {
    let res = p.props.resolution;
    let fps = p.props.fps;
    let _ = writeln!(
        out,
        "  <profile description=\"davimci\" width=\"{}\" height=\"{}\" progressive=\"1\" \
         sample_aspect_num=\"1\" sample_aspect_den=\"1\" display_aspect_num=\"{}\" \
         display_aspect_den=\"{}\" frame_rate_num=\"{}\" frame_rate_den=\"{}\" colorspace=\"709\"/>",
        res.width, res.height, res.width, res.height, fps.num, fps.den
    );
}

fn write_playlist(out: &mut String, ti: usize, track: &TrackProjection) {
    let _ = writeln!(
        out,
        "  <playlist id=\"playlist{ti}\" davimci.track=\"{}\">",
        escape(&track.name)
    );
    for (ei, entry) in track.entries.iter().enumerate() {
        match entry {
            Entry::Blank { length } => {
                let _ = writeln!(out, "    <blank length=\"{}\"/>", length.get());
            }
            Entry::Clip(c) => {
                let _ = writeln!(
                    out,
                    "    <entry producer=\"p{ti}_{ei}\" in=\"{}\" out=\"{}\"/>",
                    c.in_point.get(),
                    c.out_point.get()
                );
            }
        }
    }
    out.push_str("  </playlist>\n");
}

fn write_filter(out: &mut String, f: &FilterSpec) {
    out.push_str("    <filter>\n");
    let _ = writeln!(
        out,
        "      <property name=\"mlt_service\">{}</property>",
        escape(&f.service)
    );
    for (k, v) in &f.props {
        let _ = writeln!(
            out,
            "      <property name=\"{}\">{}</property>",
            escape(k),
            escape(v)
        );
    }
    out.push_str("    </filter>\n");
}

fn prop(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(
        out,
        "    <property name=\"{}\">{}</property>",
        escape(name),
        escape(value)
    );
}

/// XML-escape. Subtitle text is user data and reaches this function, so this
/// is a correctness boundary rather than a nicety.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_core::testing::{fixture, media_fixture};
    use davimci_core::{ClipProps, Frame};

    #[test]
    fn golden_two_track_timeline() {
        let tl = fixture(&[
            ("V1", &[(0, 100, "a"), (150, 50, "b")]),
            ("A1", &[(0, 200, "c")]),
        ]);
        insta::assert_snapshot!(to_xml(&Projection::of(&tl)));
    }

    #[test]
    fn golden_media_clip_with_filters() {
        let mut tl = media_fixture(&[(0, 100, 30, 500)]);
        let (track, clip) = (tl.tracks()[0].id, tl.tracks()[0].clips()[0].id);
        tl.set_clip_props(
            track,
            clip,
            ClipProps {
                gain_db: -3.0,
                fade_in: Frame(5),
                ..ClipProps::default()
            },
        )
        .unwrap();
        insta::assert_snapshot!(to_xml(&Projection::of(&tl)));
    }

    #[test]
    fn golden_routed_multi_audio_export_graph() {
        // The export shape M3 depends on: one stream per audio track, each
        // routed to its own channel pair before the mix sums them.
        let mut p = Projection::of(&davimci_core::testing::multi_audio_fixture(3, Some(1)));
        let layout = p.route_audio().expect("three audio tracks");
        assert_eq!(layout.total_channels, 6);
        insta::assert_snapshot!(to_xml(&p));
    }

    #[test]
    fn subtitle_text_is_escaped_not_injected() {
        let mut tl = fixture(&[("T1", &[])]);
        let track = tl.tracks()[0].id;
        let id = tl.new_clip_id();
        let mut clip = davimci_core::Clip::generated(id, "sub", Frame::ZERO, Frame(10));
        clip.text = Some("a < b & \"c\"".into());
        tl.restore(track, Frame::ZERO, &[clip], Frame(10), false)
            .unwrap();
        let xml = to_xml(&Projection::of(&tl));
        assert!(xml.contains("a &lt; b &amp; &quot;c&quot;"));
        assert!(!xml.contains("a < b &"));
    }
}
