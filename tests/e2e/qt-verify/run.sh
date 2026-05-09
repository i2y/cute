#!/usr/bin/env bash
# tests/e2e/qt-verify/run.sh
#
# Compile cutec, then build the Qt 6 e2e harness against cutec-generated
# C++ (no moc invocation), then run the resulting binary. Exits 0 on success.
#
# Requires: Qt 6 (homebrew default at /opt/homebrew/opt/qt), cmake, cargo.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
BUILD_DIR="${BUILD_DIR:-$(mktemp -d -t cute-qt-verify-XXXXXX)}"

echo ">> cargo build cute-cli"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet

echo ">> cmake configure ($BUILD_DIR)"
cmake -S "$REPO_ROOT/tests/e2e/qt-verify" -B "$BUILD_DIR" \
    -DCUTEC="$REPO_ROOT/target/debug/cute" \
    > "$BUILD_DIR/cmake.log" 2>&1 || { cat "$BUILD_DIR/cmake.log"; exit 1; }

echo ">> cmake build"
cmake --build "$BUILD_DIR" > "$BUILD_DIR/build.log" 2>&1 || { cat "$BUILD_DIR/build.log"; exit 1; }

echo ">> run cute_qt_verify"
"$BUILD_DIR/cute_qt_verify"
