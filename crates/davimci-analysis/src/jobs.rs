//! The background job runner.
//!
//! Analysis and proxy generation run behind the editor: importing a file must
//! not block the first keystroke. Jobs report progress for the status line
//! and are cancellable, and closing a project cancels everything it started.
//!
//! Jobs communicate only by message. Nothing here touches a `Timeline`, so a
//! job cannot race an edit - the frontend drains [`JobRunner::poll`] on its
//! own thread and applies the result there.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use crate::error::AnalysisError;

/// Identifies a job within one runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);

/// What a job tells the editor.
#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    Started {
        job: JobId,
        label: String,
    },
    /// Progress for the status line. `total == 0` means indeterminate.
    Progress {
        job: JobId,
        done: u64,
        total: u64,
    },
    Finished {
        job: JobId,
    },
    Failed {
        job: JobId,
        error: AnalysisError,
    },
    Cancelled {
        job: JobId,
    },
}

impl JobEvent {
    #[must_use]
    pub fn job(&self) -> JobId {
        match self {
            Self::Started { job, .. }
            | Self::Progress { job, .. }
            | Self::Finished { job }
            | Self::Failed { job, .. }
            | Self::Cancelled { job } => *job,
        }
    }

    /// Whether this is the last event a job will send.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Finished { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// The handle a running job uses to report in and to check for cancellation.
#[derive(Debug, Clone)]
pub struct JobContext {
    id: JobId,
    cancel: Arc<AtomicBool>,
    events: Sender<JobEvent>,
}

impl JobContext {
    #[must_use]
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Long-running work must check this; a cancelled job is expected to
    /// return [`AnalysisError::Cancelled`] promptly.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Bail out with [`AnalysisError::Cancelled`] if cancellation is pending.
    pub fn check(&self) -> Result<(), AnalysisError> {
        if self.is_cancelled() {
            return Err(AnalysisError::Cancelled);
        }
        Ok(())
    }

