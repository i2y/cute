#!/usr/bin/env bash
# examples/todomv_cli/run.sh - one-command build + run for the CLI demo.
#
# `cute build app.cute` detects the `cli_app { ... }` intrinsic and
# emits a QCoreApplication-only main.cpp (no QML / Gui), then drives an
# internal cmake project to link against Qt6::Core. Output: ./app.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

echo ">> cargo build cute"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet

echo ">> cute build app.cute"
cd "$HERE"
"$REPO_ROOT/target/debug/cute" build app.cute

echo ">> running ./app"
"$HERE/app"
