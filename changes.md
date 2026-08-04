# davimci - implementation history

What was built, and where the build departed from `plan.md` or `spec.md` and
why. `spec.md` says how davimci behaves; `plan.md` says what is left to do.
Neither carries this record, so corrections that mattered are not lost when a
phase is finished and struck from the plan.

Ordered by the phase that produced them, not by date.

---

## Phase 2 - Command Layer & Undo Tree (`davimci-cmd`)

Status: complete. Macros store opaque input tokens rather than commands, so
replay is keystroke-shaped as in vim; `davimci-keys` (Phase 4) gives the tokens
meaning. Undo history was not persisted at this point; project format v2
(Phase 8) added it.

---

## Phase 3 - Motions, Jump Points, Text Objects (`davimci-motion`)

Status: complete. Motions and objects are pure queries - they resolve a target
and never mutate, so a verb can validate before building a command. The
jump-point set is memoised behind a fingerprint of everything it reads, so a
stale hit is not representable. Predicate motions go through the
`PredicateIndex` trait and report `Pending` until Phase 5 implements it;
`ac` resolves to the same range as `ic` until a cut carries a transition, and
widens to cover the overlap once one does (spec 4.1, Phase 9f).

---

## Phase 4 - Key Parser & Mode FSM (`davimci-keys`)

