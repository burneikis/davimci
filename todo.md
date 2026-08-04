# Human
- add relative line numbers for tui mode (make relative line numbers/normal numbers/no numbers, settable with a flag) (this is for the jump points in timeline)
- allow plugins that render smth / a window. e.g. something like which-key
- backwards shuttle audio?
- different speed shuttle audio
- backwards shuttle lag, we can skip a lot of frames when going backwards shuttling speed
- figure out how davinci resolve can cleanly step framewise backwards
- figure out whats causing playback to break after doing a few seeks, or shuttles to the end or fast backwards etc
- refactor all for clean code
- plugin support for gui/tui sub-windows
- use clippy on pedantic / nursery
- command to center the playhead, moving the timeline scroll to center the playhead in the timeline view (user may want to have this on all the time, or bind it, or hook pause/playing states/transitions)
- strip down to core features, everything else should be built as a plugin, include default plugins for common features (first decide what features are core, and what can be plugins)
- clip grouping, imported clip video/audio should be grouped together until un-grouped, meaning splitting the clip for example should split both the video and audio, and moving one should move the other, etc

# AI
- optional detached preview window for the TUI (`--preview-window`): a bare,
  undecorated, non-focusable `winit` window in `davimci-cli` showing the same
  texture the GUI uploads, off by default, terminal keeps keyboard focus, and
  closing it falls back to inline preview instead of ending the session
- ask for `.davimci.lua` trust in the window rather than on the terminal, once the app has a modal path
- `:set proxy on|off` is not in the `:set` registry; proxies have no runtime
  switch yet
- a burned-in subtitle is not asserted by a pixel diff: MLT's text producers
  need a display, so the slow test only proves the text stayed out of the
  streams and the sidecar
- `V` (visual-line) should snap the selection to whole clips; nothing
  implements the snap, so `V` behaves like `v` until a motion extends it
- `an_exported_file_has_the_duration_of_the_timeline` fails on a 5s timeline:
  the file comes out 5.088s. Pre-dates the hardening pass; the export writes
  a few frames more than the timeline holds

# AI - deferred (not v1)
- zero-copy hardware-decode surface import into the `wgpu` presenter
- a custom subtitle layout engine in place of MLT's text producers
- beat detection as a jump-point source
- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- plugin distribution and package management
