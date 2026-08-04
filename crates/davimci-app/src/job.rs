//! Background job progress shown in the status line.
//!
//! The app does not run jobs; analysis (Phase 5) and export (Phase 8b) do.
//! This is the view of them, so every frontend reports progress the same way.

/// What a running job is doing. The label is user-facing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub label: String,
    /// Completion in tenths of a percent, so progress stays integral and two
    /// frontends cannot round differently.
    pub permille: u16,
    pub state: JobState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    Cancelled,
    Failed,
}

impl Job {
    #[must_use]
    pub fn new(id: u64, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            permille: 0,
            state: JobState::Running,
        }
    }

    /// Progress as a percentage, truncated - never shows 100% before done.
    #[must_use]
    pub fn percent(&self) -> u16 {
        self.permille / 10
    }
}

/// A job event from whoever is actually running the work.
///
/// The host runs jobs and the app displays them, so this is the only thing
/// that crosses between them. It is deliberately not a `Job`: the host does
/// not get to decide how a job is labelled once it has started, or to
/// resurrect one the app already retired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobUpdate {
    Started { id: u64, label: String },
    Progress { id: u64, permille: u16 },
    Finished { id: u64, state: JobState },
}

/// Every job the user should know about, newest last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobList {
    jobs: Vec<Job>,
}

impl JobList {
    pub fn start(&mut self, id: u64, label: impl Into<String>) {
        self.jobs.retain(|j| j.id != id);
        self.jobs.push(Job::new(id, label));
    }

    /// Update progress. Unknown ids are ignored rather than panicking: a job
    /// may have been cleared while its worker was still reporting.
    pub fn progress(&mut self, id: u64, permille: u16) {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
            j.permille = permille.min(1000);
        }
    }

    pub fn finish(&mut self, id: u64, state: JobState) {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
            j.state = state;
            if state == JobState::Done {
                j.permille = 1000;
            }
        }
    }

    /// Drop everything that is no longer running - called on project close.
    /// Fold in an update from the host.
    pub fn apply(&mut self, update: JobUpdate) {
        match update {
            JobUpdate::Started { id, label } => self.start(id, label),
            JobUpdate::Progress { id, permille } => self.progress(id, permille),
            JobUpdate::Finished { id, state } => self.finish(id, state),
        }
    }

    pub fn clear_finished(&mut self) {
        self.jobs.retain(|j| j.state == JobState::Running);
    }

    pub fn running(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|j| j.state == JobState::Running)
    }

    #[must_use]
    pub fn all(&self) -> &[Job] {
        &self.jobs
    }

    /// The job the status line shows: the first one still running.
    #[must_use]
    pub fn foreground(&self) -> Option<&Job> {
        self.running().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_clamped_and_percent_truncates() {
        let mut jobs = JobList::default();
        jobs.start(1, "Analysing bunny.mkv");
        jobs.progress(1, 5_000);
        assert_eq!(jobs.foreground().map(Job::percent), Some(100));
        jobs.progress(1, 999);
        assert_eq!(jobs.foreground().map(Job::percent), Some(99));
    }

    #[test]
    fn progress_for_an_unknown_job_is_ignored() {
        let mut jobs = JobList::default();
        jobs.progress(7, 100);
        assert!(jobs.all().is_empty());
    }

    #[test]
    fn finished_jobs_leave_the_foreground_and_can_be_cleared() {
        let mut jobs = JobList::default();
        jobs.start(1, "a");
        jobs.start(2, "b");
        jobs.finish(1, JobState::Done);
        assert_eq!(jobs.foreground().map(|j| j.id), Some(2));
        jobs.clear_finished();
        assert_eq!(jobs.all().len(), 1);
    }
}