Status: complete. The grammar (`davimci-keys::parser`) is a pure state machine
over `davimci-motion`'s `BuiltinMotion`/`TextObject` types and never touches a
`Timeline`, so golden key-string tests need no fixture. A separate `engine`
module gives the parsed `Action` meaning against a live `davimci_cmd::Session`,
which is the layer plan.md Phase 2 deferred ("`davimci-keys` gives the tokens
meaning"). Playhead motion and marks are intentionally outside the undo log -
`Session` gained narrow `set_playhead`/`set_mark` escape hatches for this,
since navigation was never meant to be a `Command`.

Open at the end of this phase: `i`/`a`/`r` need the Phase 5 media picker;
`<`/`>` jump-point edge trims parse but are not wired to a command yet;
visual-mode text-object narrowing (typing `it`/`at` while a selection is live,
spec 6) is not implemented - operators in a `VISUAL*` mode act on the whole
selection instead.

---

## Phase 5 - Media Import, Conform & Analysis (`davimci-analysis`)

Status: complete. The crate is split so that everything expressible as a pure
function of data is one - ffprobe JSON parsing, the fit rectangle, the conform
matrix, SRT parsing, silence detection, the predicate index, the proxy
threshold rule - and only `probe::FfprobeProber`, `decode`, `proxy::generate`
and `cache` touch the outside world. That is what keeps the default suite free
of decode/encode; the fixture-backed tests live in `tests/media.rs` behind
`--features slow-tests`.

Amendments made during implementation:
- Import had to become a real edit, so Phase 2's command set gained `AddTrack`
  and `RemoveTrack` and Phase 1 gained `add_track_with_id`/`remove_track`. An
  import is one `Sequence`; undoing it removes the tracks it created.
- A command cannot mint an id its own siblings need, so ids are pinned before
  the sequence is built (`Timeline::reserve_ids`, `Session::reserve_ids`).
- Re-conform is *not* self-inverse: rounding at one rate is not reversible at
  another, and a clip that rounds to zero frames has to be repaired rather
  than lost. `Reconform` therefore inverts to `RestoreConform`, which replays
  the exact prior geometry (`davimci_core::conform`). Undo of a rate change is
  byte-identical.
- Predicate queries index a threshold chosen at *query* time, which a sorted
  list cannot answer in log time. `index::MaxTree` is a max segment tree with
  a directional descent, so `]a` is O(log n) for any threshold and never
  scans.
- Spec 10.3's `prores_proxy` was not an ffmpeg encoder name; the slow tests
  caught it. The default is now `prores_ks` at profile 0, and spec.md says so.

Open at the end of this phase: nothing calls this from a frontend, because
there is no frontend. `Predicate::Tagged` matches nothing until clip tags
exist (Phase 7), and analysis measures the source rather than the post-gain
signal, so the `invalidate`/`:analyze` path in Phase 9e is a hook without a
caller.

---

## Phase 6 - Render Backend (`davimci-backend`, `davimci-mlt-sys`, `davimci-mlt`)

Status: complete. The crate is layered by testability: `projection` turns a
`Timeline` into the shape the graph must have (pure data, no MLT), `xml`
serialises that shape for the golden tests, `patch` diffs two projections, and
only `ffi`/`backend` touch the C API. `MockBackend` lives in `davimci-backend`
and decodes nothing: a mock frame's colour is a pure function of its position,
so an upstream test asserts *which* frame it got from four bytes.

Amendments made during implementation:
- The projection reads track mute/solo and the media offline flag, and nothing
  could set them, so Phase 1 gained `set_track_muted`, `set_track_solo`, and
  `set_media_offline`. Solo turned out to need a defined meaning; spec 6.1
  now says it is exclusive by effect, so any solo silences every non-solo
  track and the backend resolves it at projection time.
- Pulling frames directly from a tractor bypasses the consumer that normally
  plants MLT's normalising filters, so `mlt_frame_get_image` returned native
  YUV at native size and ignored the requested dimensions - preview scaling
  would have been a lie. Producers now carry `avcolor_space`/`rescale`/`resize`
  themselves, and `FrameRef::rgba` verifies the *returned* format instead of
  trusting the requested one. Trusting it read past the end of a smaller YUV
  buffer; the slow tests caught it as a segfault.
- `mlt_events_listen` hands back an event the properties bag still owns, so the
  listener handle takes `mlt_event_inc_ref` before it can be closed. Without it
  stopping a preview double-freed.
- Refcount testing is the wrapper unit tests (`clone_ref` is balanced by drop,
  64 create/clone/drop cycles do not grow the count, a playlist planted in a
  tractor outlives its wrapper) - they assert on MLT's own `ref_count()`
  directly rather than relying on a sanitizer. `just sanitize` now runs
  (nightly + `rust-src` installed via `rustup`) and is green with a narrow,
  documented LSAN suppression file
  (`crates/davimci-mlt/lsan-suppressions.txt`) for MLT's own one-time
  module-init state and its internal blank-producer path, neither of which
  davimci constructs or holds a handle to. LeakSanitizer's stack scan is
  conservative and can miss a real leak, so a clean run is evidence for the
  wrapper, not proof; the `ref_count()` assertions remain the primary
  guarantee.

Open at the end of this phase: nothing calls this from a frontend, because
there is no frontend. Transitions arrive in Phase 9f, so the projection plants
only the audio `mix` transitions here, and the export preset registry that
would exercise `RenderSettings` properly arrives in Phase 8b.

---

## Phase 7 - Lua Config & Plugin API (`davimci-lua`)

Status: complete. The crate's shape follows one rule - **Lua asks, it never
writes.** A `davimci.*` call either registers something or appends a `Request`;
the host runs each request through `davimci_cmd::Session`, so a plugin edit is
an ordinary undo-tree entry and the single-write-path rule holds at the
plugin boundary. That is also what keeps the crate testable: `Runtime` needs
no timeline, no backend, and no window, and the spec 9 snippets run verbatim
as the acceptance suite.

Amendments made during implementation:
- A Lua function right-hand side (spec 9.2) had nothing to resolve to, since
  `davimci-keys` must not depend on `davimci-lua`. `Action::Plugin(u32)` and
  `Outcome::Plugin(u32)` carry an opaque callback id instead: the engine
  reports it, the host invokes it, and `Engine::execute_action` (new, public)
  runs whatever edits come back. spec 9.9 now documents the request model
  and the `editor.*` command set that a string right-hand side may name.
- A registered motion cannot be handed a live `Timeline` without becoming a
  second write path, so it receives a `MotionEnv` snapshot - playhead,
  focused track, clip bounds, analysis samples. `find_next` over an
  unanalysed track reports `Pending` rather than `NoMatch`, matching
  `davimci_motion::Answer`; a Lua motion cannot accidentally be more confident
  than the analysis it queries.
- Cancellation needed defining, not just implementing: a `BeforeExport`
  handler refuses by returning `false` *or* by throwing. Throwing also
  disables the handler (Phase 0 recoverable policy); a `false` return is a
  deliberate veto and leaves it in place. spec 9.8 says so now.
- Trust is not a binary: spec 9.7 said "opt-in" without saying what a
  trusted file may do. An untrusted `.davimci.lua` is never read, and a trusted
  one still runs sandboxed (no `os`, `io`, `load`, `dofile`, and a `require`
  that resolves `davimci.*` only). spec 9.7 now spells this out.
- Export presets validate at definition, and codec names map to ffmpeg
  encoders here rather than in the preset, so section 10.3's "never a marketing
  name" rule cannot be broken by a config.

Open at the end of this phase: no frontend calls any of this, so
`Runtime::take_requests` has no production caller; `Request::Import`/`Analyze`
wait on Phase 8/9e, and text objects registered from Lua are resolvable but
not yet reachable from the key grammar, which still resolves only the built-in
`ic`/`ac`/`it`/`at`/ `is`. Keymap overrides are applied for `NORMAL` only,
because `davimci_keys::Keymap` is a single table for every mode.

---

## Phase 8 - Project Lifecycle (`davimci-cli`)

Status: complete. `davimci-cli` is the first crate allowed to touch the
filesystem, and it is split so that only the parts that must: `excmd::parse`
is a pure function from a `:` line to an `ExCommand` and is table-tested
against the spec 12 vocabulary, while `Workspace` is the only thing that
reads or writes. Every edit it performs is still an `EditCommand` - `:relink`
and `:e <media>` are ordinary undo-tree entries - so the single-write-path
rule survives contact with I/O.

Amendments made during implementation:
- `:relink` had no command to run, so Phase 2 gained `EditCommand::Relink`
  and Phase 1 gained `Timeline::set_media_source`. The offline flag is decided
  by the CLI, which is the only layer that may ask whether a file exists, and
  passed *in* to the command; `davimci-core` never stats a path. The inverse
  restores both the old path and the old offline flag, so a mistaken relink
  is one `u` away. spec 12 now documents both argument forms.
- Spec 12 called registers and marks "global" but the model stores both on a
  `Timeline`. `Workspace` implements global by syncing on every buffer
  switch, and `Session::set_register` joins `set_mark` as a non-command
  escape hatch, on the same reasoning: a register is bookkeeping, not
  timeline content, and vim does not put either in the undo log. A mark's
  focused track is dropped when it crosses into a timeline that has no such
  track; spec 12 says so now.
- Dirty state is `history().current() != saved_at`, not a flag, so undoing
  back to the saved state is clean again. This fell out of the undo tree and
  is worth more than a boolean: it makes `:q` refuse exactly when the file
  and the timeline actually differ.
- Autosave stores the log, not the state, and syncs after every edit. Undo
  shortens the log rather than extending it, so the writer appends only when
  the current log is an extension of what is on disk and rewrites otherwise.
  Each line carries the id cursor, because replaying pinned commands mints no
  ids and a recovered session would otherwise reissue ids the crashed one had
  already used.

Undo history is now persisted (format version 2): `ProjectFile` carries the
whole undo tree - every node's command and inverse, its parent, and which
node was current - so reopening a project and pressing `u` steps back past
the save point and `Ctrl-r` still follows the branches that existed then.
Intermediate drift-guard snapshots are deliberately *not* saved: they are a
rebuild-on-demand guard, and keeping them would multiply a project file by
its edit count. A version 1 document has no history and opens the way it
always did, with the saved state as a fresh root.

Still not wired: `:analyze` is listed in spec 12 but is not accepted yet,
and a *recovered autosave* replays into a fresh tree, since the autosave log
is a flat list of commands rather than a tree.

---

## Phase 9a - View State (`davimci-app`)

Status: complete. The crate holds every decision a frontend would otherwise
make for itself: zoom, scroll-follow, ruler ticks, the mode line, what an
`Outcome` says in the status line, and the meaning of a key. A frontend polls
events, reports its size, and draws a `ViewState` - `davimci-app` never sees a
window and does no I/O, so all of it is unit-testable with no display.

Amendments made during implementation:
- A "column" is deliberately unitless: a GUI pixel or a TUI cell, whichever
  the frontend measures in. That is what lets one `Viewport` serve both, and
  it is why `Surface` carries `columns`/`rows` rather than pixels.
- `davimci-keys` reports `:` as an ordinary mode change to `COMMAND`, not as
  `Outcome::EnterCommandMode`, so the app watches `ModeChanged` and hands the
  keyboard over on that. Deciding what the line *means* stays the host's.
- The `:` line itself moved *up* into `davimci-app` after 9c: the buffer, the
  caret, the history and the completions are view state, and a frontend that
  kept its own was a frontend that could show a different line. A frontend
  now forwards `Event::CommandKey` and draws `ViewState::command_line`, and
  the ex vocabulary reaches completion through
  `App::set_command_candidates`, since `davimci-app` must not know what `:wq`
  is.
- Input is drained in batches (`App::drain`), and the two expensive host
  notifications - `timeline_changed`, `playhead_moved` - are issued once per
  batch. A held `h`/`l` repeats faster than a frame decodes; one seek per
  repeat is what made holding a key lag and then freeze (spec 14).
- Thumbnails ride the same seam as waveforms and job progress: the app asks
  for the visible video clips that have no current picture, nearest the
  playhead first, and the host publishes what it decoded. A thumbnail is keyed
  by the clip's in-point, so a trim or a slip invalidates it rather than
  showing the wrong frame.
- Spec 15 is new: the status-line format for every mode, the scroll-follow
  and zoom-anchoring rules, and the zoom keys `zi`/`zo`/`z0` (spec 11).
  Zoom is view state, so `davimci-keys` only reports `Outcome::Zoom` and
  `App::zoom_*` applies it - the same entry point a pointer wheel or a menu
  uses, and nothing zoom-related reaches the undo log.
- `Host` is the seam for the three things the editor core deliberately does
  not own - `:` commands (`davimci-cli`), transport (the backend clock), and
  Lua callbacks (`davimci-lua`) - so `davimci-app` depends on none of them.
- `davimci-headless` was filled in here rather than in 9d: it is a `Frontend`
  that records view dumps, which is what the parity test compares.

---

## Phase 9b - Video Presenter (`davimci-present`)

Status: complete. The windowed host lives in `davimci-gui`'s `egui_shell`.

Amendments made during implementation:
- Composition is software and integral rather than `wgpu`-first. That is what
  makes the parity and pacing tests byte-exact assertions instead of
  tolerance-based image diffs, and it fixes the pixels a future GPU upload
  path must *reproduce* rather than redefine. `winit`/`wgpu` surface creation
  is therefore deferred to the windowed shell (9c), not to a second video
  path.
- The presenter describes overlays, it does not rasterise text: a timecode is
  a string and safe areas are rectangles, drawn by the host's own text stack.
  Rasterising here would give the GUI and the TUI two different-looking
  timecodes for the same frame. spec 15.5 says so now, along with the
  drop/repeat policy and the drop-frame-free timecode format.

Defects found and fixed by running it against real media:

- **Drop-late dropped everything.** A frame reaches the pacer stamped with the
  position the clock has *just* passed, so "older than the clock, therefore
  discard" threw away every frame and the picture froze while audio ran on -
  the reported "laggy playback". Dropping is now a skip *towards* the clock:
  the newest frame that is not in the future is always presented, frames it
  overtakes are counted as dropped, and a frame pulled before it is due is
  held in `Pacer::pending` for the tick it falls due on instead of being shown
  early or lost. Regression tests:
  `a_frame_the_clock_has_already_passed_is_still_shown`,
  `a_frame_ahead_of_the_clock_waits_instead_of_being_shown_early`,
  `the_picture_never_steps_backwards`.
- **A repeated frame was recomposed and re-uploaded at refresh rate.** Holding
  a picture now costs nothing: `Presenter` caches its last composition and
  hands it back on `Pace::Repeated`, `Presentation::pixels` is an `Arc` so
  passing it on is a refcount rather than a multi-megabyte copy, and
  `Presentation::pixels_id` lets the window skip a texture upload for pixels
  the GPU already has.
- Scale selection is one-directional (`auto_scale` never decodes below what is
  drawn), so a small window is cheap without being soft.

Open at the end of this phase: no window is created, and `PresentError::Pull`
has no production caller until a frontend drives playback.

---

## Phase 9c - GUI Frontend (`davimci-gui`) - primary

Status: complete. The decision layer is tested with no display present, and
the `egui` window that rasterises it is under "The shell" below.

Amendments made during implementation:
- The raw key model is davimci's own (`RawKey` + `Modifiers`), not `winit`'s,
  so translation is testable with no window and the same table can serve a
  terminal adapter. Shells fill in a `RawKey`; they may not decide what a key
  means.
- Painting is split from windowing because that is where a rendering
  regression actually lives. The draw-list summary is the snapshot, and the
  golden view states from 9a drive it, so a view-state change fails in
  `davimci-app` and here - the reuse plan.md asked for, without a GPU in the
  test suite.
- Modal behaviour needed defining rather than inventing: spec 15.3/15.4 now
  say that Esc or backspacing over the `:` cancels the line, that Tab
  completes to the longest common prefix, how the picker filters and wraps,
  and that an INSERT-mode edit ending equal to the original text commits
  nothing (so it never enters the undo log).
- A two-way parity test already runs (GUI vs headless: same script, identical
  view dumps); it becomes the three-way test of 9d when the TUI lands.

---

### The shell

Status: complete. `davimci-gui`'s `egui_shell` is the one module in the
project that knows what a colour or a font is; it rasterises the `DrawList`
that `layout::paint` already computed and uploads the RGBA surface that
`davimci-present` already composited. It sits behind a `window` feature, so
with it off the crate is pure and needs no display - which is how the layout,
painting and input tests still run headless.

Amendments made during implementation:
- Printable keys are taken from egui's `Event::Text` (already shifted by the
  platform layout) and named keys and Control chords from `Event::Key`, since
  `Text` is emitted for neither. Whitespace text is dropped so `Space` cannot
  arrive twice - it is a leader (spec 3.2.1) and a double press would fire
  the wrong binding.
