//! The scripted-session format: keystrokes plus assertions (plan.md 2).
//!
//! One text file drives a real [`App`] over a real [`Host`] and states what
//! must be true afterwards, so the same artefact is both a test case and a
//! debugging tool - `davimci --script bug.dvs` reproduces a report without a
//! window, and the failure names the line of the script that was wrong.
//!
//! ```text
//! # a comment
//! keys 3lyy      feed a vim-style key string
//! cmd w out.dvp  submit a `:` line, without the colon
//! tick 4         four presentation ticks
//! expect mode normal
//! expect playhead 300
//! expect track V1
//! expect clips V1 3
//! expect timeline contains [b 100-200]
//! expect view contains -- NORMAL
//! expect message contains exported
//! dump timeline   record state in the report, assert nothing
//! ```
//!
//! Assertions read state and never mutate it, so a script's meaning does not
//! depend on whether assertions are checked.

use std::fmt::Write as _;

use davimci_app::{App, Event, Host};
use davimci_keys::Key;

/// A parsed session script.
#[derive(Debug, Clone, Default)]
pub struct Script {
    steps: Vec<Step>,
}

#[derive(Debug, Clone)]
struct Step {
    line: usize,
    kind: StepKind,
}

#[derive(Debug, Clone)]
enum StepKind {
    Keys(Vec<Key>),
    Command(String),
    Tick(u32),
    Expect(Assertion),
    Dump(Subject),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    Timeline,
    View,
}

#[derive(Debug, Clone)]
enum Assertion {
    Mode(String),
    Playhead(u64),
    Track(String),
    Clips { track: String, count: usize },
    Contains { subject: Subject, needle: String },
    Message(String),
}

/// A script that could not be parsed, with the line that broke it.
#[derive(Debug, Clone, thiserror::Error)]
#[error("line {line}: {problem}")]
pub struct ParseError {
    pub line: usize,
    pub problem: String,
}

/// One assertion that did not hold.
#[derive(Debug, Clone, thiserror::Error)]
#[error("line {line}: expected {expected}, found {found}")]
pub struct Failure {
    pub line: usize,
    pub expected: String,
    pub found: String,
}

/// What a run saw: every failure, and everything `dump` asked for.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub failures: Vec<Failure>,
    pub dumps: Vec<(usize, String)>,
}

impl Report {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// A human-readable summary, suitable for a terminal or a test panic.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut s = String::new();
        for (line, text) in &self.dumps {
            let _ = writeln!(s, "-- dump (line {line}) --\n{}", text.trim_end());
        }
        for f in &self.failures {
            let _ = writeln!(s, "{f}");
        }
        if self.failures.is_empty() {
            s.push_str("ok\n");
        }
        s
    }
}

impl Script {
    /// Parse a script. Unknown directives are errors, not silently skipped:
    /// a typo that quietly asserts nothing is worse than no test at all.
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        let mut steps = Vec::new();
        for (i, raw) in source.lines().enumerate() {
            let line = i + 1;
            let text = raw.trim();
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            let (word, rest) = split_word(text);
            let kind = match word {
                "keys" => StepKind::Keys(Key::parse_str(rest)),
                "cmd" => StepKind::Command(rest.to_string()),
                "tick" => StepKind::Tick(if rest.is_empty() {
                    1
                } else {
                    rest.parse().map_err(|_| err(line, "tick needs a count"))?
                }),
                "expect" => StepKind::Expect(parse_assertion(line, rest)?),
                "dump" => StepKind::Dump(subject(line, rest)?),
                other => return Err(err(line, format!("unknown directive `{other}`"))),
            };
            steps.push(Step { line, kind });
        }
        Ok(Self { steps })
    }

    /// Drive `app` through the script, collecting failures rather than
    /// stopping at the first: a run is a report, so one broken assertion does
    /// not hide the four after it.
    pub fn run(&self, app: &mut App, host: &mut dyn Host) -> Report {
        let mut report = Report::default();
        for step in &self.steps {
            match &step.kind {
                StepKind::Keys(keys) => {
                    for k in keys {
                        app.key(*k, host);
                    }
                }
                StepKind::Command(line) => {
                    app.event(Event::Command(line.clone()), host);
                }
                StepKind::Tick(n) => {
                    for _ in 0..*n {
                        app.event(Event::Tick, host);
                    }
                }
                StepKind::Dump(subject) => {
                    report.dumps.push((step.line, read(app, *subject)));
                }
                StepKind::Expect(assertion) => {
                    if let Some(f) = check(app, assertion, step.line) {
                        report.failures.push(f);
                    }
                }
            }
        }
        report
    }
}

