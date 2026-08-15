#!/usr/bin/env bash
# Measure the preview parallelism policy against MLT's own defaults.
#
# MLT decodes with one thread (`avformat` producer `threads`, default 1) and
# processes a graph with one thread (`real_time`, value 1). davimci asks for
# more; this runs the same two slow tests both ways and prints the numbers so
# the choice is a measurement rather than an opinion.
#
# Usage: scripts/bench-preview.sh          (needs `just fixtures` first)
set -euo pipefail

cd "$(dirname "$0")/.."

if [ ! -f target/fixtures/counter_1080p60.mkv ]; then
    echo "No 1080p fixture: run 'just fixtures' first." >&2
    exit 2
fi

# 2160p is the size the policy exists for, and it is a bench input rather than
# a test fixture: nothing asserts against it, so it is made here and not in
# gen-fixtures.sh.
if [ ! -f target/fixtures/counter_2160p60.mkv ]; then
    echo "Generating the 2160p60 bench clip (once)."
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "testsrc2=size=3840x2160:rate=60:duration=10" \
        -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
        target/fixtures/counter_2160p60.mkv
fi

run() { # label, extra env...
    local label=$1
    shift
    echo
    echo "=== $label ==="
    env "$@" ./scripts/timed.sh 900 "bench-preview ($label)" \
        cargo test -p davimci-mlt --features slow-tests --test media \
        -- --include-ignored --test-threads=1 --nocapture \
        decode_cost_per_frame_is_reported_for_both_paths \
        preview_throughput_is_reported \
        2>/dev/null | grep -E 'decode |planar |preview [0-9]'
}

echo "Building once so the build does not land inside a timing."
cargo build -p davimci-mlt --features slow-tests --tests >/dev/null

run "MLT defaults (1 decode thread, real_time 1)" \
    DAVIMCI_DECODE_THREADS=1 DAVIMCI_REAL_TIME=1
run "davimci policy" DAVIMCI_BENCH=1

echo
echo "Lower ms/frame and higher frames/s are better; the percentage is how"
echo "much of real time the preview clock kept."
