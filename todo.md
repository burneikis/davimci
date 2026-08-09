# Human
- move the analysis-driven editing policies (silence cutting, scene detection
  hooks) out of the host and into bundled plugins, per `docs/plugins.md`

# AI - deferred (not v1)
- plugin distribution and package management
- a custom subtitle layout engine in place of MLT's text producers

# Plugins
- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- beat detection as a jump-point source
