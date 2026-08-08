# Human
- strip down to core features, everything else should be built as a plugin, include default plugins for common features (first decide what features are core, and what can be plugins)

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
- `an_exported_file_has_the_duration_of_the_timeline` fails on a 5s timeline:
  the file comes out 5.088s. Pre-dates the hardening pass; the export writes
  a few frames more than the timeline holds

- the first backward step of every run decodes `backstep_run` frames inline in
  `show_playhead`, so held-down `h` hitches once every `backstep_run` frames;
  the run wants prefetching onto a worker like `Scrub`, which needs a second
  graph while the transport is idle

# AI - deferred (not v1)
- GPU/hardware acceleration (decode, planar upload, zero-copy import, hardware
  encode) is planned in `docs/gpu_plan.md`
- a custom subtitle layout engine in place of MLT's text producers
- plugin distribution and package management

# Plugins
- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- beat detection as a jump-point source
