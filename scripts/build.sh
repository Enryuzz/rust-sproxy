#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="release"
RUN_TESTS=0
CLEAN=0

usage() {
    cat <<'EOF'
Build rust-sproxy.

Usage: scripts/build.sh [options]

Options:
  --release       build an optimized release binary (default)
  --debug         build an unoptimized debug binary
  --test          run the test suite after building
  --clean         remove Cargo build artifacts before building
  -h, --help      show this help

The binary is written to target/release/rust-sproxy or target/debug/rust-sproxy.
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

while (($#)); do
    case "$1" in
        --release)
            PROFILE="release"
            shift
            ;;
        --debug)
            PROFILE="debug"
            shift
            ;;
        --test)
            RUN_TESTS=1
            shift
            ;;
        --clean)
            CLEAN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

command -v cargo >/dev/null || die "cargo is required but was not found"

if ((CLEAN)); then
    printf 'Cleaning build artifacts...\n'
    cargo clean --manifest-path "$ROOT/Cargo.toml"
fi

printf 'Building %s binary...\n' "$PROFILE"
if [[ "$PROFILE" == "release" ]]; then
    cargo build --locked --release --manifest-path "$ROOT/Cargo.toml"
    BINARY="$ROOT/target/release/rust-sproxy"
else
    cargo build --locked --manifest-path "$ROOT/Cargo.toml"
    BINARY="$ROOT/target/debug/rust-sproxy"
fi

if ((RUN_TESTS)); then
    printf 'Running tests...\n'
    cargo test --locked --all-targets --manifest-path "$ROOT/Cargo.toml"
fi

printf 'Build complete: %s\n' "$BINARY"