- The presenter's surface is kept equal to the video pane, so
  `davimci-present` letterboxes into exactly the rectangle that will be
  drawn and the shell never scales an image twice.

Defect found by looking at the window: a composed frame is sized to the
surface it was composed for, and nothing recomposed it when the pane resized,
so the picture kept its startup size in the corner of the pane.
`Editor::refresh_preview` now recomposes on a size change, with a regression
test (`resizing_the_video_pane_recomposes_the_frame_at_the_new_size`).

Six more defects, all found the same way - by using the window:

- **Holding `h`/`l` lagged and then froze.** Every key repeat seeked and
  decoded, so input outran the decoder. Input is now drained one batch per
  frame (`App::drain`) and the host is told once, so a burst costs one
  picture (spec 14).
- **The `:` line was invisible while it was typed.** The app knew only that
  `COMMAND` was open; the buffer lived in the shell. The line is view state
  now - buffer, caret and completions - and the shell forwards keystrokes
  instead of hoarding them (spec 15.3). Completion candidates come from the
  host's real vocabulary, and the matches are shown on a row above the line.
- **Clip labels were painted under the waveform**, so an analysed lane had
  unreadable clip names. Labels are drawn last (spec 15.2).
- **The ruler said where `h`/`l` would land but not how far**, so a count had
  to be guessed. Every tick carries a relative jump-point number, clip
  boundaries and subdivisions alike, thinned only where two would overlap
  (spec 3.2). Labelling only the boundaries was the first attempt, and it
  was useless: the counts that need reading are the ones between cuts.
