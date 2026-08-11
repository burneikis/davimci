//! Running an external tool so a cancelled job stops now, not when ffmpeg
//! feels like it.
//!
//! `Command::output` blocks until the child exits. A job that only checks
//! its cancel flag between such calls cannot be cancelled at all, and since
//! closing a project joins its job threads, one four-minute transcode
//! becomes a four-minute quit. Everything here polls the child instead and
//! kills it the moment the flag is set.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use crate::jobs::JobContext;

/// How often a running child is checked for exit and for cancellation.
const POLL: Duration = Duration::from_millis(25);

/// Run `command` to completion, killing it if `ctx` is cancelled.
///
/// Behaves like `Command::output` - both pipes are captured, and read on
/// threads of their own so a child that fills a pipe buffer cannot deadlock
/// against the poll loop. `Ok(None)` means the run was cancelled.
pub fn output(command: &mut Command, ctx: Option<&JobContext>) -> std::io::Result<Option<Output>> {
    output_with_progress(command, ctx, |_| {})
}

/// Run `command` like [`output`], reporting how far into the media ffmpeg has
/// got, in microseconds.
///
/// A decode or an encode is the whole of a job's runtime, so a job that only
/// reports before and after it sits at 0% until it is finished. ffmpeg says
/// where it is through `-progress`, which the caller must ask for; the lines
/// arrive interleaved with any diagnostics on stderr and are kept out of the
/// captured stderr so an error message stays readable.
pub fn output_with_progress(
    command: &mut Command,
    ctx: Option<&JobContext>,
    mut on_progress: impl FnMut(u64),
) -> std::io::Result<Option<Output>> {
    let Some(ctx) = ctx else {
        return command.output().map(Some);
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = drain(child.stdout.take());
    let mut stderr = lines(child.stderr.take());
    let mut errors = String::new();

    loop {
        if let Some(rx) = &stderr {
            for line in rx.try_iter() {
                absorb(&line, &mut errors, &mut on_progress);
            }
        }
        if ctx.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        if let Some(status) = child.try_wait()? {
            let out = collect(stdout);
            if let Some(rx) = stderr.take() {
                for line in rx {
                    absorb(&line, &mut errors, &mut on_progress);
                }
            }
            return Ok(Some(Output {
                status,
                stdout: out,
                stderr: errors.into_bytes(),
            }));
        }
        std::thread::sleep(POLL);
    }
}

/// Fold one stderr line into either progress or the error text.
fn absorb(line: &str, errors: &mut String, on_progress: &mut impl FnMut(u64)) {
    match progress_us(line) {
        Some(us) => on_progress(us),
        None if is_progress_line(line) => {}
        None => {
            errors.push_str(line);
            errors.push('\n');
        }
    }
}

/// The global ffmpeg options that make it report where it is. Passed by every
/// caller that wants progress, so the argument list stays testable as data.
#[must_use]
pub fn progress_args() -> [&'static str; 3] {
    ["-nostats", "-progress", "pipe:2"]
}

/// Microseconds of media done, from one `-progress` line.
///
/// Only `out_time_us` is read: ffmpeg reports the same number again as
/// `out_time_ms`, which is microseconds too despite the name, and taking both
/// would report every step twice.
fn progress_us(line: &str) -> Option<u64> {
    let (key, value) = line.trim().split_once('=')?;
    if key != "out_time_us" {
        return None;
    }
    value.trim().parse().ok()
}

/// Whether a line is one of ffmpeg's `-progress` fields, which are noise in an
/// error message.
fn is_progress_line(line: &str) -> bool {
    let Some((key, _)) = line.trim().split_once('=') else {
        return false;
    };
    matches!(
        key,
        "frame"
            | "fps"
            | "bitrate"
            | "total_size"
            | "out_time"
            | "out_time_ms"
            | "dup_frames"
            | "drop_frames"
            | "speed"
            | "progress"
    ) || key.starts_with("stream_")
}

/// Read a pipe line by line on a thread, so progress can be seen while the
/// child is still running.
fn lines<R: Read + Send + 'static>(pipe: Option<R>) -> Option<Receiver<String>> {
    let pipe = pipe?;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Some(rx)
}

/// Read a pipe to end on a thread, so neither pipe can fill while the other
/// is being read.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Option<Receiver<Vec<u8>>> {
    let mut pipe = pipe?;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    Some(rx)
}

fn collect(rx: Option<Receiver<Vec<u8>>>) -> Vec<u8> {
    rx.and_then(|rx| rx.recv().ok()).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test-only")]
mod tests {
    use super::*;
    use crate::jobs::JobRunner;
    use std::sync::mpsc::channel;
    use std::time::Instant;

    /// Regression: closing the editor while a transcode ran waited for
    /// ffmpeg, because the job blocked in `Command::output` and never saw
    /// its cancel flag. Cancelling must kill the child, not outlive it.
    #[test]
    fn cancelling_a_job_kills_the_child_it_is_waiting_on() {
        let (started, running) = channel();
        let mut runner = JobRunner::new();
        runner.spawn("sleep", move |ctx| {
            let mut sleep = Command::new("sleep");
            sleep.arg("30");
            let _ = started.send(());
            assert!(output(&mut sleep, Some(ctx)).unwrap().is_none());
            Ok(())
        });
        running.recv().unwrap();

        let began = Instant::now();
        runner.cancel_all();
        drop(runner);
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "cancel waited for the child: {:?}",
            began.elapsed()
        );
    }

    /// Regression: a decode reported 0% until it finished, because the job
    /// only reported either side of ffmpeg. `-progress` lines are progress,
    /// and must not end up in the error text either.
    #[test]
    fn progress_lines_report_and_are_kept_out_of_stderr() {
        assert_eq!(progress_us("out_time_us=1500000"), Some(1_500_000));
        // Reported twice by ffmpeg, in the same unit: counted once.
        assert_eq!(progress_us("out_time_ms=2000000"), None);
        assert!(is_progress_line("out_time_ms=2000000"));
        assert_eq!(progress_us("speed=1.2x"), None);
        assert!(is_progress_line("frame=12"));
        assert!(is_progress_line("progress=continue"));
        assert!(!is_progress_line("file.mkv: Invalid data found"));

        let mut seen = Vec::new();
        let mut errors = String::new();
        for line in ["frame=3", "out_time_us=250000", "boom: bad file"] {
            absorb(line, &mut errors, &mut |us| seen.push(us));
        }
        assert_eq!(seen, vec![250_000]);
        assert_eq!(errors, "boom: bad file\n");
    }

    #[test]
    fn an_uncancelled_run_returns_what_the_tool_printed() {
        let mut echo = Command::new("echo");
        echo.arg("davimci");
        let out = output(&mut echo, None).unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "davimci");
    }
}
