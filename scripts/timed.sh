#!/usr/bin/env bash
# Run a test command under a deadline.
#
# A hung test suite is the worst failure to sit in front of: libtest keeps
# printing nothing, and the caller cannot tell a slow decode from a deadlock.
# This turns "hangs forever" into a non-zero exit and a sentence saying which
# threads were stuck where.
#
# Usage: scripts/timed.sh <seconds> <label> <command...>
set -euo pipefail

if [ "$#" -lt 3 ]; then
    echo "usage: $0 <seconds> <label> <command...>" >&2
    exit 2
fi

limit=$1
label=$2
shift 2

# --foreground so an interactive Ctrl-C still reaches the command; SIGKILL
# after a grace period because a deadlocked test binary ignores SIGTERM.
set +e
timeout --foreground -k 10s "${limit}s" "$@"
status=$?
set -e

if [ "$status" -eq 124 ] || [ "$status" -eq 137 ]; then
    echo >&2
    echo "TIMEOUT: '${label}' was killed after ${limit}s - treat this as a hang, not a slow run." >&2
    echo "The tests did not pass and did not fail; nothing here can be reported as green." >&2
    echo >&2
    echo "Most likely causes, in order:" >&2
    echo "  1. A deadlock in a test that shares a process-wide resource (GPU device, MLT factory)." >&2
    echo "  2. A test waiting on a job or frame that never arrives." >&2
    echo >&2
    echo "To see where it was stuck, re-run it and inspect the threads:" >&2
    echo "  <the command> &" >&2
    echo "  for t in /proc/\$(pgrep -x <test-binary>)/task/*; do" >&2
    echo "      echo \"\$(cat \$t/comm): \$(cat \$t/wchan)\"" >&2
    echo "  done" >&2
    echo "Threads parked in futex_wait or rt_mutex_schedule mean a lock, not slow work." >&2
    echo "Re-running with --test-threads=1 tells a deadlock from a genuinely slow suite." >&2
    exit 124
fi

exit "$status"
