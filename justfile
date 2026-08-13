# davimci dev tasks. Run `just` for the list.

default:
    @just --list

# Verify all build prerequisites are installed.
check-env:
    ./scripts/check-env.sh

# Build in release and install the binary under PREFIX (default ~/.local).
install PREFIX=(env_var_or_default("HOME", "") / ".local"):
    ./scripts/install.sh --from-source --prefix {{PREFIX}}

uninstall PREFIX=(env_var_or_default("HOME", "") / ".local"):
    rm -f {{PREFIX}}/bin/davimci

# Generate test media with ffmpeg (never committed).
fixtures:
    ./scripts/gen-fixtures.sh

# Every suite runs under a deadline: a deadlocked test binary otherwise sits
# there printing nothing, and a run that never finished must never be read as
# a run that passed. See scripts/timed.sh.

# Fast suite: no decode/encode. Must stay quick.
test:
    ./scripts/timed.sh 600 "fast suite" cargo test --workspace

# Real render/export tests.
test-slow: fixtures
    ./scripts/timed.sh 1800 "slow suite" cargo test --workspace --features slow-tests -- --include-ignored

# Preview pacing as a CI runner sees it: no sound card, so nothing but wall
# time keeps the clock. A developer box hides these by having audio output.
test-no-audio: fixtures
    ./scripts/no-audio.sh ./scripts/timed.sh 600 "no-audio suite" \
        cargo test -p davimci-mlt --features slow-tests --test media shuttle -- --include-ignored

# The planar upload path, which needs a GPU. Lavapipe counts; no adapter at
# all skips rather than fails.
test-gpu:
    ./scripts/timed.sh 300 "gpu suite" cargo test -p davimci-present --features gpu,slow-tests --test gpu

# Everything, including sanitizers.
test-all: test test-slow test-gpu sanitize

# Leak/UB detection, aimed at the MLT refcount wrapper.
# Suppressions filter MLT's own one-time module-init state, not davimci's -
# see crates/davimci-mlt/lsan-suppressions.txt for what and why.
sanitize:
    LSAN_OPTIONS="suppressions=$(pwd)/crates/davimci-mlt/lsan-suppressions.txt" \
    RUSTFLAGS="-Zsanitizer=address" ./scripts/timed.sh 1800 "sanitizer suite" \
        cargo +nightly test -p davimci-mlt --target x86_64-unknown-linux-gnu

# Regenerate the generated documentation (docs/keymap.md).
docs:
    DAVIMCI_UPDATE_DOCS=1 cargo test -p davimci-keys --test keymap_docs

# Timing budgets and scaling checks, in release. Ignored by the fast suite.
perf:
    cargo test --workspace --release -- --ignored

# Run a scripted-session file (keys plus assertions) through the editor.
script FILE:
    cargo run -p davimci-cli --no-default-features --features driver-only -- --script {{FILE}}

# A long editing session under ASan: the soak fuzz, sanitized.
soak-asan:
    RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p davimci-headless \
        --target x86_64-unknown-linux-gnu --test soak

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --check

# What each build profile links. Lightness is a budget, not a habit: the
# window is the only heavy thing here, and it has to stay the only one.
weigh:
    ./scripts/weigh.sh

fix:
    cargo clippy --workspace --all-targets --fix --allow-dirty
    cargo fmt

run *ARGS:
    cargo run -p davimci-cli -- {{ARGS}}

# Optional terminal frontend.
run-tui *ARGS:
    cargo run -p davimci-cli --features tui -- {{ARGS}}

bench:
    cargo bench --workspace
