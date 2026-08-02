# vimci dev tasks. Run `just` for the list.

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

# Everything, including sanitizers.
test-all: test test-slow sanitize

# Leak/UB detection, aimed at the MLT refcount wrapper (plan.md Phase 6).
sanitize:
    RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p vimci-mlt \
        --target x86_64-unknown-linux-gnu

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --check

fix:
    cargo clippy --workspace --all-targets --fix --allow-dirty
    cargo fmt

run *ARGS:
    cargo run -p vimci-cli -- {{ARGS}}

# Optional terminal frontend (plan.md Phase 9d).
run-tui *ARGS:
    cargo run -p vimci-cli --features tui -- {{ARGS}}

bench:
    cargo bench --workspace
