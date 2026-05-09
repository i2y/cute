#!/usr/bin/env bash
# examples/todomv/run.sh - one-command build + launch.
#
# `cute build app.cute` parses + type-checks app.cute, generates C++,
# stamps a temporary cmake project, builds against Qt 6, and produces
# `./app` (or `<output>` if -o given). main.qml is picked up
# automatically because the qml_app intrinsic in app.cute names a
# qrc:/main.qml resource.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

echo ">> cargo build cute"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet

echo ">> cute build app.cute"
cd "$HERE"
"$REPO_ROOT/target/debug/cute" build app.cute

echo ">> launching ./app  (close the window to exit)"
"$HERE/app"