- **Playback could not be restarted after it ran to the end.** Reaching the
  end leaves MLT's producer at speed zero, and `mlt_producer_seek` does not
  undo that - so every later play reported "playing" and never advanced.
  `preview_start` resets the speed, with a regression test that fakes the
  post-EOF state (`a_preview_started_after_playback_ran_off_the_end_plays_again`).
  Starting playback *from* the end is now refused with a sentence rather than
  reported as playback (spec 3.2.1).

Also added here: clip thumbnails (spec 15.2), drawn as a filmstrip - a
picture every thumbnail-width, each of the media at that point. Two earlier
attempts were wrong and both were caught by looking at the window: one picture
at the clip's head is not a strip, and the same picture repeated is not a
filmstrip either. The sample points are `view::strip_samples`, anchored to the
clip's start so scrolling slides the strip rather than re-cutting it, and both
the request path and the paint path read them - so nothing is decoded that
would not be drawn and nothing drawn is missing because it was never asked
for. How wide a picture is drawn is the one part a frontend knows and the app
does not, so it rides in on `Surface::thumbnail_columns`.

`Editor` decodes one picture per tick, only while the transport is stopped,
and restores the decoder's position with a bare seek rather than a repaint -
recomposing there would decode the playhead's frame again on every tick a
strip was filling in. The shell caches one GPU texture per (clip, source
frame), so a redraw is not an upload.

