#!/usr/bin/env bash
# Run a command on a machine that has no audio output, the way a CI runner
# does. A developer box has a sound card, so the preview clock is driven by
# real audio output there and every pacing bug hides; the runner has none.
#
#     ./scripts/no-audio.sh cargo test -p davimci-mlt --features slow-tests
#
# The sandbox is a rootless mount namespace with no /dev/snd and no
# PulseAudio or PipeWire socket. It needs unprivileged user namespaces and
# nothing else.
#
# It is stricter than a GitHub runner in one way: a developer box has JACK
# installed, so SDL falls back to a JACK client it cannot open rather than to
# the silent device a runner ends up with. A preview that reports no audio
# output here is that difference, not a failure.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 2
fi

if ! unshare -rm --map-root-user true 2>/dev/null; then
  echo "no-audio.sh needs unprivileged user namespaces" >&2
  exit 1
fi

runtime=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
case $runtime in
  /run/user/*) ;;
  *)
    echo "refusing to cover XDG_RUNTIME_DIR=$runtime: not a /run/user path" >&2
    exit 1
    ;;
esac

# The mounts below are undone by the namespace ending with the command: there
# is nothing to clean up and nothing to leak. `--propagation private` is what
# makes that true rather than the util-linux default making it true - without
# it, a shared host mount tree would carry the tmpfs back out.
exec unshare -rm --map-root-user --propagation private bash -euc '
  runtime=$1
  shift
  # A machine that already lacks one of these needs no covering for it.
  if [ -d /dev/snd ]; then mount -t tmpfs empty /dev/snd; fi
  if [ -d "$runtime" ]; then mount -t tmpfs empty "$runtime"; fi
  exec env -u PULSE_SERVER -u PIPEWIRE_RUNTIME_DIR "$@"
' -- "$runtime" "$@"
