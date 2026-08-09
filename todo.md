# Human
- strip down to core features, everything else should be built as a plugin, include default plugins for common features (first decide what features are core, and what can be plugins)
- move which-key to a plugin, make it not enabled by default

# AI - deferred (not v1)
- GPU/hardware acceleration: see `docs/gpu_plan.md`. Hardware decode, cheaper
  preview pixels, planar upload for stills and hardware encode have landed;
  planar frames through the playback queue and zero-copy surface import have
  not, the latter blocked by MLT handing out system memory
- plugin distribution and package management
- a custom subtitle layout engine in place of MLT's text producers

# Plugins
- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- beat detection as a jump-point source