    pub fn progress(&self, done: u64, total: u64) {
        // A dropped receiver means the editor is gone; the job will notice
        // through its cancel flag. Nothing to report to.
        let _ = self.events.send(JobEvent::Progress {
            job: self.id,
            done,
            total,
        });
    }
}

/// Spawns jobs and collects their events.
#[derive(Debug)]
pub struct JobRunner {
    next: u64,
    tx: Sender<JobEvent>,
    rx: Receiver<JobEvent>,
    running: Vec<(JobId, Arc<AtomicBool>, Option<JoinHandle<()>>)>,
}

impl Default for JobRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl JobRunner {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            next: 1,
            tx,
            rx,
            running: Vec::new(),
        }
    }

    /// Start a job. The closure runs on its own thread and must return
    /// promptly once [`JobContext::is_cancelled`] is true.
    pub fn spawn<F>(&mut self, label: impl Into<String>, work: F) -> JobId
    where
        F: FnOnce(&JobContext) -> Result<(), AnalysisError> + Send + 'static,
    {
        let id = JobId(self.next);
        self.next += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let ctx = JobContext {
            id,
            cancel: Arc::clone(&cancel),
            events: self.tx.clone(),
        };
        let label = label.into();
        let _ = self.tx.send(JobEvent::Started { job: id, label });
        let handle = std::thread::spawn(move || {
            let outcome = work(&ctx);
            let event = match outcome {
                Ok(()) if ctx.is_cancelled() => JobEvent::Cancelled { job: id },
                Ok(()) => JobEvent::Finished { job: id },
                Err(AnalysisError::Cancelled) => JobEvent::Cancelled { job: id },
                // Phase 0: a failed analysis job degrades locally. It reports
                // and dies; the editor keeps running with that track pending.
                Err(error) => JobEvent::Failed { job: id, error },
            };
            let _ = ctx.events.send(event);
        });
        self.running.push((id, cancel, Some(handle)));
        id
    }

    /// Drain every event received so far. Never blocks.
    pub fn poll(&mut self) -> Vec<JobEvent> {
        let events: Vec<JobEvent> = self.rx.try_iter().collect();
        for e in &events {
            if e.is_terminal() {
                self.reap(e.job());
            }
        }
        events
    }

    /// Block until every job has finished, returning their remaining events.
    /// Used by tests and by an orderly shutdown.
    pub fn join(&mut self) -> Vec<JobEvent> {
        for (_, _, handle) in &mut self.running {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
        self.poll()
    }

    /// Ask a job to stop. It is not stopped until it says so.
    pub fn cancel(&self, job: JobId) {
        for (id, flag, _) in &self.running {
            if *id == job {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Cancel everything - what closing a project does.
    pub fn cancel_all(&self) {
        for (_, flag, _) in &self.running {
            flag.store(true, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn active(&self) -> usize {
        self.running.len()
    }

    fn reap(&mut self, job: JobId) {
        if let Some(pos) = self.running.iter().position(|(id, _, _)| *id == job) {
            let (_, _, handle) = self.running.remove(pos);
            if let Some(h) = handle {
                let _ = h.join();
            }
        }
    }
}

impl Drop for JobRunner {
    /// Closing a project cancels its jobs and waits for them, so no thread
    /// outlives the timeline it was analysing.
    fn drop(&mut self) {
        self.cancel_all();
        for (_, _, handle) in &mut self.running {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    #[test]
    fn a_job_reports_start_progress_and_completion_in_order() {
        let mut runner = JobRunner::new();
        runner.spawn("analyse tone.wav", |ctx| {
            for i in 0..5 {
                ctx.progress(i, 5);
            }
            Ok(())
        });
        let events = runner.join();
        assert!(matches!(events.first(), Some(JobEvent::Started { .. })));
        assert!(events.last().unwrap().is_terminal());
        let progress: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                JobEvent::Progress { done, .. } => Some(*done),
                _ => None,
            })
            .collect();
        assert_eq!(progress, vec![0, 1, 2, 3, 4]);
        assert!(matches!(events.last(), Some(JobEvent::Finished { .. })));
        assert_eq!(runner.active(), 0, "a finished job is reaped");
    }

    #[test]
    fn a_failing_job_reports_and_leaves_the_runner_usable() {
        let mut runner = JobRunner::new();
        runner.spawn("analyse broken.mkv", |_| {
            Err(AnalysisError::AnalysisFailed {
                path: "/broken.mkv".into(),
                reason: "decode error".into(),
            })
        });
        let events = runner.join();
        assert!(matches!(events.last(), Some(JobEvent::Failed { .. })));

        runner.spawn("analyse fine.wav", |_| Ok(()));
        assert!(matches!(
            runner.join().last(),
            Some(JobEvent::Finished { .. })
        ));
    }

    #[test]
    fn cancellation_stops_a_long_job_and_reports_it_as_cancelled() {
        let mut runner = JobRunner::new();
        let ticks = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&ticks);
        let job = runner.spawn("proxy 4k.mkv", move |ctx| {
            for i in 0..100_000u64 {
                ctx.check()?;
                counter.store(i, Ordering::Relaxed);
                std::thread::sleep(Duration::from_micros(50));
            }
            Ok(())
        });
        // Let it get going, then pull the plug.
        while ticks.load(Ordering::Relaxed) < 2 {
            std::thread::yield_now();
        }
        runner.cancel(job);
        let events = runner.join();
        assert!(matches!(events.last(), Some(JobEvent::Cancelled { .. })));
        assert!(
            ticks.load(Ordering::Relaxed) < 100_000,
            "the job ran to completion instead of stopping"
        );
    }

    #[test]
    fn closing_a_project_cancels_everything_it_started() {
        let done = Arc::new(AtomicBool::new(false));
        {
            let mut runner = JobRunner::new();
            let flag = Arc::clone(&done);
            runner.spawn("analyse", move |ctx| {
                while !ctx.is_cancelled() {
                    std::thread::sleep(Duration::from_micros(100));
                }
                flag.store(true, Ordering::Relaxed);
                Err(AnalysisError::Cancelled)
            });
            // Dropping the runner is what closing a project does.
        }
        assert!(
            done.load(Ordering::Relaxed),
            "the job was never asked to stop"
        );
    }
}
