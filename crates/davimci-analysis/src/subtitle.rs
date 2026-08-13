//! Subtitle streams.
//!
//! A subtitle stream becomes a `text` track whose clips carry the cue text.
//! Parsing is pure - SRT text in, cues out - so the import path can be tested
//! without ffmpeg; [`extract`] is the only part that shells out.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::AnalysisError;

/// One subtitle entry, in source time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Parse SRT. Malformed blocks are skipped rather than failing the import:
/// a broken cue must not cost the user the other 400 (recoverable).
#[must_use]
pub fn parse_srt(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let mut lines = block.trim().lines();
        let Some(first) = lines.next() else { continue };
        // The index line is optional in the wild; the timing line is not.
        let timing = if first.contains("-->") {
            first
        } else {
            match lines.next() {
                Some(l) if l.contains("-->") => l,
                _ => continue,
            }
        };
        let Some((a, b)) = timing.split_once("-->") else {
            continue;
        };
        let (Some(start_ms), Some(end_ms)) = (timestamp(a.trim()), timestamp(b.trim())) else {
            continue;
        };
        if end_ms <= start_ms {
            continue;
        }
        let body = lines.collect::<Vec<_>>().join("\n");
        if body.trim().is_empty() {
            continue;
        }
        cues.push(Cue {
            start_ms,
            end_ms,
            text: body.trim().to_string(),
        });
    }
    cues
}

/// Cues as SRT text, the inverse of [`parse_srt`].
#[must_use]
pub fn to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        let _ = write!(
            out,
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_timestamp(cue.start_ms),
            srt_timestamp(cue.end_ms),
            cue.text
        );
    }
    out
}

/// Milliseconds as `00:00:01,500`.
fn srt_timestamp(ms: u64) -> String {
    let (h, m, s, milli) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    format!("{h:02}:{m:02}:{s:02},{milli:03}")
}

/// `00:00:01,500` or `00:00:01.500` -> milliseconds.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and floored at zero before the conversion"
)]
fn timestamp(text: &str) -> Option<u64> {
    let text = text.split_whitespace().next()?.replace(',', ".");
    let mut parts = text.split(':');
    let h: u64 = parts.next()?.parse().ok()?;
    let m: u64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    // A cue timestamp is bounded by the length of the media it belongs to.
    let ms = (s * 1000.0).round().max(0.0);
    Some((h * 3600 + m * 60) * 1000 + ms as u64)
}

/// Pull subtitle stream `index` out of a container as SRT, via ffmpeg.
pub fn extract(path: &Path, index: u32) -> Result<Vec<Cue>, AnalysisError> {
    let name = path.display().to_string();
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", &format!("0:{index}"), "-f", "srt", "-"])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AnalysisError::ToolMissing {
                    tool: "ffmpeg",
                    what: "subtitle import",
                }
            } else {
                AnalysisError::io(&name, &e)
            }
        })?;
    if !out.status.success() {
        return Err(AnalysisError::AnalysisFailed {
            path: name,
            reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(parse_srt(&String::from_utf8_lossy(&out.stdout)))
}

/// Every subtitle stream in `info`, keyed by stream index, for
/// [`crate::ImportOptions::subtitles`].
///
/// A stream that will not extract costs its own cues and nothing else: the
/// import still lands, the track is still there, and the reason comes back
/// to be shown. Refusing the whole file because one subtitle stream is in a
/// codec ffmpeg cannot write as SRT would trade a video for a caption.
#[must_use]
pub fn extract_all(info: &crate::MediaInfo) -> (BTreeMap<u32, Vec<Cue>>, Vec<AnalysisError>) {
    let path = Path::new(&info.path);
    let mut cues = BTreeMap::new();
    let mut problems = Vec::new();
    for stream in &info.streams {
        if stream.kind != crate::StreamKind::Subtitle {
            continue;
        }
        match extract(path, stream.index) {
            Ok(c) => {
                cues.insert(stream.index, c);
            }
            Err(e) => problems.push(e),
        }
    }
    (cues, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1\n00:00:01,000 --> 00:00:03,000\nsubtitle track 1\n\n\
                          2\n00:00:04,500 --> 00:00:06,250\ntwo\nlines\n";

    #[test]
    fn cues_carry_their_text_and_exact_timing() {
        let cues = parse_srt(SAMPLE);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].end_ms, 3000);
        assert_eq!(cues[0].text, "subtitle track 1");
        assert_eq!(cues[1].start_ms, 4500);
        assert_eq!(cues[1].end_ms, 6250);
        assert_eq!(cues[1].text, "two\nlines");
    }

    #[test]
    fn broken_cues_are_skipped_not_fatal() {
        let text = format!(
            "{SAMPLE}\n9\nnot a timing line\nbody\n\n\
                            10\n00:00:09,000 --> 00:00:08,000\nbackwards\n\n\
                            11\n00:00:10,000 --> 00:00:11,000\n\n"
        );
        let cues = parse_srt(&text);
        assert_eq!(cues.len(), 2, "only the two good cues survive");
    }

    #[test]
    fn windows_line_endings_and_dot_separators_parse() {
        let cues = parse_srt("1\r\n00:00:01.000 --> 00:00:02.000\r\nhi\r\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "hi");
    }

    #[test]
    fn junk_never_panics() {
        for junk in ["", "-->", "\n\n\n", "1\n-->\n", "99:99:99,999 --> x"] {
            let _ = parse_srt(junk);
        }
    }
}
