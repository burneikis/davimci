//! Running an external tool so a cancelled job stops now, not when ffmpeg
//! feels like it.
//!
//! `Command::output` blocks until the child exits. A job that only checks
//! its cancel flag between such calls cannot be cancelled at all, and since
//! closing a project joins its job threads, one four-minute transcode
//! becomes a four-minute quit. Everything here polls the child instead and
//! kills it the moment the flag is set.

use std::io::Read;
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
    let Some(ctx) = ctx else {
        return command.output().map(Some);
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    loop {
        if ctx.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        if let Some(status) = child.try_wait()? {
            return Ok(Some(Output {
                status,
                stdout: collect(stdout),
                stderr: collect(stderr),
            }));
        }
        std::thread::sleep(POLL);
    }
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

    #[test]
    fn an_uncancelled_run_returns_what_the_tool_printed() {
        let mut echo = Command::new("echo");
        echo.arg("davimci");
        let out = output(&mut echo, None).unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "davimci");
    }
}
