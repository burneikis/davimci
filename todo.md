# Human
- strip down to core features, everything else should be built as a plugin, include default plugins for common features (first decide what features are core, and what can be plugins)

# AI
- `the_whole_spec_one_workflow_survives_a_real_import_and_export` fails: the
  split propagates to the linked audio tracks, so A1-A3 come out with two
  clips where the test expects one. Either the expectation or the grouping is
  wrong - decide which before changing either
- a proxy is generated per import and swapped in by a relink, so it costs an
  undo entry the user did not ask for; it wants a write path that is a
  command but not a history step

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
