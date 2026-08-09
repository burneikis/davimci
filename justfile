# davimci dev tasks. Run `just` for the list.

default:
    @just --list

# Verify all build prerequisites are installed.
check-env:
    ./scripts/check-env.sh

# Generate test media with ffmpeg (never committed).
fixtures:
    ./scripts/gen-fixtures.sh

# Fast suite: no decode/encode. Must stay quick.
test:
    cargo test --workspace

# Real render/export tests.
test-slow: fixtures
    cargo test --workspace --features slow-tests -- --include-ignored

# The planar upload path, which needs a GPU. Lavapipe counts; no adapter at
# all skips rather than fails.
test-gpu:
    cargo test -p davimci-present --features gpu --test gpu

# Everything, including sanitizers.
test-all: test test-slow test-gpu sanitize

# Leak/UB detection, aimed at the MLT refcount wrapper.
# Suppressions filter MLT's own one-time module-init state, not davimci's -
# see crates/davimci-mlt/lsan-suppressions.txt for what and why.
sanitize:
    LSAN_OPTIONS="suppressions=$(pwd)/crates/davimci-mlt/lsan-suppressions.txt" \
    RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p davimci-mlt \
        --target x86_64-unknown-linux-gnu

# Regenerate the generated documentation (docs/keymap.md).
docs:
    DAVIMCI_UPDATE_DOCS=1 cargo test -p davimci-keys --test keymap_docs

# Timing budgets and scaling checks, in release. Ignored by the fast suite.
perf:
    cargo test --workspace --release -- --ignored

# Run a scripted-session file (keys plus assertions) through the editor.
script FILE:
    cargo run -p davimci-cli --no-default-features -- --script {{FILE}}

# A long editing session under ASan: the soak fuzz, sanitized.
soak-asan:
    RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p davimci-headless \
        --target x86_64-unknown-linux-gnu --test soak

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --check

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
