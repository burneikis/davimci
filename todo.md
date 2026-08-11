# TODO

## Core stripdown

The boundary is `docs/plugins.md`; the budgets are `just weigh`. Done:
transitions (model core, no type core, `gx`/`dax` with the catalogue), and
the rule that a build a user runs can always show them the timeline.

Remaining, each a move out of core rather than a deletion:

- analysis: pull-driven and feature-gated. Probe and conform stay core;
  loudness, scene detection, waveform and thumbnail caches become
  `davimci.analysis.*` requests nothing runs unasked.
- proxies: `cli/src/proxy.rs` and `:set proxy` to a bundled plugin. A proxy
  policy is a workflow opinion, and the docs already file it as one.
- `:duck` and `cli/src/audio.rs` beyond gain and mute/solo, to the audio
  plugin the EQ/compression work will need anyway.
- export presets: the export command is core, the catalogue is registration
  data, same argument the transition catalogue lost.
- `davimci-app`'s browse, picker, thumbnail and subtitle views onto the panel
  API - the test of whether that API is sufficient.
- reserve the plugin key namespaces in `docs/keymap.md`: `[`/`]` pairs,
  `<Space>` as leader, `g`/`z` plus uppercase. Core takes no new binding
  under them.

# Plugins
- check transitions work, make them work in preview
- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- beat detection as a jump-point source
- silence cutting as an operator over the `silence` plugin's motions
- chroma keying / green-screening

# Final
- Manual Testing
- Bugfixing
- Docs improvements
- Default Binds Audit
- Behaviour of core features
- Cleanup bundled plugins
- align TUI behaviour
