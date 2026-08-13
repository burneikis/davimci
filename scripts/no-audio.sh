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

exec unshare -rm --map-root-user bash -euc '
  runtime=$1
  shift
  [ -d /dev/snd ] && mount -t tmpfs empty /dev/snd
  [ -d "$runtime" ] && mount -t tmpfs empty "$runtime"
  exec env -u PULSE_SERVER -u PIPEWIRE_RUNTIME_DIR "$@"
' -- "$runtime" "$@"