Ruler numbers were clipped to their last digit: the layout sized the box to
its glyphs and the shell then padded the text inside it. `TEXT_PADDING` is one
constant now, used by both.

---

### Phase 8b - export

Status: implemented, with one gap named below.

Amendments made during implementation:
- `:cancel` and `:presets` were added to spec 7's export list; neither was
  named there and both are needed to use the feature.
- An export is a background job, so the editor stays live while one runs.
  Progress is polled on the tick, through a new `Host::jobs` seam, since a
  host runs jobs and the app only displays them.
- Progress never reports 100% before the backend says the render finished; a
  status line that reads "100%" for thirty seconds is a lie about a file that
  does not exist yet.

**The multi-audio gap is closed, and M3's export requirement with it.** An
export that keeps audio tracks separate builds its own graph: each audio
track is routed onto its own channel pair by `channelcopy` *swaps* (a swap
leaves silence behind, so no track can leak into another's range), the
tractor's `mix` transitions sum the now-disjoint bus, and the avformat
consumer cuts it back into one stream per pair via `channels.N`. The
previously-ignored slow test now runs, and it asserts content rather than
stream count: each output stream must carry its own tone.

Three findings, each a defect this work exposed:

- The graph planted **no audio mix transitions at all**, so a tractor played
  one track's audio and dropped the rest. That was a preview bug as much as
  an export one.
- Clips carried no stream selection, so three audio tracks off one container
  all decoded stream zero (spec 7 says one track per stream).
  `MediaRef` gained `stream` and `channels`, filled in at import.
- The producers were missing MLT's *audio* normalisers. `loader.ini` lists
  `swresample`/`audiochannels` and `resample`, and the loader always adds
  `audioconvert`; davimci creates services directly, so it plants them
  itself. Without them a frame keeps the source's channel count and sample
  format, which is why a routed export encoded noise as soon as the codec
  asked for anything but float.

Routing supports mono and stereo sources and at most eight streams. Anything
else is decided *before* the render, against the timeline, and reported as
"audio tracks mixed to one stream: <reason>" rather than discovered in the
file afterwards.

---

### Phase 5 leftover - the media picker opener

Status: complete. `i`/`a`/`r` no longer report `NotImplemented`.

Amendments made during implementation:
- `ImportOptions` gained `placement` (insert vs overwrite) and `target`.
  Imported media ripples rather than overwriting, so picking a file never
  destroys work; `target` puts it on the track the playhead is on, since
  landing it on a fresh track would ignore where the user was looking.
- `r` is refused *before* the picker opens when no clip is under the
  playhead: opening a file browser for an edit that cannot land is a worse
  error than the message.
- All three intents are one command, so a single `u` undoes a whole import,
  including the ripple delete that `r` needs.

Defect found on first real use: opening a file whose name contains spaces
failed with a `:e <path>` usage error. The binary stringified an argv path
into a `:` line, and the parser split it on whitespace. Two fixes, since
there were two bugs: argv paths now go straight to `ExCommand::Edit` without
a round trip through the parser, and a single-path command's argument is the
rest of the line (spec 12), because media filenames contain spaces
constantly. Regression tests cover both spellings.

Both remaining seams are now wired:

- **Click to seek.** The shell reports `Event::Click { column, row }` and the
  app decides it means "seek there, and focus that lane"; deciding it in the
  frontend would have been a second editor. It interrupts playback first,
  since the pacer would otherwise drag the playhead back on the next tick.
- **The subtitle modal has an opener.** `i` means two things by context: on a
  text track the engine answers `Outcome::EditText` with the clip's current
  text, and the frontend opens its buffer; anywhere else it still asks for
  media. Committing sends `Event::TextEdited`, which the app applies as
  `EditCommand::SetClipText` - so text editing is one undo step, and an edit
  that ends equal to the original commits nothing (spec 15.4).

---

### Wiring and transport (the glue)

Status: complete. `davimci_cli::Editor` is the only type that holds a
workspace, a `RenderBackend`, a `Presenter` and the transport at once, and it
lives in the binary crate because no frontend may reference MLT (spec 10.1).
It implements `davimci_app::Host`, so the app drives it without knowing any
of that exists. `davimci_cli::Transport` implements `<Space><Space>`, `J`/`K`/
`L` and `<Space>p`.

Amendments made during implementation:
- `Host` gained `tick`, `timeline_changed` and `playhead_moved`. Reporting
  them from one place in `App::apply_outcome` is what stops a host from
  missing an edit by handling only some outcomes; `tick` takes the session
  because playback *moves the playhead*, which is navigation on the same
  footing as a motion and still never an edit.
- `App::replace_session` and `Engine::reset` were needed for `:e`/`:bn`: a
  viewport column, a visual selection and a half-typed sequence all mean
  something only in the timeline they were made in, so they are reset rather
  than carried across. Registers survive, since spec 12 makes them global.
- Session ownership had to be decided rather than duplicated. `App` owns the
  live session; `Workspace` owns the buffers. The live one is pushed in
  before a `:` command and pulled back after
  (`set_current_session`/`current_session`), so `:w` writes what is on screen
  and `:bn` hands back a different timeline.
- Shuttle is varispeed where the backend supports it. `RenderBackend` gained
  `supports_varispeed`/`set_rate`, and `davimci-mlt` implements it with
  `mlt_producer_set_speed`, which is the right place for it: the consumer
  keeps pulling at wall-clock rate and the producer decides which frame that
  is, so the audio clock stays the master. A backend without rate control -
  `MockBackend`, and so most tests - keeps the stepped scrub, and stopping
  always restores 1x so the next `<Space><Space>` cannot inherit a rate.
- `TransportCmd::LoopSelection` is refused with a sentence rather than
  silently ignored: looping needs the visual selection, which lives in the
  key engine and is not on the `Host` seam yet.

Defect found and fixed by running it: `Transport::tick` composed a frame and
`Editor::tick` composed another, so every tick pulled twice and the pacing
counters read roughly double. The presentation is now returned from the tick
that made it, with a regression test (`a_tick_presents_exactly_once`) naming
the bug.

Stepping backwards was the other half of the lag: `frame_at` re-seeked per
step, and a seek re-decodes from the preceding keyframe, so walking back
through a GOP cost a GOP per frame. `davimci-mlt` now keeps a byte-bounded
`FrameCache` of decoded stills, and a request that moves backwards decodes the
run leading up to its target in a single pass so the following steps are hits.
The cache is dropped on any graph change. Tested by
`stepping_backwards_decodes_each_frame_once_and_is_exact` and
`editing_the_timeline_invalidates_cached_stills` against real media, plus unit
tests for the budget and scale-invalidation rules.

---

## Phase 9e - Audio Operations (`davimci-core` + `davimci-mlt`)

Status: implemented, with the selection-scope gap named below.

Amendments made during implementation:
- Mute and solo became `EditCommand::SetTrackFlags`, because "track state,
  not a clip edit" still has to be undoable, repeatable and scriptable, and
  the command layer is the only write path. They are independent flags:
  soloing a muted track leaves it muted.
- `davimci-analysis` finally has a caller. `davimci_cli::Analyser` watches
  the timeline after every edit, queues one job per audio source, and drops
  a track's envelope when its audible signature (gain, fades, in-points)
  changes - which is the cache invalidation the spec asks for, done without
  waiting for the user to type `:analyze`.
- Waveforms reach the screen the way job progress does, through the `Host`
  seam and a tick, since analysis finishes whenever it finishes. Sampling a
  column from an envelope is `davimci-app`'s arithmetic, not a frontend's,
  so the GUI and the TUI cannot disagree about it.
- `:gain`, `:fade` are workspace commands; `:normalize` and `:duck` are
  editor commands, because they need a measurement. Ducking is expressed as
  splits plus gain in one `Sequence`, since gain is one value per clip.

- The selection reached the host seam here. `davimci_core::Selection` (a
  time range plus tracks, resolved to clips against a timeline at the moment
  the command runs) is the model type both sides can name; `Host::command`
  takes one, `App` remembers the selection that was live when `:` was
  pressed (entering COMMAND clears it in the key engine), and `:gain`,
  `:fade`, `:normalize`, `:duck` and `+`/`-` all act on it. A multi-clip
  change is one `Sequence`, so one `u` undoes it.

Still open: `<Space>l` reports that it is not wired up. The selection it needs
now exists on the seam; what is missing is loop support in the transport,
which belongs with the rest of playback rather than here.

---

## Phase 9f - Transitions (`davimci-core` + `davimci-mlt`)

Status: implemented, apart from the Lua-side registry named below.

Amendments made during implementation:
- A transition is **a property of the incoming clip**, not a third object laid
  over two others. Tracks stay non-overlapping - the invariant every motion,
  ripple and projection rests on - and the overlap is materialised out of
  handle frames at projection time. Deleting a clip therefore deletes its
  transition for free, and `Timeline::settle` (the chokepoint every mutating
  primitive already ended at) drops any transition an edit has invalidated,
  which is what "resolves rather than orphans" comes down to.
- MLT composites *tracks*, not playlist entries, so the overlap projects as
  `Entry::Transition`: a nested two-track tractor holding the outgoing tail
  and the incoming head with the transition planted across them. The diff
  keys it by the incoming clip but distinctly from that clip's own entry, so
  planting one is an insert plus two resizes rather than a rebuild.
- The name-to-service registry lives in `davimci-mlt` because service names
  are MLT's, not the model's. An unknown name degrades to a dissolve rather
  than failing a render, so a project using a Lua-defined type still opens in
  a build without it.
- `:set transition.duration <frames>` is not a separate command: there is no
  `:set` family yet, and re-running `:transition <name> <frames>` on the same
  cut replaces what is there, which covers it. spec 6.2 says so.

Still open: the Lua side of the registry. `transitions::spec` is the seam it
needs, and adding it means a `RenderBackend` method so `davimci-lua` can
register a type without knowing MLT exists.
---

## Wiring the Lua config into the binary

Status: complete. `davimci-lua` was finished and tested against the spec's own
snippets, but nothing depended on it: no user config had ever been loaded by
the editor. This is the seam that closed that.

Amendments made during implementation:
- `Host::plugin` no longer returns a status string. A plugin's requests split
  in two: the ones only the host can answer (an export, an import, a
  registered motion, a re-analysis) run in `davimci-cli`, and the ones that
  are *edits* come back as `PluginEffects` - a list of `davimci_keys::Action`
  - for the app to run through the key engine. That is what keeps spec 9.9
  literally true: a plugin edit takes the same write path a keystroke does,
  because it *is* the same path. Undo, `.`-repeat and macros needed no
  special case.
- Requests queued outside a callback are drained on the tick, through a new
  `Host::plugin_tick`. An event handler that asks for an edit therefore edits
  on the following tick, never inside the notification that an edit happened;
  re-entering the command layer from within `timeline_changed` was the
  alternative and it is not one.
- The v1 event list is derived rather than announced. Insertions and
  deletions come from diffing the clip set the editor last saw, so undo, a
  macro replay and a plugin edit all report the same thing; splits are read
  off `Session::last_edit`, because a diff cannot tell a split from an
  insertion. `ModeChanged` needed a new `Host` hook - the app owns the mode
  FSM, so it was the only layer that could report it - and `Mode::name`
  reads the same table `map()` parses, the other way round, so the two
  cannot drift.
- Lua presets are translated into `davimci-backend`'s registry at startup
  rather than consulted lazily, so `:presets` and Tab completion list what
  the config defined, and a pairing `davimci-lua` accepts but the backend
  cannot build is a load-time notice rather than a render-time surprise.
- The project-local trust prompt asks on the terminal and refuses when there
  is nobody to ask. The plan wanted the app's modal path; there is no modal
  path in `davimci-app` yet, and inventing one for a question asked once at
  startup would have put a frontend concern in the core.
- Nothing about the editor is conditional on Lua: a build always has a
  `Plugins`, and a runtime that cannot even start costs the user their
  plugins rather than their editor.

Still open: a text object registered from Lua is loaded and listed, but the
key grammar has no way to name one - `TextObject` is a closed enum. That is
grammar work rather than wiring, so it moved to the remaining-key-grammar
item in `plan.md`.

---

## The `:set` family (`davimci-cli`)

One command over a typed registry, not four special cases: `setting.rs` parses
and range-checks a property name and value with no session, no filesystem and
no backend, so every setter is proved by a table. Execution then splits by
what a property *is*: clip properties and the re-conform are ordinary
`EditCommand`s the workspace runs, and `preview` is a view setting the editor
intercepts exactly as it intercepts an export, because the preview is the only
thing the workspace does not own.

`spec.md` gained section 12.1, which the spec had reached for from four places
(8, 6.2, 7.1, 15.5) without ever defining. Two decisions worth recording:
`:set clip.gain` and `:set clip.fade_*` are the same commands `:gain` and
`:fade` build rather than a parallel implementation, and `:set transition.*`
changes the transition that is there and fails when there is none - creating
one stays `:transition`, so the two verbs do not overlap.

## Transport loop (`<Space>l`)

Loop state lives on the `Transport`, next to the playhead it governs, so it
never touches the undo log. A wrap restarts the preview at the loop's start
rather than seeking inside a running consumer; the still cache is in the
backend, so the second pass costs no decode the first one already paid.

The seam moved rather than grew: `Host::transport` now carries the selection,
because `<Space>l` is the one transport action whose meaning depends on it,
and a new `Host::selection_changed` reports a selection that went away. Those
two, plus a check in `playhead_moved`, are the whole of "a loop follows the
selection it was set on and survives a seek inside its range".

## Remaining key grammar

Three gaps, three different answers:

- `<`/`>` build the same `EditCommand::Trim` that `t` + motion builds; only
  the landing position is decided by the jump-point set rather than a typed
  motion. No jump point in that direction is a user error, so the timeline is
  untouched at the end of the timeline.
- `it`/`at` in a VISUAL mode are a new `Action::NarrowSelection`: an object
  typed with a selection live changes its *scope* and never its range, which
  is what spec 6 asked for and what the objects carry a scope for.
- A config-registered text object is `TextObject::Named { name, around }`.
  The grammar carries the name and nothing else: resolving it needs Lua, so
  the engine hands the whole verb back as `Outcome::ResolveObject` and the
  host re-issues it with `Action::with_range`. The verb then runs through the
  ordinary command path, so a plugin object undoes and repeats like any other
  edit. Config wins over defaults, as it does for keymaps, so registering `c`
  replaces the built-in `ic`/`ac` rather than being unreachable behind it.

## `:analyze`

Wiring, not machinery: `Analyser::reanalyse` already dropped every envelope
and re-queued the work for the Lua `analyze` request, so the command is the
same call with a sentence on the end. It lives in the editor for the reason
`:normalize` does - the workspace has no analysis.

## Undo history across crash recovery

The autosave stopped being a flat command log. Each record now carries the
tree edge its command was applied at, and a move of the current position is
its own record, so recovery rebuilds a `SavedHistory` and hands it to
`UndoTree::restore` - the same path a saved project takes. Undo no longer
rewrites the file: a tree only grows, so the log is append-only unless the
root itself changes.

Tolerance is deliberate and narrow. Only the *last* record may be incomplete,
and only when the file does not end in a newline - that is what a crash
mid-write leaves. A `Current` record pointing past the states that survived
lands on the last one rather than refusing the whole recovery.

## Subtitle export selection

`SubtitleMode` now reaches the renderer as one bit, `RenderSettings::
burn_subtitles`, because that is the only decision the backend has to make:
whether the text tracks are in the graph. Everything else is the CLI's, which
is the layer allowed to do I/O - the sidecar is written before the render
starts, so a cancelled export still leaves the SRT, and `embedded` muxes the
same file into the output with ffmpeg once the render lands.

Departure from the plan: a failed mux does not fail the export. The render is
on disk and playable; only the subtitles are missing, which is a recoverable
error and is reported as one.

Found on the way: projecting a transition on a clip with no source handles
underflowed. Generated clips have no handles, so this was reachable from a
test and would have been reachable from an offline clip; the projection now
saturates.

## Lua-registered transition types

`RenderBackend::register_transition` takes a `TransitionDef` - a name, a
service and properties - so `davimci-lua` can define a type without learning
what MLT is, and a backend with no registry refuses instead of pretending.
The MLT side keeps registered types in a process-global map that `spec` reads
before the built-ins, because `spec` is a pure lookup called from the
projection, which has no backend to ask.

`transitions::spec` stayed the only seam, so the degradation rule needed no
change: a name this build does not know is still a dissolve, and a project
made with a plugin opens without it.

---

## Integration and hardening

### The scripted-session format

One artefact does both jobs the plan asked for. `davimci-headless::script`
parses a file of directives - `keys`, `cmd`, `tick`, `dump`, `expect` - and
runs it against a real `App` over any `Host`, so the same `.dvs` file is a
test in `crates/davimci-headless/tests/sessions/` and a reproduction that
`davimci --script` replays through the whole editor, MLT included.

Two decisions the format turns on. An unknown directive is a parse error
rather than a skipped line, because a typo that silently asserts nothing is
worse than no test. And a run collects every failure instead of stopping at
the first, with the script's line number on each, so one broken assertion does
not hide the four after it.

Assertions only read state. That is what lets a script mean the same thing
whether or not the assertions are checked, which is what makes it usable as a
debugging tool rather than only as a test.

### What the soak fuzz found

Random key sequences against a fixture project, asserting no panic, invariants
after every chunk, and an exact return to the start when the log is undone.
The generator is a seeded xorshift rather than a dependency so a failure
reproduces from its seed, and the run asserts it made edits at all - a fuzz
that types only motions proves nothing.

It found two bugs in VISUAL mode, both of them the same shape: a verb that
forgot the selection.

- `y` on a selection yanked and left VISUAL live, while `d` and `gd` ended it.
  The next motion then extended a selection the user believed was gone.
- Motions in VISUAL resolved from the playhead, which stays at the anchor, so
  the *moving* end was not the one that moved: `w` then `<Left>` collapsed the
  selection to a single frame instead of shrinking it by one. `MotionCtx`
  gained an `origin`, and the engine passes the selection's active end.

Both ship with regression tests naming what they were.

The fuzz also surfaced a gap rather than a bug: spec 6's "visual-line snaps to
clip boundaries" is not implemented anywhere. It is in `todo.md` rather than
fixed here, because it is a feature with a spec sentence and not a hardening
defect.

### Performance

Criterion measures; separate `#[ignore]`d tests assert. Splitting the two
matters: a benchmark that fails a threshold is noise on a busy machine, and a
threshold that lives inside a benchmark never runs in CI. `just perf` runs the
budgets in release, because a debug build proves nothing.

Measured on the development machine: ripple delete on 500 clips 16 us, 500
undos 0.9 ms, a 500-clip 200-edit project load 2.9 ms, jump-point rebuild
7-10 us, a jump-point step 9 ns. Predicate lookup is 96 ns over a minute of
analysis and 115 ns over an hour, which is the shape spec 14 asks for; the
test asserts the *ratio*, not the time, because "never scans" is a structural
claim and a ratio is what a scan would break.

The 1080p60 budget subtracts the source's own cost rather than budgeting it.
The mock synthesises a 1080p buffer per frame, as a decoder would, and that
cost is not the presenter's; what is left - pull, compose, letterbox - has to
fit in a quarter of the frame, leaving room for the decoder and the GUI.

Found while writing the benchmarks: splitting at a clip boundary is a
rejection, so the first draft of the undo benchmark measured 500 refused
commands. Both the benchmark and the budget now pick interior frames, and the
budgets `unwrap` every command so a silent rejection fails rather than
flattering the number.

### The generated keymap

`docs/keymap.md` is generated from `default_bindings` and checked against it
by a test, so a binding cannot change without the document changing with it.
The description of every action is an exhaustive match, which means a new
`LeafAction` stops compiling until it has a sentence - the drift the plan was
worried about is a compile error rather than a review comment.

### The full-workflow test

Spec 1's five steps end to end through real MLT: import a multi-track MKV,
split and ripple, mute and trim an audio track, add an overlay, add a
subtitle, export, and assert with `ffprobe` that the stream layout survived
and the output is exactly as long as the timeline.

Two things it taught. `Timeline::duration` is the longest track, so a cut on
the video track alone does not shorten it - the assertion had to name the
track it cut. And the import is itself an undoable command, so "undo
everything" lands on an empty timeline; the test records the tree node the
import produced and returns to that instead.

Found on the way: `davimci-mlt`'s render smoke test had not been updated for
`RenderSettings::burn_subtitles` and no longer compiled under `slow-tests`.
The fast suite could not see it, which is the argument for building the slow
suite in CI even when it is not run.