fn check(app: &App, assertion: &Assertion, line: usize) -> Option<Failure> {
    let (expected, found, ok) = match assertion {
        Assertion::Mode(want) => {
            let got = app.mode().name();
            (format!("mode {want}"), format!("mode {got}"), got == want)
        }
        Assertion::Playhead(want) => {
            let got = app.session().timeline().playhead().frame.get();
            (
                format!("playhead {want}"),
                format!("playhead {got}"),
                got == *want,
            )
        }
        Assertion::Track(want) => {
            let tl = app.session().timeline();
            let got = tl
                .track(tl.playhead().track)
                .map_or("<none>", |t| t.name.as_str());
            (format!("track {want}"), format!("track {got}"), got == want)
        }
        Assertion::Clips { track, count } => {
            let tl = app.session().timeline();
            let got = tl.track_by_name(track).map(|t| t.clips().len());
            (
                format!("{count} clip(s) on {track}"),
                got.map_or_else(|| format!("no track {track}"), |n| n.to_string()),
                got == Some(*count),
            )
        }
        Assertion::Contains { subject, needle } => {
            let text = read(app, *subject);
            let ok = text.contains(needle.as_str());
            (format!("{subject:?} containing `{needle}`"), text, ok)
        }
        Assertion::Message(needle) => {
            let got = app
                .view()
                .message
                .map_or_else(|| "<no message>".to_string(), |m| m.text.clone());
            let ok = got.contains(needle.as_str());
            (format!("message containing `{needle}`"), got, ok)
        }
    };
    if ok {
        None
    } else {
        Some(Failure {
            line,
            expected,
            found,
        })
    }
}

fn read(app: &App, subject: Subject) -> String {
    match subject {
        Subject::Timeline => app.session().timeline().dump(),
        Subject::View => app.view().dump(),
    }
}

fn parse_assertion(line: usize, rest: &str) -> Result<Assertion, ParseError> {
    let (what, args) = split_word(rest);
    match what {
        "mode" => Ok(Assertion::Mode(args.to_string())),
        "playhead" => args
            .parse()
            .map(Assertion::Playhead)
            .map_err(|_| err(line, "playhead needs a frame number")),
        "track" => Ok(Assertion::Track(args.to_string())),
        "clips" => {
            let (track, count) = split_word(args);
            let count = count
                .parse()
                .map_err(|_| err(line, "clips needs a track and a count"))?;
            Ok(Assertion::Clips {
                track: track.to_string(),
                count,
            })
        }
        "message" => Ok(Assertion::Message(args.to_string())),
        "timeline" | "view" => {
            let (verb, needle) = split_word(args);
            if verb != "contains" {
                return Err(err(line, format!("expected `contains`, found `{verb}`")));
            }
            Ok(Assertion::Contains {
                subject: subject(line, what)?,
                needle: needle.to_string(),
            })
        }
        other => Err(err(line, format!("unknown assertion `{other}`"))),
    }
}

fn subject(line: usize, word: &str) -> Result<Subject, ParseError> {
    match word {
        "timeline" => Ok(Subject::Timeline),
        "view" => Ok(Subject::View),
        other => Err(err(line, format!("unknown subject `{other}`"))),
    }
}

fn split_word(text: &str) -> (&str, &str) {
    match text.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim_start()),
        None => (text, ""),
    }
}

fn err(line: usize, problem: impl Into<String>) -> ParseError {
    ParseError {
        line,
        problem: problem.into(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_app::{App, NullHost};
    use davimci_cmd::Session;
    use davimci_core::testing::fixture;

    fn app() -> App {
        App::new(Session::new(fixture(&[(
            "V1",
            &[(0, 100, "a"), (100, 100, "b")],
        )])))
    }

    #[test]
    fn a_passing_script_reports_nothing() {
        let script = Script::parse(
            "# move and check\n\
             keys ll\n\
             expect mode normal\n\
             expect clips V1 2\n\
             expect timeline contains [b 100-200]\n",
        )
        .unwrap();
        let report = script.run(&mut app(), &mut NullHost);
        assert!(report.passed(), "{}", report.summary());
    }

    #[test]
    fn a_failure_names_the_line_that_asserted_it() {
        let script = Script::parse("keys l\nexpect playhead 999\n").unwrap();
        let report = script.run(&mut app(), &mut NullHost);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].line, 2);
    }

    #[test]
    fn every_failure_is_reported_not_just_the_first() {
        let script = Script::parse("expect playhead 999\nexpect mode insert\n").unwrap();
        let report = script.run(&mut app(), &mut NullHost);
        assert_eq!(report.failures.len(), 2);
    }

    #[test]
    fn a_typo_is_an_error_rather_than_a_silently_skipped_line() {
        let e = Script::parse("keys l\nexpct mode normal\n").unwrap_err();
        assert_eq!(e.line, 2);
    }

    #[test]
    fn dump_records_state_without_asserting() {
        let script = Script::parse("dump timeline\n").unwrap();
        let report = script.run(&mut app(), &mut NullHost);
        assert!(report.passed());
        assert!(report.dumps[0].1.contains("V1:"));
    }
}
