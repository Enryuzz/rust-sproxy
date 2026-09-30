#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="release"
BUILD_ARGS=()
DEFAULT_SERVER_ARGS=(--listen 0.0.0.0:8181 --allow-public-listen --type auto)

usage() {
    cat <<'EOF'
Build and run rust-sproxy.

Usage: scripts/run.sh [build options] [--] [server options]

Build options:
  --release       build an optimized release binary (default)
  --debug         build a debug binary
  --test          run tests before starting the proxy
  --clean         clean build artifacts before building
  -h, --help      show this script's help

Server options are forwarded unchanged. Use -- to end build options.
If no server options are provided, the script uses:
  --listen 0.0.0.0:8181 --allow-public-listen --type auto
Relative config paths are resolved from your current working directory.

Examples:
  scripts/run.sh
  scripts/run.sh -- --type socks5
  scripts/run.sh --debug --test -- --config sproxy.toml
  scripts/run.sh -- --help
EOF
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
        --test|--clean)
            BUILD_ARGS+=("$1")
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            break
            ;;
        *)
            break
            ;;
    esac
done

# Bash 3.2 on macOS treats expansion of an empty array as an unset
# parameter under `set -u`. Expand BUILD_ARGS only when it has values.
if ((${#BUILD_ARGS[@]} > 0)); then
    bash "$ROOT/scripts/build.sh" "--$PROFILE" "${BUILD_ARGS[@]}"
else
    bash "$ROOT/scripts/build.sh" "--$PROFILE"
fi
if (($# == 0)); then
    set -- "${DEFAULT_SERVER_ARGS[@]}"
fi
exec "$ROOT/target/$PROFILE/rust-sproxy" "$@"
