//! The only place analysis touches real media.
//!
//! Decoding is done by piping raw samples out of ffmpeg rather than linking a
//! second demux stack: MLT (Phase 6) owns playback, and analysis just needs
//! numbers. Everything above this module works on `&[f32]`, which is why the
//! measurement code is testable with no media at all.

use std::path::Path;
use std::process::Command;

use crate::error::AnalysisError;
use crate::jobs::Phase;

/// Decode an audio stream to mono `f32` at `sample_rate`.
///
/// Downmixed on purpose: analysis answers "is there sound here", which is a
/// property of the take rather than of the channel layout. Cancelling the
/// job kills ffmpeg rather than waiting out the file.
pub fn decode_mono(
    path: &Path,
    stream: u32,
    sample_rate: u32,
    phase: Phase<'_>,
) -> Result<Vec<f32>, AnalysisError> {
    let name = path.display().to_string();
    let total_us = crate::probe::duration_us(path);
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error"])
        .args(crate::run::progress_args())
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            &format!("0:{stream}"),
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
            "-",
        ]);
    let out = crate::run::output_with_progress(&mut command, phase.ctx(), |us| {
        phase.report(us, total_us.unwrap_or(0));
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AnalysisError::ToolMissing {
                tool: "ffmpeg",
                what: "audio analysis",
            }
        } else {
            AnalysisError::io(&name, &e)
        }
    })?
    .ok_or(AnalysisError::Cancelled)?;
    if !out.status.success() {
        return Err(AnalysisError::AnalysisFailed {
            path: name,
            reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(from_f32le(&out.stdout))
}

/// Little-endian `f32` bytes to samples. A trailing partial sample is
/// dropped rather than misread.
#[must_use]
pub fn from_f32le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// ffmpeg's `scdet` threshold, as a percentage change between frames.
/// 10 is the filter's own default and is what davimci uses.
pub const SCENE_THRESHOLD: f32 = 10.0;

/// Scene-change points in milliseconds, via ffmpeg's `scdet` filter.
///
/// Optional: a failure here degrades to "no scene changes"
/// rather than failing the import.
pub fn scene_changes(
    path: &Path,
    threshold: f32,
    phase: Phase<'_>,
) -> Result<Vec<u64>, AnalysisError> {
    let name = path.display().to_string();
    let total_us = crate::probe::duration_us(path);
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "info"])
        .args(crate::run::progress_args())
        .arg("-i")
        .arg(path)
        .args([
            "-vf",
            &format!("scdet=threshold={threshold}"),
            "-f",
            "null",
            "-",
        ]);
    let out = crate::run::output_with_progress(&mut command, phase.ctx(), |us| {
        phase.report(us, total_us.unwrap_or(0));
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AnalysisError::ToolMissing {
                tool: "ffmpeg",
                what: "scene detection",
            }
        } else {
            AnalysisError::io(&name, &e)
        }
    })?
    .ok_or(AnalysisError::Cancelled)?;
    Ok(parse_scdet(&String::from_utf8_lossy(&out.stderr)))
}

/// Parse `scdet` log lines, which look like:
///
/// ```text
/// [Parsed_scdet_0 @ 0x1] lavfi.scd.score: 15.625, lavfi.scd.time: 2
/// ```
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and floored at zero before the conversion"
)]
pub fn parse_scdet(log: &str) -> Vec<u64> {
    let mut out: Vec<u64> = log
        .lines()
        .filter_map(|l| {
            let (_, rest) = l.split_once("lavfi.scd.time:")?;
            let field = rest.split(',').next()?.trim();
            let secs: f64 = field.trim_end_matches('s').trim().parse().ok()?;
            let ms = (secs * 1000.0).round().max(0.0);
            // A timestamp ffmpeg reports is seconds into a file, so this is
            // never large enough to lose anything.
            Some(ms as u64)
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_samples_decode_from_little_endian_bytes() {
        let mut bytes = Vec::new();
        for v in [0.0f32, 0.5, -1.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        // A truncated tail must be ignored, not misread.
        bytes.push(0x7f);
        assert_eq!(from_f32le(&bytes), vec![0.0, 0.5, -1.0]);
        assert!(from_f32le(&[]).is_empty());
    }

    #[test]
    fn scdet_output_parses_into_sorted_milliseconds() {
        // Real ffmpeg output, including the duplicate a two-pass filter emits.
        let log = "\
[Parsed_scdet_0 @ 0x1] lavfi.scd.score: 60.000, lavfi.scd.time: 4
[Parsed_scdet_0 @ 0x1] lavfi.scd.score: 15.625, lavfi.scd.time: 2.000
frame= 240 fps=0.0 q=-0.0 size=N/A time=00:00:04.00
[Parsed_scdet_0 @ 0x1] lavfi.scd.score: 15.625, lavfi.scd.time: 2";
        assert_eq!(parse_scdet(log), vec![2000, 4000]);
        assert!(parse_scdet("no scenes here").is_empty());
    }
}
