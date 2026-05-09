#!/usr/bin/env bash
# examples/counter/run.sh - Cute UI counter, no .qml on disk.
#
# `cute build counter.cute` lowers the `view Main { ... }` declaration
# to a generated Main.qml inside the build cache, embeds it via qrc,
# and produces ./counter. No hand-written QML file.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

echo ">> cargo build cute"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet

echo ">> cute build counter.cute"
cd "$HERE"
"$REPO_ROOT/target/debug/cute" build counter.cute

echo ">> launching ./counter  (close the window to exit)"
"$HERE/counter"
