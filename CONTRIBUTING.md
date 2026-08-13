# Contributing

## Before you open a PR

```sh
just test   # fast suite
just lint   # clippy -D warnings + fmt --check
```

Both must pass. Every behavioural change ships with a test; every bug fix ships
with a regression test.

## PR structure

Keep it brief. Two sections:

**Summary** - what changed and why, in a few lines.

**Test Plan** - the automated tests you added or ran, plus the manual steps you
took. Include screenshots or a short recording for anything visible in the GUI
or TUI.

If you used an LLM or a coding agent, please include a human-written foreword
saying what you asked for and what you checked yourself.

## Changelog

User-visible changes get a line in [`CHANGELOG.md`](CHANGELOG.md) under
`Unreleased`. See the guidelines at the top of that file.
