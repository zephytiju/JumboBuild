#!/usr/bin/env sh

set -eu

SCRIPT_DIR=$(dirname "$0")
SCRIPT_DIR=$(CDPATH= cd "$SCRIPT_DIR" && pwd)
REPO_ROOT=$(CDPATH= cd "$SCRIPT_DIR/../../../.." && pwd)

if [ -f "$REPO_ROOT/Cargo.toml" ] && grep -q 'name = "jumbo-build"' "$REPO_ROOT/Cargo.toml"; then
    run_jumbo() {
        cargo run --quiet --locked --manifest-path "$REPO_ROOT/Cargo.toml" -- "$@"
    }
elif command -v jumbo >/dev/null 2>&1; then
    run_jumbo() {
        jumbo "$@"
    }
else
    echo "Jumbo CLI not found: install 'jumbo' or run this skill from a JumboBuild source checkout." >&2
    exit 127
fi

run_jumbo --version
echo

if [ "$#" -eq 0 ]; then
    run_jumbo --help
else
    run_jumbo "$@" --help
fi
