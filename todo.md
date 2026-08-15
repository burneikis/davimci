# TODO

- Manual Testing
- Bugfixing
- Forwards preview on a machine with no audio device at all shows no picture:
  the picture is lifted from consumer events, and the consumers that run
  without a device either fire none (`null`) or stop early. The backwards pass
  already decodes its own picture in a worker; forwards needs the same.
  Reproduce with `./scripts/no-audio.sh`.

- Preview playback at 2160p60 shows ~34 of 60 frames while the clock keeps
  96% of real time, and neither decode threads nor `real_time` move that
  number (`just bench-preview`). What is left is per-frame work on the
  consumer thread: `on_frame_show` allocates and copies a fresh 33 MB RGBA
  buffer per frame. A pool of buffers instead of a `Vec` per frame is the
  fix. Rerun the bench now that a reduced scale really reduces the work.

# Plugins Todo

- advanced audio: EQ, compression, noise reduction beyond `:duck`
- video effects beyond transform and transitions
- ML-based scene detection hook
- beat detection as a jump-point source
- silence cutting as an operator over the `silence` plugin's motions
- chroma keying / green-screening
