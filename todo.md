# TODO

- Manual Testing
- Bugfixing
- Forwards preview on a machine with no audio device at all shows no picture:
  the picture is lifted from consumer events, and the consumers that run
  without a device either fire none (`null`) or stop early. The backwards pass
  already decodes its own picture in a worker; forwards needs the same.
  Reproduce with `./scripts/no-audio.sh`.

# Plugins Todo

- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- beat detection as a jump-point source
- silence cutting as an operator over the `silence` plugin's motions
- chroma keying / green-screening
