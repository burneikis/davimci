# AGENTS.md - davimci

## Source of truth

The code is the behaviour; there is no separate spec. `docs/keymap.md` is
generated from the code and describes the default keymap. `todo.md` holds
outstanding work: a `# Human` section, an `# AI` section, and deferred items
not targeted for v1. Delete an item from `todo.md` when it lands; do not keep
history there.

## Architectural rules

These are load-bearing; breaking one is a design defect rather than a style
issue.

Nothing outside `davimci-mlt` may reference MLT types - the backend sits behind
the `RenderBackend` trait so it can be replaced. `davimci-core` and `davimci-cmd`
have no backend and no I/O, and must stay testable with no window, GPU, or
media. No frontend contains view logic; if a change has to land in both
`davimci-gui` and `davimci-tui`, it belongs in `davimci-app` or `davimci-present`.

All mutation goes through a `Command` whose `apply` returns the command that
undoes it. There is no other write path to the timeline - undo, `.`-repeat, macros, the Lua API, and the
project format all depend on it. Commands validate before they mutate, so a
rejected command leaves the timeline byte-identical and never enters the undo
log.

A timeline has one framerate and one resolution; sources are conformed on
import. Time is `Frame(u64)`, with no floats in the model.

`libmlt` is linked dynamically and `melt`/`melted` are never vendored, since
davimci is GPL-3.0 over LGPL-2.1 MLT.

## Comments and docs

Name things so the code reads without commentary; comment the reasons the code
cannot state. A module doc says what the module is for and which rule it
enforces, in a few lines. Do not narrate implementation history, restate the
signature below, list what is unfinished, or track phase numbers - that is what
`todo.md` is for. Do not cite `spec.md`, `plan.md` or `changes.md`; they no
longer exist.

A comment that is now false is a bug. Delete stale comments rather than
appending corrections to them.

## Errors

Every error is classified and carries a complete user-facing sentence; raw
`Debug` output must never reach the status line. User errors are rejected
before mutating. Offline media keeps the project editable but blocks export.
Recoverable errors degrade locally and keep editing alive. Corruption flushes
autosave and exits.

`unwrap`, `expect`, and `panic` are denied in library crates; the only
sanctioned panic is `assert_invariant!`. Libraries use typed `thiserror`
errors, `anyhow` only at the binary edge.

## Testing

Every behavioural change ships with a test, and every bug fix ships with a
regression test naming the issue.

```sh
just test   # fast suite - keep it free of decode/encode
just lint   # clippy -D warnings + fmt --check
```

Run both before declaring work done; do not report success without them.

Never loosen a tolerance or delete an assertion to make a test pass. Diagnose
first - a failing test is usually right, and when it is wrong, correct its
expectation and explain why.

Test media is generated, never committed. New fixtures go in
`scripts/gen-fixtures.sh` and need a property that can be asserted exactly,
such as a known silence span, cut frame, or stream count. Anything needing real
media goes behind `--features slow-tests`.

Match technique to layer: property tests and invariants for the model,
apply/invert round-trips for commands, table-driven landing positions for
motions, golden key-string tests for the parser, snapshot tests for rendering,
`ffprobe` assertions for export. The cross-frontend parity test is an enforcer,
not a formality: one scripted session must give identical results through
headless, GUI, and TUI, and a failure there is a frontend bug, never a core
one.
