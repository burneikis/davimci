# Human
- strip down to core features, everything else should be built as a plugin, include default plugins for common features (first decide what features are core, and what can be plugins)
- move which-key to a plugin, make it not enabled by default

# AI - deferred (not v1)
- GPU/hardware acceleration is planned in `docs/gpu_plan.md`; hardware decode
  (`:set decode cpu|auto`) has landed, planar upload, zero-copy import and
  hardware encode have not
- plugin distribution and package management
- a custom subtitle layout engine in place of MLT's text producers

# Plugins
- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- beat detection as a jump-point source
